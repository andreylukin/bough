//go:build !windows

package session

import (
	"context"
	"encoding/json"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"maps"
	"os"
	"slices"
	"sort"
	"strings"
	"time"

	"github.com/google/uuid"
	"github.com/unreallabsai/unreal-agent/harness/inbox"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/session"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/unreal/project"
	"github.com/andreylukin/bough/internal/unreal/prompt"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
	"github.com/andreylukin/bough/plugins/loop"
)

// jobWake opens the turn a background notice starts while the agent is
// idle: the loop's text (loop.go jobWake), so both engines read the same.
const jobWake = "[background job] A command you started in the background has finished while you were idle. Deal with it if it needs anything, then reply to the user with what happened.\n\n"

// cancelWait bounds how long a cancel waits for the calls it cancelled
// to report: main.go's AwaitCancelled(3s) bounds the whole SIGINT path,
// and done has to be on disk inside it.
const cancelWait = time.Second

// toolSettle is how long a changed tool set must stay changed before the
// coordinator restarts for it. A /model swap remounts every row that
// reads the llm, and their tools unregister and register again within
// one command; restarting on the first signal rebuilt the coordinator
// mid-cascade, and again once the set came back unchanged.
const toolSettle = 500 * time.Millisecond

// progressEvery and progressMax coalesce a call's live output: at most
// one call-delta per call per 100 ms, never more than 4 KiB held back.
const (
	progressEvery = 100 * time.Millisecond
	progressMax   = 4 << 10
)

// pendingCancelFor is how long a cancel that arrived before its turn
// opened is honoured: the ui submits and may cancel before the line has
// crossed the inputs chan, and dropping that cancel leaves a turn the
// user believes stopped running on.
const pendingCancelFor = 2 * time.Second

type queued struct{ id, payload string }

type adoption struct {
	job    int
	detail string
	finish func(exit *int, stopped bool)
	ended  bool
}

type coord struct {
	gen    int
	cancel context.CancelFunc
	inbox  *inbox.Inbox
	hash   string
	// stopped: the actor cancelled Run to restart it, so whatever Run
	// returns is that cancel, however the harness words it.
	stopped bool
}

type cancelState struct {
	stop     string // "" = the user; "max_steps" | "max_cost" = a budget
	calls    map[string]bool
	deadline bool // the wait is over whatever is still running
}

type progressBuf struct {
	text  strings.Builder
	armed bool
}

type forkPoint struct {
	parent  string
	turn    string
	running int
	jobs    string // "job 3: go test ./..." for the refusal
}

// actorState belongs to the actor goroutine; nothing else touches it.
type actorState struct {
	r     *Runtime
	m     *Mirror
	proj  *project.Projector
	metas map[string]project.Meta

	run      *coord
	gen      int
	restart  bool
	toolsAt  time.Time // the last tool-set change signal
	rebuilds int

	open      bool
	wake      bool
	turnStart time.Time
	turnTree  string
	adoptBase string // the tree the last turn closed on, while adopted calls run
	reported  llm.Usage
	// usageSeeded: reported holds the tally as of this Runtime's first
	// turn, not zero.
	usageSeeded bool
	lastReply   string
	stopUsed    bool
	tries       int

	inputs     []string          // every input id sent this session (a cancel parks them)
	unobserved map[string]queued // sent, not yet recorded by the coordinator
	steerQ     []queued
	noticeQ    []queued
	notes      []string
	pending    []string // lines that arrived while a turn was open
	cancelAt   time.Time
	// noticeWaits: job news arrived while a cancel was closing; it is
	// taken once the cancel's done is written.
	noticeWaits bool

	adopted    map[string]*adoption
	turnCalls  map[string]bool
	cancelled  map[string]bool
	cancelling *cancelState

	settleGen   int
	settleArmed bool
	settle      *time.Timer

	progress map[string]*progressBuf
	jobSeq   int
	drains   []func()

	seed      string // transcript prefixed to the next input
	seeded    string // "loop" | "reseeded"
	fork      *forkPoint
	frozen    string
	lastParts *prompt.Parts
	caught    int // rows the catch-up recorded at Open
	closed    bool
}

func (a *actorState) init(r *Runtime) {
	a.r = r
	a.m = newMirror()
	a.metas = map[string]project.Meta{}
	a.unobserved = map[string]queued{}
	a.adopted = map[string]*adoption{}
	a.turnCalls = map[string]bool{}
	a.cancelled = map[string]bool{}
	a.progress = map[string]*progressBuf{}
	a.jobSeq = 999
	if r.d.Projector != nil {
		a.proj = r.d.Projector("", "")
	} else {
		a.proj = project.New(project.Config{
			Detail:    r.detail,
			Render:    renderCall,
			JS:        r.jsTool(),
			RowOutput: r.cfg.RowOutput,
		})
	}
}

// observe is the store observer. It runs synchronously on the
// coordinator goroutine, so it only applies the sync mirror (which the
// Gate reads before the coordinator's next request) and queues the item.
func (r *Runtime) observe(id session.ID, it sessionstore.Item) {
	if string(id) != r.sid {
		return
	}
	r.sync.apply(it)
	r.post(func() { r.a.item(it) })
}

