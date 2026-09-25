package ui

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/andreylukin/bough/internal/linegate"
	"github.com/andreylukin/bough/internal/stepgate"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/llm"
)

// Headless mode reads lines from stdin into inputs and prints loop
// events as "[kind] text" on stdout. On stdin EOF it waits for
// in-flight runs to finish (a "done" event per sent line, with an idle
// timeout), then interrupts the process so the launcher unmounts and
// exits 0.
//
// A line that arrives while a turn runs STEERS it (the loop's "steer"
// service, when mounted): it lands at the turn's next boundary —
// between blocks, or right after the final reply — as a user message
// and the model is asked again, inside the same turn: one "[steer]"
// line, no extra "[done]", and the pending count is untouched (the
// turn's own done pays for it). A line the loop refuses (no turn, or
// the turn just took its last boundary) is sent as input and is a
// turn of its own, as before. Pipe prompts one at a time (wait for
// "[done]") when each must be its own turn.
//
// Stdin can only be read once per process, but hot reload can remount
// the ui row (with a fresh inputs channel), so the stdin pump is a
// process-wide singleton and the target channel is swapped per mount.
var (
	hlOnce    sync.Once
	hlMu      sync.Mutex
	hlInputs  chan<- string // current mount's inputs; nil while unmounted
	hlCmds    commandsView  // current mount's commands service; nil = "/" is plain text
	hlHist    historyAppender
	hlAnswer  askAnswers        // current mount's "ask-answers" service; nil = no asks
	hlSteer   func(string) bool // current mount's "steer" service; nil = mid-turn lines queue
	hlNotify  func(string) bool // routes a {"notice"} line to job-notices; false/nil = not mounted yet
	hlUsage   llm.UsageReporter // current mount's "usage" service; nil = no usage lines
	hlAsk     *hlAskState
	hlPending atomic.Int64
	hlEOF     atomic.Bool // stdin closed: no stdin line can answer an ask
	hlTick    = make(chan struct{}, 1)

	// hlErrored flips on the first "error" event; the launcher exits 1
	// after the clean unmount when any turn errored.
	hlErrored atomic.Bool
	// hlOut/hlErr are the event sinks: "[assistant]" and friends on
	// stdout, "[error]" on stderr. Vars so tests can capture them.
	hlOut io.Writer = &hlStdout{w: os.Stdout}
	hlErr io.Writer = os.Stderr

	// HeadlessJSON (--json) prints every event as one JSON object per
	// line ({"kind","text",...}) instead of "[kind] text", so a
	// multi-line text never spills onto continuation lines.
	HeadlessJSON bool
)

// hlDrain looks up the engine's "drain" key at stdin EOF (nil, or a nil
// result, when the loop row runs the loop).
var hlDrain func() func(context.Context) error

// hlStdout is headless stdout. Once a write fails (the reader went
// away: `bough --headless | head -1`), later writes are dropped so the
// run still finishes its turns and records them in history; the
// launcher then exits by SIGPIPE (see StdoutBroken).
type hlStdout struct {
	w      io.Writer
	broken atomic.Bool
}

func (o *hlStdout) Write(p []byte) (int, error) {
	if o.broken.Load() {
		return len(p), nil
	}
	if _, err := o.w.Write(p); err != nil {
		o.broken.Store(true)
	}
	return len(p), nil
}

// StdoutBroken reports whether a headless write to stdout failed.
func StdoutBroken() bool {
	o, ok := hlOut.(*hlStdout)
	return ok && o.broken.Load()
}

// hlLine writes one event line: "[kind] text" or, under HeadlessJSON,
// a JSON object carrying kind, text and extra.
func hlLine(w io.Writer, kind, text string, extra map[string]any) {
	if !HeadlessJSON {
		fmt.Fprintf(w, "[%s] %s\n", kind, text)
		return
	}
	obj := map[string]any{"kind": kind, "text": text}
	for k, v := range extra {
		obj[k] = v
	}
	b, _ := json.Marshal(obj)
	w.Write(append(b, '\n'))
}

// ExitCode is the process exit status the launcher should use after
// unmounting: 1 when a headless turn errored, else 0.
func ExitCode() int {
	if hlErrored.Load() {
		return 1
	}
	return 0
}

// Interrupted reports whether a headless turn is still in flight: an
// interrupt now is a cancel, not the self-interrupt that follows stdin
// EOF (that one drains every turn first). The launcher exits 130.
func Interrupted() bool {
	return hlPending.Load() > 0
}

