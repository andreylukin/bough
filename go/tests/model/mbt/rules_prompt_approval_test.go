//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/rules_prompt_approval.fizz against a real serve: a Codex prefix
// rule with decision "prompt" (~/.codex/rules/rpa.rules matches
// `echo danger …`) gates the model's bash on the person's approval, in
// code mode (the loop row is plugin: loop; tools.bash inside a block),
// on the engine (engine-unreal; the native bash call) and from a
// subagent's bash, beside the engine's native ask.
//
// How each spec event is made real:
//
//   - The model is llm-control. Code mode's requests are its Stream
//     (the reply's text carries the js block), a code-mode subagent's
//     its Complete; the engine's are its Respond. One block turn is
//     always queued ("t…"), so whatever request the parent makes next is
//     held; a child's turns are queued as "a…", which sort first, right
//     before the child asks for them.
//   - Each call's command, question or task carries a marker
//     (c1-w3t2: call c1 of walk 3's turn 2), which is how every entry,
//     row ask and arm is tied back to the call.
//   - On the engine the four calls of one reply start at once, so the
//     first of Bash, NativeAsk and SpawnBash in a turn releases the
//     reply with all four (bash c1, bash c2, ask, spawn) and each is held
//     at the Asker by a hold-req-<marker> file in
//     BOUGH_TEST_ASK_EXPIRE_DIR until its own action removes it; a call
//     that queues behind an open ask is also held by hold-grant-<marker>
//     until Grant picks it. Finish withdraws the calls the walk never
//     requested ("skip"), which end without an ask.
//   - Timeout drops a file named for the open ask's id in the same dir.
//   - Stop is the Stop button pressed (the adapter's stopping); Cancel
//     is serve's interrupt reaching the child, which must bring the
//     session to stopped.
//   - SwitchLoop and ReloadAsk rewrite $HOME/.bough/bough.yml (the loop
//     row's plugin; the ask row's timeout_minutes, which remounts it)
//     and wait for the live child's "bough: reloaded".
//
// What is read off the server: status (the row); each call's state
// (its ask entry, its answer or its call's recorded end, the turn's
// close; "wait" is a call the walk released that has asked nothing
// yet); shown (the row's ask); armed (serve's arm, probed with an
// /answer naming no real question, tied to the last ask entry); raised
// (which Asker instance numbered the open ask: each numbers its asks
// from ask-1, and a ReloadAsk on a live child starts a new one); crossed
// (the call the ask/answer entry an answer wrote belongs to, against
// the call the page answered). mode, gen and stopping are the adapter's
// own: they are the person's edits and button.

const rpaRules = `prefix_rule(pattern = ["echo", "danger"], decision = "prompt")` + "\n"

// rpaGrace outlasts the engine's one-second hold on a reply's finished
// calls, after which a call's end makes the parent's next request.
const rpaGrace = 1200 * time.Millisecond

var rpaCalls = []string{"c1", "c2", "nat", "sub"}

var rpaMarker = regexp.MustCompile(`\b(c1|c2|nat|sub)-w\d+t\d+\b`)

func rpaConfig(mode string, gen int) string {
	loop := "engine-unreal"
	if mode == "code" {
		loop = "loop"
	}
	return controlConfig + fmt.Sprintf("- id: ask\n  plugin: ask\n  config:\n    timeout_minutes: %d\n- id: loop\n  plugin: %s\n", 10+gen, loop)
}

type rpaAdapter struct {
	t      *testing.T
	s      *servetest.Server
	dir    string // llm-control's queue
	expire string // BOUGH_TEST_ASK_EXPIRE_DIR
	gate   gate

	id, cwd string
	ids     []string
	walk    int
	turn    int
	q       int      // turn names
	n       int      // answer markers
	next    string   // the first block turn queued for the parent
	queued  []string // the parent's block turns, not yet taken
	held    []string // parent requests taken and not answered
	child   []string // child turns queued, removed if never taken

	mode      string
	gen       int
	stopping  bool
	crossed   bool
	replied   bool            // engine: this turn's reply is out
	requested map[string]bool // calls the walk has made ask, this turn
	live      bool            // the child has spawned
	insts     []int           // history length where each Asker instance begins
	instGen   []int           // the gen it was mounted with
	instChild []int           // and the child it lives in
	children  int

	wantGrant string // the path walk's choice for the next Grant
	did       map[string]int

	// timeoutMisses is the wrong-adapter test's bug: Timeout expires a
	// question that is not the open one.
	timeoutMisses bool
}

func newRpaAdapter(t *testing.T) *rpaAdapter {
	expire := t.TempDir()
	s := servetest.Start(t, servetest.Options{
		Config: rpaConfig("code", 0),
		Files:  map[string]string{".codex/rules/rpa.rules": rpaRules},
		Env:    []string{"BOUGH_TEST_ASK_EXPIRE_DIR=" + expire},
	})
	return &rpaAdapter{t: t, s: s, dir: control.Dir(s.Home), expire: expire, did: map[string]int{}}
}