// ---- output ----

func (a *actorState) emit(o project.Out) {
	if o.Record {
		a.r.d.History.Append(o.Kind, o.Data)
	}
	a.r.d.Emit(o.Kind, o.Text, o.Data)
}

// note records an entry and emits the same kind: history first, because
// the event is only a change signal and its reader must find the entry.
func (a *actorState) note(kind, text string, extra map[string]any) {
	data := map[string]any{}
	maps.Copy(data, extra)
	if _, ok := data["text"]; !ok || text != "" {
		data["text"] = text
	}
	a.r.d.History.Append(kind, data)
	a.r.d.Emit(kind, text, extra)
}

func (a *actorState) live(kind, text string, data map[string]any) {
	a.r.d.Emit(kind, text, data)
}

func (a *actorState) drainHooks() {
	lc := a.lifecycle()
	if lc == nil {
		return
	}
	for _, rec := range lc.Drain() {
		a.r.d.History.Append("hook", rec)
		name, _ := rec["name"].(string)
		a.r.d.Emit("hook", name, rec)
	}
}

func (a *actorState) lifecycle() Lifecycle {
	if a.r.d.Lifecycle == nil {
		return nil
	}
	return a.r.d.Lifecycle()
}

// ---- store items, metas, deltas ----

func (a *actorState) item(it sessionstore.Item) {
	if a.closed {
		return
	}
	before := map[string]bool{}
	if it.Kind == sessionstore.ItemToolCallStatus {
		for id := range a.m.Outstanding {
			before[id] = true
		}
	}
	a.m.Apply(it)
	switch it.Kind {
	case sessionstore.ItemInput:
		if in, ok := it.Data.(inbox.Input); ok && in.Kind == inbox.InputExternal {
			delete(a.unobserved, string(in.ID))
		}
	case sessionstore.ItemModelResponse:
		mr, _ := it.Data.(sessionstore.ModelResponse)
		meta := a.metas[mr.Response.ID]
		delete(a.metas, mr.Response.ID)
		for _, o := range a.proj.Item(it) {
			a.emit(o)
		}
		a.response(mr, meta)
		return
	case sessionstore.ItemToolCallStatus:
		st, _ := it.Data.(sessionstore.ToolCallStatus)
		a.flushProgress(st.CallID)
		for _, o := range a.proj.Item(it) {
			a.emit(o)
		}
		if _, still := a.m.Outstanding[st.CallID]; before[st.CallID] && !still {
			a.ended(st)
			a.viewedImage(st)
		}
		return
	}
	for _, o := range a.proj.Item(it) {
		a.emit(o)
	}
}

func (a *actorState) response(mr sessionstore.ModelResponse, meta project.Meta) {
	r := mr.Response
	if meta.Muted {
		return
	}
	// "model is thinking" and "writing bash call" were about the
	// request, which has answered; left up, the label named a five
	// minute build "writing bash call" in the status bar and the web.
	a.live("activity", "", nil)
	if meta.Err != "" {
		a.providerError(meta.Err)
		return
	}
	if r.Stop == ullm.StopRefused {
		cat, why := "", ""
		if r.Failure != nil {
			cat = strings.TrimPrefix(r.Failure.Code, "refusal:")
			why = r.Failure.Message
		}
		a.note("error", fmt.Sprintf("the model declined (%s): %s", cat, why), map[string]any{"refusal": cat})
	}
	for _, o := range r.Output {
		switch o.Type {
		case ullm.ItemMessage:
			if msg, ok := o.Data.(ullm.Message); ok && msg.Role == ullm.RoleAssistant && strings.TrimSpace(msg.Text) != "" {
				a.lastReply = msg.Text
			}
		case ullm.ItemToolCall:
			if tc, ok := o.Data.(ullm.ToolCall); ok {
				a.turnCalls[tc.CallID] = true
			}
		}
	}
	// The request in flight has answered: the steers and notices that
	// waited for its boundary go in now.
	for _, q := range a.steerQ {
		a.send(q)
	}
	for _, q := range a.noticeQ {
		a.send(q)
	}
	a.steerQ, a.noticeQ = nil, nil
}

// providerError is §9.9: the turn fails, and the calls still running are
// adopted as jobs and parked, so their results reach the model with the
// next input instead of re-triggering a failing provider.
func (a *actorState) providerError(text string) {
	a.note("error", text, nil)
	if !a.open || a.cancelling != nil {
		return
	}
	fg := a.foreground()
	if slices.ContainsFunc(fg, a.blocking) {
		// An ask or secret still waits on the person: the turn stays
		// open, as evaluate keeps it for a blocking call, and the answer
		// reaches the model with the other calls' results once all have
		// ended. Adopted, the ask closed the turn with the Asker still
		// blocking and nothing on screen to answer it. The request is
		// over, failed or not: the steers and notices that waited for its
		// boundary go in now, as response() sends them.
		for _, q := range a.steerQ {
			a.send(q)
		}
		for _, q := range a.noticeQ {
			a.send(q)
		}
		a.steerQ, a.noticeQ = nil, nil
		return
	}
	for _, c := range fg {
		a.adopt(c)
	}
	a.r.gate.Park(slices.Collect(maps.Keys(a.turnCalls)), nil)
	a.closeTurn(len(fg), "error")
}