// hlAskState is the pending tools.ask the next stdin line answers.
type hlAskState struct {
	id      string
	options []string
	secret  bool   // tools.secret: the line is a credential, taken raw and never printed
	call    string // the engine call a native ask blocks; "" from a code block
}

// runHeadless wires this mount's inputs and broadcaster into the pump,
// starts the pump on first call, and returns a disposer that detaches
// inputs so a reload never sends into a closed channel. The printer
// goroutine for a disposed mount leaks quietly (its broadcaster stops
// publishing); one idle goroutine per reload is accepted.
func runHeadless(inputs chan<- string, b *broadcaster, cmds commandsView, hlog historyAppender, ask askAnswers, steer func(string) bool, notify func(string) bool) func() {
	events, _ := b.subscribe()
	go func() {
		for ev := range events {
			hlPrint(ev)
		}
	}()

	hlMu.Lock()
	hlInputs = inputs
	hlCmds = cmds
	hlHist = hlog
	hlAnswer = ask
	hlSteer = steer
	hlNotify = notify
	hlMu.Unlock()
	hlOnce.Do(func() { go headlessPump() })

	return func() {
		hlMu.Lock()
		hlInputs = nil
		hlCmds = nil
		hlHist = nil
		hlAnswer = nil
		hlSteer = nil
		hlNotify = nil
		hlMu.Unlock()
	}
}

// hlPrint renders one loop event: "[ask]" arms answer routing before
// printing so a caller waiting on that line can answer; "[error]" goes
// to stderr and marks the run failed; everything else to stdout.
func hlPrint(ev Event) {
	switch ev.Kind {
	case "assistant-delta", "thinking-delta", "activity", "call-delta", "delta-reset":
		// "activity" is the small model's live label for what the turn is
		// doing ("" when it ends): a status line, same transport, same rule.
		// Fragments of a reply that is still forming. Plain headless is
		// read by humans and by the bench harness, so a token per line
		// would ruin it: drop them there, as before. Under --json each
		// one is its own object, tagged by kind, so `bough serve` can
		// stream a reply into the browser and a script can ignore them
		// on kind. They are never history, and the finished reply still
		// prints once as "[assistant]". An engine call's live output
		// (call-delta) and a reset of superseded stream text are the same
		// kind of fragment; they carry the call id / request seq a
		// reader needs, so their data rides along.
		if HeadlessJSON {
			var extra map[string]any
			if ev.Kind == "call-delta" || ev.Kind == "delta-reset" {
				extra = ev.Data
			}
			hlLine(hlOut, ev.Kind, ev.Text, extra)
		}
		return
	}
	switch ev.Kind {
	case "title", "context":
		// Bookkeeping around the turn, not the turn's output: a script
		// (and the benchmark harness) reads these lines as results.
		return
	}
	if ev.Kind == "ask" {
		hlMu.Lock()
		call, _ := ev.Data["call"].(string)
		hlAsk = &hlAskState{id: ev.ID, options: ev.Options, secret: ev.Secret, call: call}
		hlMu.Unlock()
		hlNote()
		if HeadlessJSON {
			extra := map[string]any{"id": ev.ID, "options": ev.Options}
			if ev.Secret {
				extra["secret"] = true
			}
			if call != "" {
				// serve's arm keys on it the same way (supervisor.go).
				extra["call"] = call
			}
			hlLine(hlOut, "ask", ev.Text, extra)
			return
		}
		fmt.Fprintf(hlOut, "[ask] %s\n", ev.Text)
		for i, o := range ev.Options {
			fmt.Fprintf(hlOut, "  %d. %s\n", i+1, o)
		}
		if hlEOF.Load() {
			hlCancelAsk()
		}
		return
	}
	if ev.Kind == "done" {
		// The turn ended: stop routing stdin to a dead ask. An "error" is
		// not an end: on the engine a sibling call's failure, a stderr
		// line or a refusal note lands while a native ask still waits, and
		// clearing on it made the next line a steer past the question. A
		// code-mode ask that errors ends with its block's result below.
		hlMu.Lock()
		hlAsk = nil
		hlMu.Unlock()
		hlNote()
	}
	hlMu.Lock()
	if hlAsk != nil && askEnded(ev, hlAsk.call) {
		// The ask returned with no answer (a timeout): the next line is
		// a steer again, not an answer Asker.Answer would refuse and lose.
		hlAsk = nil
	}
	hlMu.Unlock()
	hlNote()
	switch ev.Kind {
	case "error":
		// Held until the turn ends: a failed code block is followed by the
		// model trying again, and a run that recovered is not a failure (the
		// wiki ingest updated its pages and still exited 1).
		hlTurnErr.Store(true)
		hlLine(hlErr, "error", ev.Text, nil)
	case "call", "sub:call":
		// The call's record (tool, id, phase, ms, exit, add/del): serve
		// renders it, so it rides along. Plain output prints a call once,
		// at its recorded end: the live start carries the same text, and
		// the two lines could not be told apart.
		if !HeadlessJSON && ev.Data["phase"] == "start" {
			return
		}
		hlLine(hlOut, ev.Kind, ev.Text, ev.Data)
	case "result":
		// Only a later block that ran counts as recovery; a reply that
		// just gives up after the failure still ends the turn on it.
		hlTurnErr.Store(false)
		hlLine(hlOut, ev.Kind, ev.Text, nil)
	case "done":
		// serve keeps a background agent whose calls became jobs
		// (running > 0, or jobs > 0 from an earlier turn) in its slot,
		// and an interim close unreported; without these every close
		// read as the end.
		var extra map[string]any
		for _, k := range []string{"running", "jobs", "wake"} {
			if v, ok := ev.Data[k]; ok {
				if extra == nil {
					extra = map[string]any{}
				}
				extra[k] = v
			}
		}
		hlLine(hlOut, ev.Kind, ev.Text, extra)
	default:
		hlLine(hlOut, ev.Kind, ev.Text, nil)
	}
	if ev.Kind == "done" && hlTurnErr.Swap(false) {
		hlErrored.Store(true) // the turn ended on the error
		hlNote()
	}
	if ev.Kind == "done" {
		hlMu.Lock()
		u := hlUsage
		hlMu.Unlock()
		if u != nil {
			us := u.Usage()
			if HeadlessJSON {
				hlLine(hlOut, "usage", "", map[string]any{"input_tokens": us.InputTokens, "output_tokens": us.OutputTokens, "cost_usd": us.Cost, "priced": us.Priced})
			} else {
				fmt.Fprintf(hlOut, "[usage] {\"input_tokens\":%d,\"output_tokens\":%d,\"cost_usd\":%.6f,\"priced\":%t}\n",
					us.InputTokens, us.OutputTokens, us.Cost, us.Priced)
			}
		}
		// An engine wake turn (a call that outlived its turn finished)
		// was never a stdin line: its done pays for nothing, and taking
		// one off would end the drain before a line that is still running.
		if ev.Data["wake"] != true {
			hlPending.Add(-1)
		}
	}
	select {
	case hlTick <- struct{}{}:
	default:
	}
}