func (a *rpaAdapter) marker(c string) string { return fmt.Sprintf("%s-w%dt%d", c, a.walk, a.turn) }

func (a *rpaAdapter) cmd(c string) string { return "echo danger " + a.marker(c) }

// queueNext keeps two block turns queued for the parent: the engine
// restarts a request whose tool set changed under it (a reload), and
// the restart asks again at once.
func (a *rpaAdapter) queueNext() {
	for len(a.queued) < 2 {
		a.q++
		name := fmt.Sprintf("t%06d", a.q)
		a.queued = append(a.queued, name)
		control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "Done."})
	}
	a.next = a.queued[0]
}

// queueChild queues a subagent's next reply; only a subagent takes it.
func (a *rpaAdapter) queueChild(turn control.Turn) {
	turn.Child = true
	a.q++
	name := fmt.Sprintf("a%06d", a.q)
	a.child = append(a.child, name)
	control.Queue(a.t, a.dir, name, turn)
}

// childReport is the subagent's last reply, queued before its bash
// ends so its next request finds it.
func (a *rpaAdapter) childReport() {
	a.queueChild(control.Turn{Mode: "ok", Text: "Status: ok\nFindings: none\nFiles: none\nOpen: none"})
}

// dropChild removes child turns nobody took, so they cannot answer a
// later request.
func (a *rpaAdapter) dropChild() {
	for _, name := range a.child {
		os.Remove(filepath.Join(a.dir, name+".json"))
	}
	a.child = nil
}

// absorb moves the queued turns requests have taken onto held.
func (a *rpaAdapter) absorb() bool {
	took := false
	for len(a.queued) > 0 {
		if _, err := os.Stat(filepath.Join(a.dir, a.queued[0]+".taken")); err != nil {
			break
		}
		a.held = append(a.held, a.queued[0])
		a.queued = a.queued[1:]
		took = true
	}
	if took {
		a.queueNext()
	}
	return took
}

// release answers the newest held request as turn says: the engine has
// one request in flight at a time, and one it restarted (its tool set
// changed under a reload) is left held and answers nothing.
func (a *rpaAdapter) release(turn control.Turn) error {
	if len(a.held) == 0 {
		return errors.New("no model request is held")
	}
	name := a.held[len(a.held)-1]
	a.held = a.held[:len(a.held)-1]
	control.ReleaseWith(a.t, a.dir, name, turn)
	return nil
}

func (a *rpaAdapter) hold(stage, c, body string) error {
	return os.WriteFile(filepath.Join(a.expire, "hold-"+stage+"-"+a.marker(c)), []byte(body), 0o644)
}

func (a *rpaAdapter) unhold(stage, c string) {
	os.Remove(filepath.Join(a.expire, "hold-"+stage+"-"+a.marker(c)))
}

func (a *rpaAdapter) held4(stage, c string) bool {
	_, err := os.Stat(filepath.Join(a.expire, "hold-"+stage+"-"+a.marker(c)))
	return err == nil
}

// clearHolds drops every hold and expire file: a new turn starts clean.
func (a *rpaAdapter) clearHolds() {
	ents, _ := os.ReadDir(a.expire)
	for _, e := range ents {
		os.Remove(filepath.Join(a.expire, e.Name()))
	}
}

func (a *rpaAdapter) newTurn() {
	a.replied = false
	a.requested = map[string]bool{}
	a.clearHolds()
	a.dropChild()
}

// writeConfig replaces the overlay and, while the child is alive, waits
// for its "bough: reloaded" (serve relays the child's stderr as events)
// stamped after the write: the stream replays the ones before it, a
// child's that has since gone among them.
func (a *rpaAdapter) writeConfig() error {
	path := filepath.Join(a.s.Home, ".bough", "bough.yml")
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, []byte(rpaConfig(a.mode, a.gen)), 0o644); err != nil {
		return err
	}
	written := time.Now()
	if err := os.Rename(tmp, path); err != nil {
		return err
	}
	if !a.live {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	st, err := a.s.Events(ctx, a.id)
	if err != nil {
		return err
	}
	defer st.Close()
	_, err = st.WaitFor(ctx, func(ev serve.Event) bool {
		return strings.Contains(ev.Text, "bough: reloaded") && ev.At.After(written)
	})
	if err != nil {
		return fmt.Errorf("waiting for the child to reload: %w", err)
	}
	return nil
}

func (a *rpaAdapter) Init() error {
	a.walk++
	a.turn = 0
	a.mode, a.gen = "code", 0
	a.stopping, a.crossed, a.live = false, false, false
	a.insts, a.instGen, a.instChild = nil, nil, nil
	a.held = nil
	a.newTurn()
	a.gate.reset()
	if err := a.writeConfig(); err != nil {
		return err
	}
	a.queueNext()
	ctx, cancel := actionCtx()
	defer cancel()
	a.cwd = a.s.Dir(a.t, fmt.Sprintf("w%d", a.walk))
	row, err := a.s.CreateSession(ctx, a.cwd, "")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	row, err = waitRow(a.s, a.id, "idle", func(r serve.Row) bool { return r.Status == serve.StatusIdle })
	if err != nil {
		return err
	}
	a.spawned(row, 0)
	return nil
}