func (a *actorState) meta(m project.Meta) {
	a.metas[m.ResponseID] = m
	a.proj.Meta(m)
}

func (a *actorState) delta(d agentllm.Delta) {
	if a.closed {
		return
	}
	if d.Kind == agentllm.DeltaStart {
		if !a.open && a.cancelling == nil {
			a.openWake()
		}
		a.live("activity", "model is thinking", nil)
		return
	}
	for _, o := range a.proj.Delta(d) {
		a.emit(o)
	}
}

// progress coalesces a call's live output into call-delta events.
func (a *actorState) onProgress(id, text string) {
	b := a.progress[id]
	if b == nil {
		b = &progressBuf{}
		a.progress[id] = b
	}
	b.text.WriteString(text)
	if b.text.Len() >= progressMax {
		a.flushProgress(id)
		return
	}
	if !b.armed {
		b.armed = true
		time.AfterFunc(progressEvery, func() { a.r.post(func() { a.flushProgress(id) }) })
	}
}

func (a *actorState) flushProgress(id string) {
	b := a.progress[id]
	if b == nil {
		return
	}
	delete(a.progress, id)
	for _, o := range a.proj.Progress(id, b.text.String()) {
		a.emit(o)
	}
}

// ended handles a call that just reached its result.
func (a *actorState) ended(st sessionstore.ToolCallStatus) {
	delete(a.cancelled, st.CallID)
	ad := a.adopted[st.CallID]
	if ad == nil || ad.ended {
		return
	}
	ad.ended = true
	var exit *int
	stopped := false
	_, h, _ := renderCall(st.CallID, st.Status, st.Operations)
	if v, ok := h.Data["exit"]; ok {
		if n, ok := toInt(v); ok {
			exit = &n
		}
	}
	for _, op := range st.Operations {
		if op.Status == "canceled" {
			stopped = true
		}
	}
	if ad.finish != nil {
		ad.finish(exit, stopped)
	}
}

// ---- opening turns ----

func (a *actorState) submit(line string) {
	if a.closed {
		return
	}
	if a.open || a.cancelling != nil {
		a.pending = append(a.pending, line)
		return
	}
	cancel := !a.cancelAt.IsZero() && time.Since(a.cancelAt) < pendingCancelFor
	a.cancelAt = time.Time{}
	a.startTurn(line)
	if cancel && a.open {
		a.cancelTurn("")
	}
}

// admit is the loop's admit: the user-prompt-submit hook (which may
// block or rewrite the line), @file expansion, image references and
// skills. It returns what the model is sent and what history records.
func (a *actorState) admit(line string) (text, blocked string) {
	if lc := a.lifecycle(); lc != nil {
		out, block, contexts := lc.PromptSubmit(a.r.ctx, line)
		for _, c := range contexts {
			a.live("context", c, nil)
		}
		if block != "" {
			return line, block
		}
		if out != "" && out != line {
			// A hook rewriting what you typed is not allowed to do it
			// behind your back.
			a.live("context", "hook user-prompt-submit rewrote your message\n"+out, nil)
			line = out
		}
	}
	var b strings.Builder
	b.WriteString(line)
	for _, block := range loop.ExpandAt(line, a.expandRoot()) {
		b.WriteString("\n\n" + block)
	}
	// The harness takes no user images: a pasted path becomes a pointer
	// the model follows with view_image.
	for _, p := range llm.ImageRefs(line) {
		b.WriteString("\n\nthe user attached " + p + "; call view_image to look at it")
	}
	if a.r.d.Skills != nil {
		if s := a.r.d.Skills(); s != nil {
			for _, block := range s.Inject(line) {
				b.WriteString("\n\n" + block)
				a.live("context", "skill injected: "+skillName(block)+"\n"+block, nil)
			}
		}
	}
	return b.String(), ""
}

// expandRoot is where an "@path" resolves: the directory the session
// works in. d.Cwd is taken when the engine mounts, and in a project
// session the orb row mounts after it and moves the process into the
// primary worktree, so d.Cwd is still $HOME there: every @ the picker
// offered (it searches the worktree) went to the model as bare text.
func (a *actorState) expandRoot() string {
	if a.r.d.Orb != nil {
		if _, ok := a.r.d.Orb(); ok {
			if wd, err := os.Getwd(); err == nil {
				return wd
			}
		}
	}
	return a.r.d.Cwd
}

func skillName(block string) string {
	head, _, _ := strings.Cut(block, "\n")
	if n, ok := strings.CutPrefix(strings.TrimSpace(head), "[skill:"); ok {
		return strings.TrimSpace(strings.TrimSuffix(n, "]"))
	}
	return "unknown"
}