func headlessPump() {
	sc := bufio.NewScanner(os.Stdin)
	sc.Buffer(make([]byte, 0, 64*1024), 1024*1024)
	sc.Buffer(make([]byte, 1024*1024), 16*1024*1024) // a task brief can be long
	cwd, _ := os.Getwd()
	gate := linegate.Open("in", cwd)
	for n := 0; sc.Scan(); n++ {
		done := gate.Hold("")
		stepped := hlGate.Hold(fmt.Sprintf("in.%d", n))
		done(hlLineIn(sc.Text()))
		stepped()
	}

	// EOF: no line can answer an ask now, so fail a pending one (and
	// any that arms later) instead of blocking the turn until its
	// timeout. Then drain until every sent line saw its "done", or
	// events go idle.
	hlEOF.Store(true)
	hlCancelAsk()
	hlGate.Note("stdin", "eof")
	drainHeadless()
	drainEngine()
	interruptSelf()
}

// drainEngine waits, on the engine, for calls that outlived their turn
// and the wake turns they start: a one-shot run would otherwise exit
// with work the model never saw the end of. The idle guard is
// drainHeadless's: the wait is given up after BOUGH_HEADLESS_IDLE with
// no loop event. Background bash jobs are not waited for, as on the
// loop.
func drainEngine() {
	hlMu.Lock()
	get := hlDrain
	hlMu.Unlock()
	if get == nil {
		return
	}
	drain := get()
	if drain == nil {
		return
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan struct{})
	go func() {
		defer close(done)
		_ = drain(ctx)
	}()
	idle := hlIdleTimeout()
	t := time.NewTimer(idle)
	defer t.Stop()
	for {
		select {
		case <-done:
			return
		case <-hlTick:
			t.Reset(idle)
		case <-t.C:
			fmt.Fprintf(os.Stderr, "ui: headless: no loop event for %s while the engine drained, giving up (BOUGH_HEADLESS_IDLE)\n", idle)
			cancel()
			<-done
			return
		}
	}
}