// spawned notes a child that came up since the last look (a create, or
// the prompt after a Stop took the last one down): its Asker is a new
// instance, mounted with the config's gen, numbering the asks recorded
// from history position at on.
func (a *rpaAdapter) spawned(row serve.Row, at int) {
	switch {
	case row.Live && !a.live:
		a.live = true
		a.children++
		a.newInst(at)
	case !row.Live:
		a.live = false
	}
}

// Cleanup ends the walk's child (Archive kills it) and takes back what
// the walk left queued or held, so the next walk's requests are its own.
func (a *rpaAdapter) Cleanup() error {
	a.clearHolds()
	ctx, cancel := actionCtx()
	defer cancel()
	if a.id != "" {
		if _, err := a.s.Archive(ctx, a.id); err != nil {
			return err
		}
		if _, err := waitRow(a.s, a.id, "the child to be gone", func(r serve.Row) bool { return !r.Live }); err != nil {
			return err
		}
	}
	for _, h := range a.held {
		control.Release(a.t, a.dir, h)
	}
	a.held = nil
	a.dropChild()
	for _, name := range a.queued {
		os.Remove(filepath.Join(a.dir, name+".json"))
	}
	a.queued, a.next = nil, ""
	return nil
}

func (a *rpaAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

// rpaView is one read of the server.
type rpaView struct {
	row     serve.Row
	entries []history.Entry
	calls   map[string]string // c1..sub -> "", "wait", "open", "end", "gone"
	askID   map[string]string
	askAt   map[string]int
	shown   string
	armed   string
	raised  string
}

func callOf(text string) string {
	if m := rpaMarker.FindStringSubmatch(text); m != nil {
		return m[1]
	}
	return ""
}

// entryHas says whether e's text or data mentions s.
func entryHas(e history.Entry, s string) bool {
	if strings.Contains(history.EntryText(e), s) {
		return true
	}
	b, _ := json.Marshal(e.Data)
	return strings.Contains(string(b), s)
}

func callEnd(e history.Entry) bool {
	return (e.Kind == "call" || e.Kind == "sub:call") && e.Data["phase"] != "start"
}

func (a *rpaAdapter) view() (rpaView, error) {
	v := rpaView{calls: map[string]string{}, askID: map[string]string{}, askAt: map[string]int{}}
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return v, err
	}
	v.row = row
	v.entries, err = history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
	if err != nil && !os.IsNotExist(err) {
		return v, err
	}
	// The turn: from the last input to its done or cancelled.
	start, closed := -1, true
	for i, e := range v.entries {
		switch e.Kind {
		case "input":
			start, closed = i, false
		case "done", "cancelled":
			closed = true
		}
	}
	for _, c := range rpaCalls {
		m := a.marker(c)
		answered, ended := false, false
		if start >= 0 {
			for i := start; i < len(v.entries); i++ {
				e := v.entries[i]
				switch {
				case e.Kind == "ask" && entryHas(e, m):
					v.askID[c], v.askAt[c] = str(e.Data["id"]), i
					answered, ended = false, false
				case e.Kind == "ask/answer" && v.askID[c] != "" && str(e.Data["id"]) == v.askID[c]:
					answered = true
				case e.Kind == "ask/end" && v.askID[c] != "" && str(e.Data["id"]) == v.askID[c]:
					ended = true
				case callEnd(e) && entryHas(e, m):
					ended = true
				}
			}
		}
		switch {
		case closed:
			v.calls[c] = ""
		case v.askID[c] != "" && (answered || ended):
			v.calls[c] = "end"
		case v.askID[c] != "":
			v.calls[c] = "open"
		case a.requested[c] && ended:
			v.calls[c] = "gone"
		case a.requested[c]:
			v.calls[c] = "wait"
		default:
			v.calls[c] = ""
		}
	}
	if row.Ask != nil {
		v.shown = "?"
		if c := callOf(row.Ask.Text); c != "" && strings.Contains(row.Ask.Text, a.marker(c)) {
			v.shown = c
		}
	}
	// serve's arm, probed without touching it: an answer naming no real
	// question is refused as expired while an ask is armed, and as no
	// pending ask when none is. The arm is the last ask emitted.
	code, msg, err := a.post(ctx, "answer", map[string]string{"text": "", "ask": "probe-not-an-ask"})
	switch {
	case err != nil:
		return v, err
	case code == http.StatusConflict && strings.Contains(msg, "expired"):
		v.armed = "?"
		for i := len(v.entries) - 1; i >= 0; i-- {
			if e := v.entries[i]; e.Kind == "ask" {
				if c := callOf(str(e.Data["question"])); c != "" && entryHas(e, a.marker(c)) {
					v.armed = c
				}
				break
			}
		}
	case code == http.StatusConflict && strings.Contains(msg, "no pending ask"):
	default:
		return v, fmt.Errorf("arm probe: %d %s", code, msg)
	}
	open := ""
	for _, c := range rpaCalls {
		if v.calls[c] == "open" {
			if open == "" || c == v.shown {
				open = c
			}
		}
	}
	if open != "" {
		v.raised = a.raisedOn(v.entries, v.askAt[open])
	}
	return v, nil
}