func (a *actorState) startTurn(line string) {
	msg, blocked := a.admit(line)
	a.drainHooks()
	if blocked != "" {
		a.note("error", blocked, nil)
		a.note("done", "", map[string]any{"files": []string{}})
		return
	}
	id := newInputID()
	data := map[string]any{"text": msg, "input_id": id}
	if msg != line {
		data["typed"] = line
	}
	tree := a.snapshot()
	if tree != "" {
		data["checkpoint"] = tree
	}
	e := a.r.d.History.Append("input", data)
	if cp := a.checkpoints(); cp != nil && tree != "" {
		cp.Pin(e.Seq, tree)
	}
	a.openTurn(tree, false)
	if err := a.ensureRun(); err != nil {
		a.note("error", err.Error(), nil)
		a.closeTurn(0, "error")
		return
	}
	a.send(queued{id: id, payload: a.payload(msg)})
}

// payload is what the model reads for an input: the notes left over
// from a cancel, the drift reminders, the seed transcript, then the text.
func (a *actorState) payload(text string) string {
	var parts []string
	if a.seed != "" {
		parts = append(parts, a.seed)
		a.seed = ""
	}
	parts = append(parts, a.notes...)
	a.notes = nil
	if rem := a.reminder(); rem != "" {
		parts = append(parts, rem)
	}
	parts = append(parts, text)
	return strings.Join(parts, "\n\n")
}

func (a *actorState) openTurn(tree string, wake bool) {
	if !a.usageSeeded {
		// The cost row starts a resumed session from what its history
		// says it spent; a delta from zero would charge the first turn
		// the whole earlier session again, and compound through every
		// later sum (the loop seeds at mount: loop.go r.reported). Read
		// here, not at Open, which runs inside the row's Apply, where a
		// Get would make the usage row a remount edge.
		a.usageSeeded = true
		if a.r.d.Usage != nil {
			a.reported = a.r.d.Usage()
		}
	}
	a.open, a.wake = true, wake
	a.r.openSteers()
	a.turnStart = time.Now()
	a.turnTree = tree
	a.lastReply = ""
	a.stopUsed = false
	a.tries = 0
	a.rebuilds = 0
	clear(a.turnCalls)
	for id, ad := range a.adopted {
		if ad.ended {
			delete(a.adopted, id)
		}
	}
	a.r.gate.TurnReset()
}

// send submits one input to the coordinator's inbox, building the
// coordinator first when there is none.
func (a *actorState) send(q queued) {
	a.unobserved[q.id] = q
	a.inputs = append(a.inputs, q.id)
	if err := a.ensureRun(); err != nil {
		a.note("error", err.Error(), nil)
		return
	}
	a.deliver(q)
}

func (a *actorState) deliver(q queued) {
	text := q.payload
	if a.r.d.Redact != nil {
		// History redacts the input it records (the orb row's redactor);
		// the harness store writes the payload to disk as sent, and an
		// @.env expansion or a pasted key would otherwise sit there, and
		// in `bough engine inspect`, in the clear. The model reads the
		// redacted text, as it reads the loop's redacted history.
		text = a.r.d.Redact(text)
	}
	payload, _ := json.Marshal(text)
	in := inbox.Input{ID: inbox.ID(q.id), Kind: inbox.InputExternal, Payload: jsontext.Value(payload)}
	if err := a.run.inbox.Submit(a.r.ctx, in); err != nil {
		a.note("error", "engine: "+err.Error(), nil)
	}
}

func newInputID() string { return "in-" + uuid.NewString() }

// openWake opens a bough turn the coordinator started on its own: an
// adopted call finished, or a heartbeat fired. The Gate's DeltaStart
// says so, not the ItemTurn, so a muted request never opens one.
func (a *actorState) openWake() {
	var lines, calls []string
	reason := "call"
	for _, r := range a.m.TurnReasons {
		kind, id, _ := strings.Cut(r, ":")
		switch kind {
		case "call":
			calls = append(calls, id)
			if ad := a.adopted[id]; ad != nil {
				lines = append(lines, fmt.Sprintf("[background] job %d finished: %s", ad.job, ad.detail))
			} else {
				tool, detail, _ := a.proj.Call(id)
				lines = append(lines, fmt.Sprintf("[background] %s call finished: %s", tool, detail))
			}
		case "heartbeat":
			reason = "heartbeat"
			lines = append(lines, "[background] heartbeat")
		}
	}
	if len(lines) == 0 {
		lines = []string{"[background] the model was woken"}
	}
	text := strings.Join(lines, "\n")
	tree := a.snapshot()
	data := map[string]any{"text": text, "wake": true, "reason": reason}
	if len(calls) > 0 {
		data["calls"] = calls
	}
	if tree != "" {
		data["checkpoint"] = tree
	}
	e := a.r.d.History.Append("input", data)
	if cp := a.checkpoints(); cp != nil && tree != "" {
		cp.Pin(e.Seq, tree)
	}
	a.live("job", text, map[string]any{"wake": true})
	a.openTurn(tree, true)
	if a.adoptBase != "" {
		// What an adopted call wrote after its turn closed is this
		// wake's, but the snapshot above already holds it: diff from
		// the tree its turn closed on instead.
		a.turnTree = a.adoptBase
	}
	a.r.gate.mu.Lock()
	a.r.gate.steps = 1 // this request already went out
	a.r.gate.mu.Unlock()
}