// askEnded reports an event that means the pending ask has returned,
// answered or not: for a code-mode ask (call "") its block's result, as
// tools.ask blocks its block; for the engine's native ask or secret the
// recorded end of its own call, since a sibling run_js in the same reply
// records a result too. serve's StatusOf and its arm read the same
// events.
func askEnded(ev Event, call string) bool {
	if ev.Kind == "result" {
		return call == ""
	}
	if ev.Kind != "call" || ev.Data["phase"] == "start" {
		return false
	}
	t, _ := ev.Data["tool"].(string)
	id, _ := ev.Data["id"].(string)
	return (t == "ask" || t == "secret") && (call == "" || id == call)
}

// hlTyped is BOUGH_TYPED_ANSWERS, which serve sets on every child it
// runs: an answer then arrives as {"answer", "ask"} naming its question,
// and an untyped line is never an answer. Guarded by hlMu for tests.
var hlTyped = os.Getenv("BOUGH_TYPED_ANSWERS") != ""

// hlLineIn routes one stdin line and says where it went.
func hlLineIn(line string) string {
	// A typed answer goes to the question it names or nowhere: serve
	// wrote it while that question was armed, and the child may have
	// timed it out (and asked the next, a secret perhaps) since.
	hlMu.Lock()
	secret := hlAsk != nil && hlAsk.secret
	typed := hlTyped
	hlMu.Unlock()
	if id, text, ok := typedAnswer(line); typed && ok {
		if hlAnswerTo(id, text) {
			return "answer"
		}
		return "drop"
	}
	// A pending secret takes the raw line: no JSON sniffing, so a value
	// that happens to start with "{" is still the answer.
	if !typed && secret && hlAnswerPending(line) {
		return "answer"
	}
	// A JSON object line {"prompt": "..."} is one multi-line prompt:
	// the way a harness hands over a task brief with its newlines.
	// {"notice": "..."} is serve reporting a background agent.
	if strings.HasPrefix(line, "{") {
		var obj struct {
			Prompt string `json:"prompt"`
			Notice string `json:"notice"`
		}
		if err := json.Unmarshal([]byte(line), &obj); err == nil {
			// Before hlAnswerPending: a pending tools.ask would take the
			// notice as its answer. Not a prompt, so no done is owed.
			if obj.Notice != "" {
				hlNotice(obj.Notice)
				return "notice"
			}
			if obj.Prompt != "" {
				line = obj.Prompt
			}
		}
	}
	// Under serve an untyped line is a prompt even with a question
	// open: serve arms a question only once it reads the child's event,
	// so a /prompt it let through in that gap would be eaten as the
	// answer to a question the person never saw answered.
	if !typed && hlAnswerPending(line) {
		return "answer" // the line answered a pending tools.ask
	}
	if strings.HasPrefix(line, "/") && hlDispatch(line) {
		return "command" // dispatched: never reaches the loop/LLM
	}
	if strings.HasPrefix(line, "!") {
		hlBang(line)
		return "bang" // ran as a shell command: never reaches the loop/LLM
	}
	if hlSteerLine(line) {
		return "steer" // mid-turn: steered the running turn (its own done still ends it)
	}
	hlSubmit(line)
	return "input"
}

// hlNoticeWait bounds how long a notice waits for job-notices. serve
// counted the stdin write as delivered and stored nothing, so dropping
// in a reload gap loses the report for good; waiting forever would
// wedge the stdin pump of a session that has no job-notices at all.
var hlNoticeWait = 30 * time.Second

// hlNotice hands a notice to the job-notices service, which queues it
// and wakes an idle agent (or lands it before the next model step).
// Like hlSubmit it waits out a mid-reload gap.
func hlNotice(text string) {
	deadline := time.Now().Add(hlNoticeWait)
	for {
		hlMu.Lock()
		notify := hlNotify
		hlMu.Unlock()
		if notify != nil && notify(text) {
			return
		}
		if time.Now().After(deadline) {
			hlLine(hlErr, "error", "ui: headless: notice dropped: no job-notices service", nil)
			return
		}
		time.Sleep(50 * time.Millisecond)
	}
}