// raisedOn names the Asker instance that numbered the ask entry at idx:
// its gen when it is the current one, "stale" when it is one a ReloadAsk
// replaced. Each instance numbers its asks ask-1, ask-2, …, so an entry
// is the current instance's when it is next in its count.
func (a *rpaAdapter) raisedOn(entries []history.Entry, idx int) string {
	count := make([]int, len(a.insts))
	cur, inst := -1, -1
	for i, e := range entries {
		for cur+1 < len(a.insts) && a.insts[cur+1] <= i {
			cur++
		}
		if e.Kind != "ask" || cur < 0 {
			continue
		}
		k, err := strconv.Atoi(strings.TrimPrefix(str(e.Data["id"]), "ask-"))
		in := -1
		if err == nil {
			// Only an instance of the child that recorded it: a child
			// that is gone took its Askers with it.
			for j := cur; j >= 0 && a.instChild[j] == a.instChild[cur]; j-- {
				if count[j]+1 == k {
					in = j
					break
				}
			}
		}
		if in >= 0 {
			count[in]++
		}
		if i == idx {
			inst = in
			break
		}
	}
	if inst < 0 || inst != len(a.insts)-1 {
		return "stale"
	}
	return strconv.Itoa(a.instGen[inst])
}

// newInst notes an Asker instance of the current child, mounted with
// the config's gen, numbering the asks recorded from position at on.
func (a *rpaAdapter) newInst(at int) {
	a.insts = append(a.insts, at)
	a.instGen = append(a.instGen, a.gen)
	a.instChild = append(a.instChild, a.children)
}

func (a *rpaAdapter) GetState() (map[string]any, error) {
	v, err := a.view()
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"status":   string(v.row.Status),
		"mode":     a.mode,
		"gen":      a.gen,
		"c1":       v.calls["c1"],
		"c2":       v.calls["c2"],
		"nat":      v.calls["nat"],
		"sub":      v.calls["sub"],
		"shown":    v.shown,
		"armed":    v.armed,
		"raised":   v.raised,
		"stopping": a.stopping,
		"crossed":  a.crossed,
	}, nil
}

func (a *rpaAdapter) post(ctx context.Context, verb string, body any) (int, string, error) {
	b, _ := json.Marshal(body)
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.s.URL+"/api/sessions/"+a.id+"/"+verb, strings.NewReader(string(b)))
	if err != nil {
		return 0, "", err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, "", err
	}
	defer resp.Body.Close()
	var e struct {
		Error string `json:"error"`
	}
	json.NewDecoder(resp.Body).Decode(&e)
	return resp.StatusCode, e.Error, nil
}

// settle waits until the transcript, the row and the llm queue have
// been still for a moment, absorbing any request the parent made.
func (a *rpaAdapter) settle() error {
	const quiet = 300 * time.Millisecond
	deadline := time.Now().Add(actionTimeout)
	last, lastAt := "", time.Now()
	for time.Now().Before(deadline) {
		if a.absorb() {
			lastAt = time.Now()
		}
		ctx, cancel := actionCtx()
		row, lines, err := a.s.GetSession(ctx, a.id)
		cancel()
		if err != nil {
			return err
		}
		sig := fmt.Sprintf("%s %v %d %d", row.Status, row.Live, len(lines), len(a.held))
		if sig != last {
			last, lastAt = sig, time.Now()
		} else if time.Since(lastAt) >= quiet {
			return nil
		}
		time.Sleep(20 * time.Millisecond)
	}
	return fmt.Errorf("the session did not settle in %s", actionTimeout)
}

// settleEnd is settle after a call ended: on the engine the parent's
// next request comes a second later.
func (a *rpaAdapter) settleEnd() error {
	if a.mode == "engine" {
		time.Sleep(rpaGrace)
	}
	return a.settle()
}