// notice lands what job-notices queued: a turn of its own while idle,
// a [notice] input inside the turn in flight otherwise.
func (a *actorState) notice() {
	if a.closed || a.r.d.Jobs == nil {
		return
	}
	j := a.r.d.Jobs()
	if j == nil {
		return
	}
	if a.cancelling != nil {
		// A [notice] input now would be an input the cancel did not
		// park: the Gate would unpark and the model carry on after Esc.
		// The news waits in Jobs until the cancel has closed.
		a.noticeWaits = true
		return
	}
	news := j.Take()
	if len(news) == 0 {
		return
	}
	text := strings.Join(news, "\n\n")
	if !a.open {
		a.live("job", text, map[string]any{"wake": true})
		id := newInputID()
		tree := a.snapshot()
		data := map[string]any{"text": jobWake + text, "wake": true, "reason": "notice", "input_id": id}
		if tree != "" {
			data["checkpoint"] = tree
		}
		e := a.r.d.History.Append("input", data)
		if cp := a.checkpoints(); cp != nil && tree != "" {
			cp.Pin(e.Seq, tree)
		}
		a.openTurn(tree, true)
		a.send(queued{id: id, payload: a.payload(jobWake + text)})
		return
	}
	a.note("job", text, nil)
	a.steerOrQueue(queued{id: newInputID(), payload: "[notice] " + text}, &a.noticeQ)
}

// landSteers admits the steers Steer queued. The gate closes with the
// turn, taking the queue first, so a queued steer finds its turn open;
// only a steer sent while a submit was still on its way can find none
// (the submit was blocked by a hook), and it runs as that input would.
func (a *actorState) landSteers() {
	for _, text := range a.r.takeSteers(false) {
		if !a.steer(text) {
			a.submit(text)
		}
	}
}

// steer lands a mid-turn message (§9.5).
func (a *actorState) steer(text string) bool {
	if a.closed || !a.open || a.cancelling != nil {
		return false
	}
	msg, blocked := a.admit(text)
	a.drainHooks()
	a.live("steer", text, nil)
	if blocked != "" {
		a.note("error", blocked, nil)
		return true
	}
	id := newInputID()
	data := map[string]any{"text": msg, "steer": true, "input_id": id}
	// As startTurn: the transcript and Edit show what was typed, not the
	// view_image pointers and @file blocks admit added for the model.
	if msg != text {
		data["typed"] = text
	}
	a.r.d.History.Append("input", data)
	a.steerOrQueue(queued{id: id, payload: msg}, &a.steerQ)
	return true
}

// steerOrQueue sends now, or at the boundary of the request in flight:
// an input the coordinator sees mid-request supersedes the request and
// throws away its paid partial output, which only steer_interrupts asks
// for.
func (a *actorState) steerOrQueue(q queued, into *[]queued) {
	if a.m.Inflight && !a.r.cfg.SteerInterrupts {
		*into = append(*into, q)
		a.inputs = append(a.inputs, q.id)
		return
	}
	a.send(q)
}

// ---- closing turns ----

// foreground is what holds the turn open: outstanding calls, minus
// those adopted as jobs and those a cancel already let go of.
func (a *actorState) foreground() []*Call {
	var fg []*Call
	for id, c := range a.m.Outstanding {
		if a.adopted[id] != nil || a.cancelled[id] {
			continue
		}
		fg = append(fg, c)
	}
	sort.Slice(fg, func(i, j int) bool {
		if !fg[i].FirstAt.Equal(fg[j].FirstAt) {
			return fg[i].FirstAt.Before(fg[j].FirstAt)
		}
		return fg[i].ID < fg[j].ID
	})
	return fg
}

func (a *actorState) blocking(c *Call) bool {
	if a.r.d.Tools == nil {
		return false
	}
	t, ok := a.r.d.Tools.Lookup(c.Tool)
	return ok && t.Blocking
}

func (a *actorState) quiescent() bool {
	return !a.m.Inflight && a.m.Pending == 0 && len(a.unobserved) == 0 &&
		len(a.steerQ) == 0 && len(a.noticeQ) == 0
}

// evaluate runs after every actor event: it is the only place a turn
// closes on its own, so a decision never runs ahead of the entries
// already written.
func (a *actorState) evaluate() {
	if a.closed {
		return
	}
	if a.cancelling != nil {
		a.checkCancel()
	}
	if a.open && a.cancelling == nil {
		if !a.quiescent() {
			a.disarm()
		} else {
			fg := a.foreground()
			switch {
			case len(fg) == 0:
				a.disarm()
				a.closeTurn(0, "")
			case slices.ContainsFunc(fg, a.blocking):
				// ask and secret wait on the user: they hold the turn
				// with no settle, as tools.ask does under the loop.
				a.disarm()
			case !a.settleArmed:
				a.arm()
			}
		}
	}
	if a.open || a.cancelling != nil {
		return
	}
	if len(a.pending) > 0 {
		line := a.pending[0]
		a.pending = a.pending[1:]
		a.submit(line)
		return
	}
	if a.restart && !a.m.Inflight && a.m.Pending == 0 && len(a.unobserved) == 0 &&
		time.Since(a.toolsAt) >= toolSettle {
		if a.run != nil && a.r.toolsHash() == a.run.hash {
			a.restart = false // the set came back as it was
		} else {
			a.restartRun()
		}
	}
	if len(a.drains) > 0 && a.idle() {
		for _, f := range a.drains {
			f()
		}
		a.drains = nil
	}
}

