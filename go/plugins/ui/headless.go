package ui

import (
	"bufio"
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
	hlOut io.Writer = os.Stdout
	hlErr io.Writer = os.Stderr

	// HeadlessJSON (--json) prints every event as one JSON object per
	// line ({"kind","text",...}) instead of "[kind] text", so a
	// multi-line text never spills onto continuation lines.
	HeadlessJSON bool
)

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
}

// runHeadless wires this mount's inputs and broadcaster into the pump,
// starts the pump on first call, and returns a disposer that detaches
// inputs so a reload never sends into a closed channel. The printer
// goroutine for a disposed mount leaks quietly (its broadcaster stops
// publishing); one idle goroutine per reload is accepted.
func runHeadless(inputs chan<- string, b *broadcaster, cmds commandsView, hlog historyAppender, ask askAnswers, steer func(string) bool) func() {
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
	hlMu.Unlock()
	hlOnce.Do(func() { go headlessPump() })

	return func() {
		hlMu.Lock()
		hlInputs = nil
		hlCmds = nil
		hlHist = nil
		hlAnswer = nil
		hlSteer = nil
		hlMu.Unlock()
	}
}

// hlPrint renders one loop event: "[ask]" arms answer routing before
// printing so a caller waiting on that line can answer; "[error]" goes
// to stderr and marks the run failed; everything else to stdout.
func hlPrint(ev Event) {
	if ev.Kind == "assistant-delta" {
		return // the whole reply prints once as "[assistant]"
	}
	switch ev.Kind {
	case "title", "context", "thinking-delta", "activity":
		// Bookkeeping around the turn, not the turn's output: a script
		// (and the benchmark harness) reads these lines as results.
		return
	}
	if ev.Kind == "ask" {
		hlMu.Lock()
		hlAsk = &hlAskState{id: ev.ID, options: ev.Options}
		hlMu.Unlock()
		if HeadlessJSON {
			hlLine(hlOut, "ask", ev.Text, map[string]any{"id": ev.ID, "options": ev.Options})
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
	if ev.Kind == "done" || ev.Kind == "error" {
		// The turn ended (or the ask timed out into a run error):
		// stop routing stdin to a dead ask.
		hlMu.Lock()
		hlAsk = nil
		hlMu.Unlock()
	}
	if ev.Kind == "error" {
		hlErrored.Store(true)
		hlLine(hlErr, "error", ev.Text, nil)
	} else {
		hlLine(hlOut, ev.Kind, ev.Text, nil)
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
		hlPending.Add(-1)
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
	for sc.Scan() {
		line := sc.Text()
		// A JSON object line {"prompt": "..."} is one multi-line prompt:
		// the way a harness hands over a task brief with its newlines.
		if strings.HasPrefix(line, "{") {
			var obj struct {
				Prompt string `json:"prompt"`
			}
			if err := json.Unmarshal([]byte(line), &obj); err == nil && obj.Prompt != "" {
				line = obj.Prompt
			}
		}
		if hlAnswerPending(line) {
			continue // the line answered a pending tools.ask
		}
		if strings.HasPrefix(line, "/") && hlDispatch(line) {
			continue // dispatched: never reaches the loop/LLM
		}
		if strings.HasPrefix(line, "!") {
			hlBang(line)
			continue // ran as a shell command: never reaches the loop/LLM
		}
		if hlSteerLine(line) {
			continue // mid-turn: steered the running turn (its own done still ends it)
		}
		hlSubmit(line)
	}

	// EOF: no line can answer an ask now, so fail a pending one (and
	// any that arms later) instead of blocking the turn until its
	// timeout. Then drain until every sent line saw its "done", or
	// events go idle.
	hlEOF.Store(true)
	hlCancelAsk()
	drainHeadless()
	interruptSelf()
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
func hlAnswerPending(line string) bool {
	hlMu.Lock()
	pa, ans := hlAsk, hlAnswer
	hlAsk = nil
	hlMu.Unlock()
	if pa == nil || ans == nil {
		return false
	}
	text := line
	if n, err := strconv.Atoi(strings.TrimSpace(line)); err == nil && n >= 1 && n <= len(pa.options) {
		text = pa.options[n-1]
	}
	if err := ans.Answer(pa.id, text); err != nil {
		hlErrored.Store(true)
		hlLine(hlErr, "error", err.Error(), nil)
	}
	return true
}

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