// hlSubmit sends one line to the loop as user input, waiting out a
// mid-reload gap until the remounted ui row reattaches. Always true.
func hlSubmit(line string) bool {
	hlPending.Add(1)
	for {
		hlMu.Lock()
		ch := hlInputs
		if ch != nil {
			// Send under the lock: the disposer (which runs before the
			// loop row closes the channel) blocks until we finish.
			ch <- line
			hlMu.Unlock()
			return true
		}
		hlMu.Unlock()
		// Mid-reload: wait for the remounted ui row to reattach.
		time.Sleep(50 * time.Millisecond)
	}
}

// hlSteerLine hands one stdin line to the turn in flight through the
// loop's "steer" service. False (send it as input instead) without
// the service or when no turn is running.
func hlSteerLine(line string) bool {
	hlMu.Lock()
	steer := hlSteer
	hlMu.Unlock()
	return steer != nil && steer(line)
}

// hlAnswerPending routes one stdin line to the pending tools.ask, if
// any: a bare number picks that option, anything else is the literal
// answer (same mapping as the composer). True when the line was
// consumed as an answer.
func hlAnswerPending(line string) bool { return hlAnswerTo("", line) }

// hlAnswerTo is hlAnswerPending for the pending ask only if it is id
// ("" takes whichever is pending).
func hlAnswerTo(id, line string) bool {
	hlMu.Lock()
	pa, ans := hlAsk, hlAnswer
	if pa != nil && id != "" && pa.id != id {
		hlMu.Unlock()
		return false
	}
	hlAsk = nil
	hlMu.Unlock()
	if pa != nil && ans == nil {
		// Mid-reload (the ui row is between dispose and remount): the
		// line is this ask's answer, so wait for the remounted router
		// like hlSubmit does. Falling through made it a prompt, and a
		// secret an input entry.
		ans = hlAwaitAnswerer()
	}
	if pa == nil || ans == nil {
		return false
	}
	text := line
	if n, err := strconv.Atoi(strings.TrimSpace(line)); err == nil && n >= 1 && n <= len(pa.options) {
		text = pa.options[n-1]
	}
	if err := ans.Answer(pa.id, text); err != nil {
		hlErrored.Store(true)
		hlGate.Note("refused", "true")
		hlLine(hlErr, "error", err.Error(), nil)
	}
	hlNote()
	return true
}

// hlAwaitAnswerer waits out a mid-reload gap and returns the remounted
// row's "ask-answers" service. A mounted row with none returns nil at
// once: nothing will ever answer.
func hlAwaitAnswerer() askAnswers {
	for {
		hlMu.Lock()
		mounted, ans := hlInputs != nil, hlAnswer
		hlMu.Unlock()
		if mounted {
			return ans
		}
		time.Sleep(50 * time.Millisecond)
	}
}

// typedAnswer reads serve's {"answer": text, "ask": id} line.
func typedAnswer(line string) (id, text string, ok bool) {
	if !strings.HasPrefix(line, "{") {
		return "", "", false
	}
	var obj struct {
		Answer *string `json:"answer"`
		Ask    string  `json:"ask"`
	}
	if json.Unmarshal([]byte(line), &obj) != nil || obj.Answer == nil || obj.Ask == "" {
		return "", "", false
	}
	return obj.Ask, *obj.Answer, true
}

// hlGate is the BOUGH_TEST_STEP_GATE hook, nil unless a model test set
// it (tests/model/specs/ask_timeout_vs_answer.fizz): it steps stdin a
// line at a time and notes hlAsk and hlErrored, which no API shows.
var hlGate = stepgate.Here()

// hlNote records hlAsk and hlErrored for the test hook.
func hlNote() {
	if hlGate == nil {
		return
	}
	hlMu.Lock()
	open := hlAsk != nil
	hlMu.Unlock()
	hlGate.Note("hlAsk", strconv.FormatBool(open))
	hlGate.Note("errored", strconv.FormatBool(hlErrored.Load()))
}

// hlTurnErr is an error the running turn has not recovered from yet: set
// by an "error" event, cleared by a later block's result, and turned into an
// errored run only if the turn's done arrives first.
var hlTurnErr atomic.Bool

// askCanceler is the optional Cancel half of the "ask-answers" service.
type askCanceler interface {
	Cancel(id string) error
}