// idle is Drain's condition.
func (a *actorState) idle() bool {
	if a.open || a.cancelling != nil || len(a.pending) > 0 || len(a.unobserved) > 0 {
		return false
	}
	if a.run == nil {
		return true // nothing can move until the next input rebuilds
	}
	return !a.m.Inflight && a.m.Pending == 0 && len(a.m.Outstanding) == 0
}

func (a *actorState) addDrain(f func()) {
	a.drains = append(a.drains, f)
}

func (a *actorState) arm() {
	a.settleArmed = true
	a.settleGen++
	gen := a.settleGen
	a.settle = time.AfterFunc(a.r.cfg.TurnSettle, func() {
		a.r.post(func() { a.settleFired(gen) })
	})
}

func (a *actorState) disarm() {
	if a.settle != nil {
		a.settle.Stop()
	}
	a.settleArmed = false
	a.settleGen++
}

// settleFired is §9.3: the turn stayed quiescent for turn_settle with
// foreground calls still running, so each becomes a numbered job and
// the turn closes. The call's completion wakes the model later.
func (a *actorState) settleFired(gen int) {
	if gen != a.settleGen || !a.settleArmed || !a.open || a.cancelling != nil || !a.quiescent() {
		return
	}
	a.settleArmed = false
	fg := a.foreground()
	if len(fg) == 0 || slices.ContainsFunc(fg, a.blocking) {
		return
	}
	if a.landQueued(false) {
		// Something new was said: the turn is not idle after all, and
		// its calls stay foreground rather than become jobs.
		return
	}
	for _, c := range fg {
		a.adopt(c)
	}
	a.closeTurn(len(fg), "")
}

// adopt numbers a running call as a job, so the strip, /jobkill N and
// serve's job signals see it.
func (a *actorState) adopt(c *Call) {
	if a.adopted[c.ID] != nil {
		return
	}
	_, detail, ok := a.proj.Call(c.ID)
	if !ok || detail == "" {
		detail = c.Tool
	}
	id := c.ID
	kill := func() { go func() { _ = a.r.CancelCall(id) }() }
	job, finish := 0, (func(*int, bool))(nil)
	if a.r.d.Jobs != nil {
		if j := a.r.d.Jobs(); j != nil {
			job, finish = j.Adopt(detail, id, kill)
		}
	}
	if job == 0 {
		// No job numbering in this session (no tools row, or one that
		// cannot adopt): the entries serve reads are written here.
		a.jobSeq++
		job = a.jobSeq
		n := job
		a.r.d.History.Append("job", map[string]any{"id": n, "event": "started", "cmd": detail, "call": id})
		finish = func(exit *int, stopped bool) {
			data := map[string]any{"id": n, "event": "finished", "cmd": detail, "call": id}
			if exit != nil {
				data["exit"] = *exit
			}
			if stopped {
				data["stopped"] = true
			}
			a.r.d.History.Append("job", data)
			a.live("job", fmt.Sprintf("job %d finished: %s", n, detail), data)
		}
	}
	a.adopted[id] = &adoption{job: job, detail: detail, finish: finish}
	if a.proj != nil {
		a.proj.Adopt(id, job)
	}
	a.live("job", fmt.Sprintf("job %d: %s (still running; you will be told when it finishes)", job, detail), map[string]any{"id": job, "event": "started", "call": id})
}

func (a *actorState) nudge(text string) {
	id := newInputID()
	a.note("nudge", text, nil)
	a.send(queued{id: id, payload: text})
}

// closeTurn is the only writer of done, and it runs at most once per
// bough turn: that is what makes one done per turn a property.
func (a *actorState) closeTurn(running int, stop string) {
	if !a.open {
		return
	}
	// A steer queued while this turn was ending is the turn's: landed,
	// it asks the model again, and the turn goes on.
	if a.landQueued(false) {
		return
	}
	if running == 0 && stop == "" {
		if lc := a.lifecycle(); lc != nil && !a.stopUsed {
			cont := lc.Stop(a.r.ctx, a.lastReply)
			a.drainHooks()
			if cont != "" {
				a.stopUsed = true
				a.nudge(cont)
				return
			}
		}
		if a.r.d.Schema != nil {
			if sc := a.r.d.Schema(); len(sc) > 0 {
				if _, issues := sc.ValidateJSON(a.lastReply); len(issues) > 0 {
					if a.tries < a.r.cfg.StopRetries {
						a.tries++
						a.nudge(fmt.Sprintf(loop.SchemaNote, "- "+strings.Join(issues, "\n- ")))
						return
					}
					a.note("error", "reply did not match the schema: "+strings.Join(issues, "; "), nil)
				}
			}
		}
	}
	// The gate shuts with the done; a steer that slipped in since the
	// take above lands instead of the done.
	if a.landQueued(true) {
		return
	}
	a.drainHooks()
	a.note("done", "", a.doneData(running, stop))
	a.open, a.wake = false, false
	if a.r.gate != nil {
		a.r.gate.unlock()
	}
	a.disarm()
	a.markAdoptBase()
}

