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
// service, when mounted): it lands at the turn's next block boundary
// as a user message and the model is asked again, inside the same
// turn — one "[steer]" line, no extra "[done]". A line that arrives
// between turns is a turn of its own, as before. Pipe prompts one at a
// time (wait for "[done]") when each must be its own turn.
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
	hlTick    = make(chan struct{}, 1)

	// hlErrored flips on the first "error" event; the launcher exits 1
	// after the clean unmount when any turn errored.
	hlErrored atomic.Bool
	// hlOut/hlErr are the event sinks: "[assistant]" and friends on
	// stdout, "[error]" on stderr. Vars so tests can capture them.
	hlOut io.Writer = os.Stdout
	hlErr io.Writer = os.Stderr
)

// ExitCode is the process exit status the launcher should use after
// unmounting: 1 when a headless turn errored, else 0.
func ExitCode() int {
	if hlErrored.Load() {
		return 1
	}
	return 0
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
	if ev.Kind == "ask" {
		hlMu.Lock()
		hlAsk = &hlAskState{id: ev.ID, options: ev.Options}
		hlMu.Unlock()
		fmt.Fprintf(hlOut, "[ask] %s\n", ev.Text)
		for i, o := range ev.Options {
			fmt.Fprintf(hlOut, "  %d. %s\n", i+1, o)
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
		fmt.Fprintf(hlErr, "[error] %s\n", ev.Text)
	} else {
		fmt.Fprintf(hlOut, "[%s] %s\n", ev.Kind, ev.Text)
	}
	if ev.Kind == "done" {
		hlMu.Lock()
		u := hlUsage
		hlMu.Unlock()
		if u != nil {
			us := u.Usage()
			fmt.Fprintf(hlOut, "[usage] {\"input_tokens\":%d,\"output_tokens\":%d,\"cost_usd\":%.6f,\"priced\":%t}\n",
				us.InputTokens, us.OutputTokens, us.Cost, us.Priced)
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

	// EOF: drain until every sent line saw its "done", or events go idle.
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
		fmt.Fprintf(hlErr, "[error] %s\n", err)
	}
	return true
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
		if cur, rows, ok := commands.ModelPickerChoices(act); ok {
			out = "model: " + cur + "\nchoices: " + strings.Join(rows, ", ")
		}
	case err != nil:
		out = err.Error()
	case out == "":
		out = "/" + name
	}
	if hlog != nil {
		hlog.Append("system", map[string]any{"text": out})
	}
	fmt.Fprintf(hlOut, "[system] %s\n", out)
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
	fmt.Fprintf(hlOut, "[system] %s\n", out)
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