// waitFor polls the view until ok holds, for at most d; it never fails:
// the state check after the step says what did not happen.
func (a *rpaAdapter) waitFor(d time.Duration, ok func(rpaView) bool) {
	deadline := time.Now().Add(d)
	for time.Now().Before(deadline) {
		a.absorb()
		if v, err := a.view(); err == nil && ok(v) {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func busy(v rpaView) bool {
	for _, c := range rpaCalls {
		if v.calls[c] == "open" || v.calls[c] == "wait" {
			return true
		}
	}
	return false
}

func anyOpen(v rpaView) bool {
	for _, c := range rpaCalls {
		if v.calls[c] == "open" {
			return true
		}
	}
	return false
}

func between(v rpaView) bool {
	switch v.row.Status {
	case serve.StatusIdle, serve.StatusDone, serve.StatusStopped:
		return true
	}
	return false
}

func (a *rpaAdapter) midTurn(v rpaView) bool {
	return (v.row.Status == serve.StatusRunning || v.row.Status == serve.StatusNeedsYou) && !a.stopping
}

// --- the person, between turns ---

func (a *rpaAdapter) Prompt() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(between(v) && !a.stopping) {
		return nil
	}
	a.turn++
	a.newTurn()
	a.spawned(v.row, len(v.entries))
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, fmt.Sprintf("go w%dt%d", a.walk, a.turn)); err != nil {
		return err
	}
	if err := waitTaken(a.dir, a.next); err != nil {
		return err
	}
	a.absorb()
	row, err := waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
	a.spawned(row, len(v.entries))
	if err != nil {
		return err
	}
	return a.settle()
}

func (a *rpaAdapter) SwitchLoop() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(between(v) && !a.stopping) {
		return nil
	}
	if a.mode == "code" {
		a.mode = "engine"
	} else {
		a.mode = "code"
	}
	a.spawned(v.row, len(v.entries))
	return a.writeConfig()
}

func (a *rpaAdapter) ReloadAsk() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(between(v) && !a.stopping) {
		return nil
	}
	a.spawned(v.row, len(v.entries))
	a.gen = 1 - a.gen
	if a.live {
		a.newInst(len(v.entries))
	}
	return a.writeConfig()
}

// --- the model ---

// reply releases the engine's reply for this turn: all four calls at
// once, each held at the Asker until its own action.
func (a *rpaAdapter) reply() error {
	for _, c := range rpaCalls {
		if err := a.hold("req", c, ""); err != nil {
			return err
		}
	}
	a.queueChild(control.Turn{Mode: "ok", Calls: []control.Call{{
		ID: "bash_" + a.marker("sub"), Name: "bash", Args: map[string]any{"command": a.cmd("sub")},
	}}})
	calls := []control.Call{
		{ID: "bash_" + a.marker("c1"), Name: "bash", Args: map[string]any{"command": a.cmd("c1")}},
		{ID: "bash_" + a.marker("c2"), Name: "bash", Args: map[string]any{"command": a.cmd("c2")}},
		{ID: "ask_" + a.marker("nat"), Name: "ask", Args: map[string]any{"question": "Which one? " + a.marker("nat"), "options": []string{"red", "blue"}}},
		{ID: "spawn_" + a.marker("sub"), Name: "spawn", Args: map[string]any{"task": "Run the command " + a.cmd("sub") + " and report. " + a.marker("sub")}},
	}
	if err := a.release(control.Turn{Calls: calls}); err != nil {
		return err
	}
	a.replied = true
	return nil
}

// request makes c reach the Asker now: it asks at once when no ask is
// open, and otherwise queues, held for Grant as well.
func (a *rpaAdapter) request(c string, v rpaView) error {
	free := !anyOpen(v)
	if !free {
		if err := a.hold("grant", c, ""); err != nil {
			return err
		}
	}
	a.requested[c] = true
	a.unhold("req", c)
	if free {
		a.waitFor(5*time.Second, func(v rpaView) bool { return v.calls[c] == "open" })
	}
	return a.settle()
}

// block is a code-mode reply: one js block, its failure caught so a
// refused command is a result, not an error that marks the turn.
func block(js string) string {
	return "```js\ntry { " + js + " } catch (e) { 'not run: ' + e }\n```"
}

func (a *rpaAdapter) Bash() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.midTurn(v) && v.calls["c1"] == "" && (a.mode == "engine" || !busy(v))) {
		return nil
	}
	if a.mode == "code" {
		a.requested["c1"] = true
		if err := a.release(control.Turn{Text: block("tools.bash(" + strconv.Quote(a.cmd("c1")) + ")")}); err != nil {
			return err
		}
		a.waitFor(10*time.Second, func(v rpaView) bool { return v.calls["c1"] == "open" })
		return a.settle()
	}
	if !a.replied {
		if err := a.reply(); err != nil {
			return err
		}
	}
	return a.request("c1", v)
}

func (a *rpaAdapter) ParallelBash() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.midTurn(v) && a.mode == "engine" && v.calls["c1"] != "" && v.calls["c2"] == "") {
		return nil
	}
	return a.request("c2", v)
}

func (a *rpaAdapter) NativeAsk() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.midTurn(v) && a.mode == "engine" && v.calls["nat"] == "") {
		return nil
	}
	if !a.replied {
		if err := a.reply(); err != nil {
			return err
		}
	}
	return a.request("nat", v)
}

func (a *rpaAdapter) SpawnBash() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.midTurn(v) && v.calls["sub"] == "" && (a.mode == "engine" || !busy(v))) {
		return nil
	}
	if a.mode == "code" {
		a.requested["sub"] = true
		a.queueChild(control.Turn{Mode: "ok", Text: block("tools.bash(" + strconv.Quote(a.cmd("sub")) + ")")})
		task := "Run the command " + a.cmd("sub") + " and report. " + a.marker("sub")
		if err := a.release(control.Turn{Text: block("tools.spawn(" + strconv.Quote(task) + ")")}); err != nil {
			return err
		}
		a.waitFor(10*time.Second, func(v rpaView) bool { return v.calls["sub"] == "open" })
		return a.settle()
	}
	if !a.replied {
		if err := a.reply(); err != nil {
			return err
		}
	}
	return a.request("sub", v)
}