// landQueued lands the steers Steer queued into the open turn, and
// reports whether there were any. closing also shuts the gate when
// there were none, in the same step as the take.
func (a *actorState) landQueued(closing bool) bool {
	texts := a.r.takeSteers(closing)
	for _, text := range texts {
		a.steer(text)
	}
	return len(texts) > 0
}

// markAdoptBase remembers the tree a turn closed on while adopted calls
// still run. Their writes land after this turn's diff and before the
// next turn's snapshot, so without it they are in no turn's done.files
// and neither /undo nor Changes shows them.
func (a *actorState) markAdoptBase() {
	a.adoptBase = ""
	for id, ad := range a.adopted {
		if _, running := a.m.Outstanding[id]; running && !ad.ended {
			a.adoptBase = a.snapshot()
			return
		}
	}
}

// doneData is loop.doneData plus the engine keys.
func (a *actorState) doneData(running int, stop string) map[string]any {
	data := map[string]any{}
	var files []string
	shell := true
	if a.r.d.Stats != nil {
		if st := a.r.d.Stats(); st != nil {
			f, exit, ran := st.Take()
			files, shell = f, ran
			if ran {
				data["exit"] = exit
			}
		}
	}
	// Only when a shell ran: the diff catches what the write tools
	// could not see, and it costs a snapshot.
	if cp := a.checkpoints(); cp != nil && shell && a.turnTree != "" {
		files = mergeFiles(files, cp.Changed(a.turnTree))
	}
	if files == nil {
		files = []string{}
	}
	data["files"] = files
	if !a.turnStart.IsZero() {
		data["ms"] = time.Since(a.turnStart).Milliseconds()
	}
	if len(files) > 0 {
		after := make(map[string]string, len(files))
		for _, f := range files {
			after[f] = history.Sum(f)
		}
		data["after"] = after
	}
	if a.r.d.Usage != nil {
		now := a.r.d.Usage()
		if u := loop.UsageDelta(a.reported, now); u != nil {
			if ttl := a.cacheTTL(); ttl > 0 {
				u["ttl"] = int(ttl.Seconds())
			}
			data["usage"] = u
		}
		a.reported = now
	}
	if a.m.LastTurn != "" {
		data["engine_turn"] = string(a.m.LastTurn)
	}
	if running > 0 {
		data["running"] = running
	}
	// Jobs earlier turns adopted that still run: this close is final,
	// but the session is not done working, and serve keeps a background
	// agent's running slot until they end.
	jobs := -running
	for id, ad := range a.adopted {
		if _, out := a.m.Outstanding[id]; out && !ad.ended {
			jobs++
		}
	}
	if jobs > 0 {
		data["jobs"] = jobs
	}
	if a.wake {
		data["wake"] = true
	}
	if stop != "" {
		data["stop"] = stop
	}
	return data
}

func (a *actorState) cacheTTL() time.Duration {
	if a.r.d.LLM == nil {
		return 0
	}
	src, _, err := a.r.d.LLM()
	if err != nil || src == nil {
		return 0
	}
	return llm.AgentCacheTTL(src)
}

func mergeFiles(a, b []string) []string {
	seen := map[string]bool{}
	var out []string
	for _, f := range append(append([]string{}, a...), b...) {
		if f == "" || seen[f] {
			continue
		}
		seen[f] = true
		out = append(out, f)
	}
	return out
}

func (a *actorState) checkpoints() Checkpointer {
	if a.r.d.Checkpoints == nil {
		return nil
	}
	return a.r.d.Checkpoints()
}

func (a *actorState) snapshot() string {
	if cp := a.checkpoints(); cp != nil {
		return cp.Snapshot()
	}
	return ""
}

// ---- cancel ----

// cancelTurn is §9.7. stop is "" for the user and the budget name when
// a budget ran out.
func (a *actorState) cancelTurn(stop string) {
	if a.closed {
		return
	}
	if !a.open {
		if stop == "" {
			a.cancelAt = time.Now()
		}
		return
	}
	if a.cancelling != nil {
		return
	}
	// Steers queued before the cancel land first, while the turn is
	// still open, so they are recorded and reach the model with the
	// next input like the steers already waiting (below); the gate
	// shuts once the queue is empty.
	for a.landQueued(true) {
	}
	a.disarm()
	parked := map[string]bool{}
	for id := range a.turnCalls {
		if a.adopted[id] == nil {
			parked[id] = true
		}
	}
	// Completions the model has not been shown yet are parked too, a
	// job's that ended while this turn was open included: it reported
	// into the turn being cancelled, and unparked it woke the model into
	// a turn of its own right after the cancel.
	for _, r := range append(append([]string(nil), a.m.Reasons...), a.m.TurnReasons...) {
		if kind, id, _ := strings.Cut(r, ":"); kind == "call" && (a.adopted[id] == nil || a.adopted[id].ended) {
			parked[id] = true
		}
	}
	a.r.gate.Park(slices.Collect(maps.Keys(parked)), a.inputs)
	a.r.gate.CancelInflight()
	cs := &cancelState{stop: stop, calls: map[string]bool{}}
	for id, c := range a.m.Outstanding {
		if a.adopted[id] != nil || !a.turnCalls[id] {
			continue
		}
		cs.calls[id] = true
		a.cancelled[id] = true
		for _, op := range c.Ops {
			op := op
			go func() { _ = a.r.ops.Cancel(op, "cancelled by the user") }()
		}
	}
	// Queued steers are not lost: they reach the model with the next
	// input, under the cancelled note.
	for _, q := range a.steerQ {
		a.notes = append(a.notes, q.payload)
	}
	a.steerQ, a.noticeQ = nil, nil
	a.cancelling = cs
	time.AfterFunc(cancelWait, func() {
		a.r.post(func() {
			if a.cancelling == cs {
				cs.deadline = true
			}
		})
	})
}