// hlCancelAsk fails the pending tools.ask, if any, once stdin is
// closed: the run is marked errored (an unanswered question is not a
// clean success) and the blocked turn gets a tool error to finish on.
func hlCancelAsk() {
	hlMu.Lock()
	pa, ans := hlAsk, hlAnswer
	hlAsk = nil
	hlMu.Unlock()
	if pa == nil {
		return
	}
	hlErrored.Store(true)
	fmt.Fprintf(hlErr, "[error] stdin closed with tools.ask %s unanswered\n", pa.id)
	if c, ok := ans.(askCanceler); ok {
		c.Cancel(pa.id)
	}
	hlNote()
}

// hlDispatch runs a "/" line through the commands service, printing
// "[system] <output>" — the line never reaches the loop/LLM. False
// when no commands service is mounted ("/" is then plain text). The
// UI-owned actions have no UI here: quit stops the process like stdin
// EOF; the rest echo the command name as the notice (M27: output or a
// reason). Dispatches are recorded as "command"/"system" entries.
func hlDispatch(line string) bool {
	hlMu.Lock()
	cmds, hlog := hlCmds, hlHist
	hlMu.Unlock()
	if cmds == nil {
		return false
	}
	name, args, _ := strings.Cut(strings.TrimPrefix(line, "/"), " ")
	args = strings.TrimSpace(args)
	if hlog != nil {
		hlog.Append("command", map[string]any{"text": line})
	}
	out, err := cmds.Run(name, args)
	act, isAct := errors.AsType[commands.UIAction](err)
	switch {
	case isAct:
		out = "/" + name
		if target, cur, rows, ok := commands.ModelPickerChoices(act); ok {
			label := "model: "
			if target == "small" {
				label = "small model: "
			}
			out = label + cur + "\nchoices: " + strings.Join(rows, ", ")
		}
		if id, ok := commands.ResumeID(act); ok {
			// No session swap here: name the file so it can be resumed.
			out = "session " + id + " (bough --resume " + id + ")"
		}
	case err != nil:
		out = err.Error()
	case out == "":
		out = "/" + name
	}
	if hlog != nil {
		hlog.Append("system", map[string]any{"text": out})
	}
	hlLine(hlOut, "system", out, nil)
	if act == commands.ActionQuit {
		drainHeadless()
		interruptSelf()
	}
	if text, ok := commands.SubmitText(act); ok {
		return hlSubmit(text)
	}
	return true
}

// hlBang runs a "!" line directly as a shell command — never the
// loop/LLM — printing "[system] <output>" and recording the same
// "command"/"system" history entries the tui/web block pair gets.
func hlBang(line string) {
	hlMu.Lock()
	hlog := hlHist
	hlMu.Unlock()
	if hlog != nil {
		hlog.Append("command", map[string]any{"text": line})
	}
	out := runBang(bangCmd(line))
	if hlog != nil {
		hlog.Append("system", map[string]any{"text": out})
	}
	hlLine(hlOut, "system", out, nil)
}

// drainHeadless waits for every sent line's "done" (with an idle
// timeout), so quitting never races an in-flight turn's output.
func drainHeadless() {
	idle := hlIdleTimeout()
	for hlPending.Load() > 0 {
		select {
		case <-hlTick:
		case <-time.After(idle):
			fmt.Fprintf(os.Stderr, "ui: headless: no loop event for %s, giving up (BOUGH_HEADLESS_IDLE)\n", idle)
			hlPending.Store(0)
		}
	}
}

// AwaitCancelled waits (at most d) for a cancelled headless turn's
// closing "[cancelled]"/"[done]" lines to print, so a SIGINT exit
// never cuts the stream off before its terminal event.
func AwaitCancelled(d time.Duration) {
	deadline := time.After(d)
	for hlPending.Load() > 0 {
		select {
		case <-hlTick:
		case <-deadline:
			return
		}
	}
}

// hlIdleTimeout is how long the drain waits between loop events before
// it gives the turn up: BOUGH_HEADLESS_IDLE seconds, default 30 min. A
// long model call or a long tool run produces no event while it runs,
// so this is a hang guard, not a turn budget — the caller's own
// timeout is the budget.
func hlIdleTimeout() time.Duration {
	if s := os.Getenv("BOUGH_HEADLESS_IDLE"); s != "" {
		if n, err := strconv.Atoi(s); err == nil && n > 0 {
			return time.Duration(n) * time.Second
		}
	}
	return 30 * time.Minute
}