func (a *rpaAdapter) Grant() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	var waiting []string
	for _, c := range rpaCalls {
		if v.calls[c] == "wait" {
			waiting = append(waiting, c)
		}
	}
	if !a.gate.pass(!a.stopping && !anyOpen(v) && len(waiting) > 0) {
		return nil
	}
	c := waiting[0]
	for _, w := range waiting {
		if w == a.wantGrant {
			c = w
		}
	}
	a.unhold("grant", c)
	a.waitFor(5*time.Second, func(v rpaView) bool { return v.calls[c] == "open" })
	return a.settle()
}

// Finish lets the turn end: the calls the walk never made ask are
// withdrawn, and every request the parent makes is answered with text.
func (a *rpaAdapter) Finish() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Status == serve.StatusRunning && !a.stopping && !busy(v)) {
		return nil
	}
	if a.replied {
		for _, c := range rpaCalls {
			if a.held4("req", c) {
				if c == "sub" {
					a.childReport()
				}
				if err := a.hold("req", c, "skip"); err != nil {
					return err
				}
			}
		}
	}
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		a.absorb()
		for len(a.held) > 0 {
			if err := a.release(control.Turn{Text: "Done."}); err != nil {
				return err
			}
		}
		ctx, cancel := actionCtx()
		row, _, err := a.s.GetSession(ctx, a.id)
		cancel()
		if err != nil {
			return err
		}
		if row.Status != serve.StatusRunning && row.Status != serve.StatusNeedsYou {
			a.newTurn()
			return a.settle()
		}
		time.Sleep(20 * time.Millisecond)
	}
	return errors.New("finish: the turn did not end")
}

// --- the person, on the ask ---

func (a *rpaAdapter) answer(text string) error {
	v, err := a.view()
	if err != nil {
		return err
	}
	c, askID := v.shown, v.row.Ask.ID
	if c == "sub" {
		a.childReport()
	}
	ctx, cancel := actionCtx()
	defer cancel()
	code, _, err := a.post(ctx, "answer", map[string]string{"text": text, "ask": askID})
	if err != nil {
		return err
	}
	if code != http.StatusOK {
		// Refused: the page shows why and nothing changed.
		return a.settle()
	}
	// The call the child handed the answer to: the ask entry whose id
	// the ask/answer it wrote names.
	n := len(v.entries)
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		es, _ := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
		got := ""
		for i := n; i < len(es) && got == ""; i++ {
			if es[i].Kind != "ask/answer" {
				continue
			}
			id := str(es[i].Data["id"])
			got = "?"
			for j := i - 1; j >= 0; j-- {
				if es[j].Kind == "ask" && str(es[j].Data["id"]) == id {
					got = callOf(str(es[j].Data["question"]))
					break
				}
			}
		}
		if got != "" {
			if got != c {
				a.crossed = true
			}
			break
		}
		time.Sleep(20 * time.Millisecond)
	}
	return a.settleEnd()
}

func (a *rpaAdapter) approval(v rpaView) bool {
	return !a.stopping && (v.shown == "c1" || v.shown == "c2" || v.shown == "sub") && v.row.Ask != nil
}

func (a *rpaAdapter) ClickRun() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.approval(v)) {
		return nil
	}
	return a.answer("run")
}

func (a *rpaAdapter) ClickRefuse() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.approval(v)) {
		return nil
	}
	return a.answer("refuse")
}

func (a *rpaAdapter) AnswerText() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(!a.stopping && v.shown != "" && v.row.Ask != nil) {
		return nil
	}
	a.n++
	return a.answer(fmt.Sprintf("typed answer %d", a.n))
}

// --- the Asker's timeout ---

func (a *rpaAdapter) Timeout() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(anyOpen(v) && !a.stopping) {
		return nil
	}
	c := ""
	for _, o := range rpaCalls {
		if v.calls[o] == "open" {
			c = o
			break
		}
	}
	if c == "sub" {
		a.childReport()
	}
	id := v.askID[c]
	if a.timeoutMisses {
		id = "ask-999"
	}
	if err := os.WriteFile(filepath.Join(a.expire, id), nil, 0o644); err != nil {
		return err
	}
	a.waitFor(5*time.Second, func(v rpaView) bool { return v.calls[c] != "open" })
	return a.settleEnd()
}

// --- Stop ---

func (a *rpaAdapter) Stop() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.midTurn(v)) {
		return nil
	}
	a.stopping = true
	return nil
}