func (a *actorState) budgetStop(reason string) {
	if !a.open || a.cancelling != nil {
		return
	}
	a.cancelTurn(reason)
}

// checkCancel finishes a cancel once the calls it cancelled have
// reported (their rows land before the close) and the cut request has
// answered, or when the wait is over.
func (a *actorState) checkCancel() {
	cs := a.cancelling
	if !cs.deadline {
		if a.m.Inflight {
			return
		}
		for id := range cs.calls {
			if _, running := a.m.Outstanding[id]; running {
				return
			}
		}
	}
	a.cancelling = nil
	// The cancelled calls' post-result fires land before the marker:
	// readers take "cancelled" then "done" as one close, as the loop
	// writes it (§9.7 step 4).
	a.drainHooks()
	if cs.stop == "" {
		a.note("cancelled", "", nil)
	} else {
		text := fmt.Sprintf("step budget reached (max_steps %d); stopped", a.r.cfg.MaxSteps)
		if cs.stop == "max_cost" {
			text = fmt.Sprintf("cost budget reached (max_cost_usd %g); stopped", a.r.cfg.MaxCostUSD)
		}
		a.note("system", text, nil)
	}
	a.note("done", "", a.doneData(0, cs.stop))
	a.open, a.wake = false, false
	if a.r.gate != nil {
		a.r.gate.unlock()
	}
	a.markAdoptBase()
	if a.noticeWaits {
		a.noticeWaits = false
		a.notice()
	}
}

func (a *actorState) cancelCall(callID string) error {
	c, ok := a.m.Outstanding[callID]
	if !ok {
		return fmt.Errorf("engine-unreal: call %s is not running", callID)
	}
	var errs []error
	for _, op := range c.Ops {
		if err := a.r.ops.Cancel(op, "cancelled by the user"); err != nil {
			errs = append(errs, err)
		}
	}
	return errors.Join(errs...)
}

// ---- lifecycle ----

func (a *actorState) runExit(gen int, err error) {
	if a.run == nil || a.run.gen != gen {
		return
	}
	stopped := a.run.stopped
	a.run = nil
	restarting := a.restart
	a.restart = false
	if err != nil && !stopped && !errors.Is(err, context.Canceled) {
		a.note("error", "engine: "+err.Error(), nil)
		if a.open && a.cancelling == nil {
			a.closeTurn(0, "error")
		}
	}
	// Calls still running report through a coordinator: without one,
	// their results would wait for the next input. One rebuild, not a
	// loop, if the store itself is what fails.
	if (restarting || len(a.m.Outstanding) > 0) && a.rebuilds < 1 && !a.closed {
		a.rebuilds++
		if err := a.ensureRun(); err != nil {
			a.note("error", err.Error(), nil)
		}
	}
}

func (a *actorState) toolsChanged() {
	if a.run == nil || a.closed {
		return
	}
	if h := a.r.toolsHash(); h != a.run.hash {
		a.restart = true
		a.toolsAt = time.Now()
		// evaluate runs after every posted func: this one is the
		// re-check once the set has had toolSettle to stop moving.
		time.AfterFunc(toolSettle, func() { a.r.post(func() {}) })
	}
}

func (a *actorState) restartRun() {
	if a.run != nil {
		a.run.stopped = true
		a.run.cancel()
	}
}

func (a *actorState) shutdown() {
	if a.closed {
		return
	}
	if a.open && a.cancelling == nil {
		a.cancelTurn("")
	}
	if a.cancelling != nil {
		a.cancelling.deadline = true
		a.checkCancel()
	}
	for id := range a.progress {
		a.flushProgress(id)
	}
	a.disarm()
	a.closed = true
	if a.run != nil {
		a.run.cancel()
		a.run = nil
	}
	a.r.gate.close()
}

func toInt(v any) (int, bool) {
	switch n := v.(type) {
	case int:
		return n, true
	case int64:
		return int(n), true
	case uint64:
		// project stamps hseq as a Sequence (uint64) on the live entry;
		// read back from the file it is a float64.
		return int(n), true
	case float64:
		return int(n), true
	case json.Number:
		i, err := n.Int64()
		return int(i), err == nil
	}
	return 0, false
}