// Cancel is the interrupt reaching the child: the turn must close as
// stopped with every call let go, the held and queued ones included.
func (a *rpaAdapter) Cancel() error {
	if !a.gate.pass(a.stopping) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Interrupt(ctx, a.id); err != nil {
		return err
	}
	a.stopping = false
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		row, _, err := a.s.GetSession(ctx, a.id)
		if err != nil {
			return err
		}
		if row.Status == serve.StatusStopped {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}
	// The requests the cancel ended are nobody's now.
	a.held = nil
	a.newTurn()
	if err := a.settle(); err != nil {
		return err
	}
	v, err := a.view()
	if err != nil {
		return err
	}
	a.spawned(v.row, len(v.entries))
	return nil
}

func rpaAction(name string, f func(*rpaAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*rpaAdapter)
		err := f(a)
		if !a.gate.off {
			a.did[name]++
		}
		return nil, err
	}
}

var rpaActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Prompt":       rpaAction("Prompt", (*rpaAdapter).Prompt),
	"SwitchLoop":   rpaAction("SwitchLoop", (*rpaAdapter).SwitchLoop),
	"ReloadAsk":    rpaAction("ReloadAsk", (*rpaAdapter).ReloadAsk),
	"Bash":         rpaAction("Bash", (*rpaAdapter).Bash),
	"ParallelBash": rpaAction("ParallelBash", (*rpaAdapter).ParallelBash),
	"NativeAsk":    rpaAction("NativeAsk", (*rpaAdapter).NativeAsk),
	"SpawnBash":    rpaAction("SpawnBash", (*rpaAdapter).SpawnBash),
	"Grant":        rpaAction("Grant", (*rpaAdapter).Grant),
	"Finish":       rpaAction("Finish", (*rpaAdapter).Finish),
	"ClickRun":     rpaAction("ClickRun", (*rpaAdapter).ClickRun),
	"ClickRefuse":  rpaAction("ClickRefuse", (*rpaAdapter).ClickRefuse),
	"AnswerText":   rpaAction("AnswerText", (*rpaAdapter).AnswerText),
	"Timeout":      rpaAction("Timeout", (*rpaAdapter).Timeout),
	"Stop":         rpaAction("Stop", (*rpaAdapter).Stop),
	"Cancel":       rpaAction("Cancel", (*rpaAdapter).Cancel),
}}

func rpaOptions() map[string]any {
	return map[string]any{"max-seq-runs": 60, "max-actions": 12, "max-parallel-runs": 0}
}

// rpaHistory reads the abstract trace off a transcript. Config reloads
// leave nothing in history: a turn run with "code" entries is code mode,
// one with calls and no code is the engine, and a SwitchLoop goes in
// before the prompt of a turn whose mode differs; ReloadAsk is never
// seen (gen only names which Asker is current). A queued call leaves no
// entry until it is granted, so each call's request is read at its ask,
// when the slot was free; a turn whose second bash asked before the
// first (the first queued behind an ask and the second was granted
// first) cannot be told from history and ends the trace there. The
// check is on status and shown.
func rpaHistory(entries []history.Entry) []tracecheck.Step {
	st := func(status, shown string) map[string]any {
		return map[string]any{"Session#0.status": status, "Session#0.shown": shown}
	}
	steps := []tracecheck.Step{{Action: "Init", State: st("idle", "")}}
	add := func(action string, state map[string]any) {
		steps = append(steps, tracecheck.Step{Action: "Session#0." + action, State: state})
	}
	request := map[string]string{"c1": "Bash", "c2": "ParallelBash", "nat": "NativeAsk", "sub": "SpawnBash"}
	mode := "code"
	var turns [][]history.Entry
	for i, e := range entries {
		if e.Kind == "input" {
			turns = append(turns, nil)
		}
		if len(turns) > 0 {
			turns[len(turns)-1] = append(turns[len(turns)-1], entries[i])
		}
	}
	for _, turn := range turns {
		tm := ""
		for _, e := range turn {
			switch {
			case e.Kind == "code":
				tm = "code"
			case e.Kind == "call" && tm == "":
				tm = "engine"
			}
		}
		if tm != "" && tm != mode {
			add("SwitchLoop", nil)
			mode = tm
		}
		add("Prompt", st("running", ""))
		open := map[string]string{} // ask id -> call
		asked := map[string]bool{}
		closed := false
		for _, e := range turn {
			if closed {
				break
			}
			switch e.Kind {
			case "ask":
				c := callOf(str(e.Data["question"]))
				if c == "" {
					continue
				}
				if c == "c2" && !asked["c1"] {
					return steps
				}
				asked[c] = true
				open[str(e.Data["id"])] = c
				add(request[c], st("needs-you", c))
			case "ask/answer":
				if _, ok := open[str(e.Data["id"])]; !ok {
					continue
				}
				delete(open, str(e.Data["id"]))
				switch str(e.Data["text"]) {
				case "run":
					add("ClickRun", st("running", ""))
				case "refuse":
					add("ClickRefuse", st("running", ""))
				default:
					add("AnswerText", st("running", ""))
				}
			case "ask/end":
				if _, ok := open[str(e.Data["id"])]; ok {
					delete(open, str(e.Data["id"]))
					add("Timeout", st("running", ""))
				}
			case "cancelled":
				add("Stop", nil)
				add("Cancel", st("stopped", ""))
				closed = true
			case "done":
				add("Finish", st("done", ""))
				closed = true
			}
		}
	}
	return steps
}

func init() { historyProjections["rules_prompt_approval"] = rpaHistory }

func TestRulesPromptApproval(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newRpaAdapter(t)
	if err := runMBT(t, "rules_prompt_approval", a, rpaActions, rpaOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("steps taken: %v", a.did)
	g, err := tracecheck.Load(fizzCheck(t, "rules_prompt_approval"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), rpaHistory)
	}
}

func rpaPaths(t *testing.T, cover tracecheck.Cover) [][]tracecheck.Step {
	t.Helper()
	b, err := pathsJSONCover("rules_prompt_approval", cover)
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		t.Fatal(err)
	}
	var out [][]tracecheck.Step
	for _, p := range f.Paths {
		out = append(out, p.Trace)
	}
	return out
}

// walkRpaPath drives one generated path and compares the whole state
// after every step; the first difference ends the path.
func walkRpaPath(a *rpaAdapter, path []tracecheck.Step) error {
	defer a.Cleanup()
	check := func(i int, want map[string]any) error {
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, path[i].Action, err)
		}
		gb, _ := json.Marshal(got)
		var g map[string]any
		json.Unmarshal(gb, &g)
		var bad []string
		for k, v := range want {
			field, ok := strings.CutPrefix(k, "Session#0.")
			if !ok {
				continue
			}
			if fmt.Sprint(g[field]) != fmt.Sprint(v) {
				bad = append(bad, fmt.Sprintf("%s = %v, want %v", field, g[field], v))
			}
		}
		if len(bad) > 0 {
			return fmt.Errorf("step %d (%s): %s\n  got  %s", i, path[i].Action, strings.Join(bad, "; "), gb)
		}
		return nil
	}
	if err := a.Init(); err != nil {
		return fmt.Errorf("Init: %w", err)
	}
	if err := check(0, path[0].State); err != nil {
		return err
	}
	for i := 1; i < len(path); i++ {
		name := strings.TrimPrefix(path[i].Action, "Session#0.")
		fn, ok := rpaActions["Session"][name]
		if !ok {
			return fmt.Errorf("step %d: no adapter action %q", i, name)
		}
		a.wantGrant = ""
		if name == "Grant" {
			for _, c := range rpaCalls {
				if path[i].State["Session#0."+c] == "open" {
					a.wantGrant = c
				}
			}
		}
		if _, err := fn(a, nil); err != nil {
			return fmt.Errorf("step %d (%s): %w", i, name, err)
		}
		if a.gate.off {
			return fmt.Errorf("step %d (%s): the adapter found it disabled", i, name)
		}
		if err := check(i, path[i].State); err != nil {
			return err
		}
	}
	return nil
}

func acts(path []tracecheck.Step) []string {
	var out []string
	for _, st := range path[1:] {
		out = append(out, strings.TrimPrefix(st.Action, "Session#0."))
	}
	return out
}

// TestRulesPromptApprovalPaths walks every derived path against real
// serves (four share them) and replays each transcript on the graph.
func TestRulesPromptApprovalPaths(t *testing.T) {
	t.Parallel()
	paths := rpaPaths(t, envCover())
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("rules_prompt_approval")), "..", "testdata", "rules_prompt_approval"))
	if err != nil {
		t.Fatal(err)
	}
	const shards = 4
	for s := range shards {
		t.Run(fmt.Sprint(s), func(t *testing.T) {
			t.Parallel()
			a := newRpaAdapter(t)
			for i := s; i < len(paths); i += shards {
				if err := walkRpaPath(a, paths[i]); err != nil {
					t.Errorf("path %d %v: %v", i, acts(paths[i]), err)
				}
			}
			t.Logf("steps taken: %v", a.did)
			for _, id := range a.ids {
				checkHistory(t, g, sessionHistory(t, a.s.Home, id), rpaHistory)
			}
		})
	}
}

// The path walk proves nothing unless a wrongly wired adapter fails it:
// a Timeout that expires some other question must be caught on the
// first Timeout transition a walk takes.
func TestRulesPromptApprovalPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newRpaAdapter(t)
	a.timeoutMisses = true
	for _, p := range rpaPaths(t, tracecheck.CoverTransitions) {
		cut := 0
		for i, st := range p {
			if st.Action == "Session#0.Timeout" {
				cut = i + 1
				break
			}
		}
		if cut == 0 {
			continue
		}
		err := walkRpaPath(a, p[:cut])
		switch {
		case err == nil:
			t.Fatalf("a walk whose Timeout expires no question passed: %v", acts(p[:cut]))
		case !strings.Contains(err.Error(), fmt.Sprintf("step %d (Session#0.Timeout)", cut-1)):
			t.Fatalf("the walk failed before its Timeout, so it says nothing about the adapter: %v: %v", acts(p[:cut]), err)
		}
		t.Logf("caught: %v", err)
		return
	}
	t.Fatal("no transitions walk takes Timeout")
}
