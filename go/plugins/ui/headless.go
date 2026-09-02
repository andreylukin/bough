package ui

import (
	"bufio"
	"errors"
	"fmt"
	"os"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/andreylukin/bough/plugins/commands"
)

// Headless mode reads lines from stdin into inputs and prints loop
// events as "[kind] text" on stdout. On stdin EOF it waits for
// in-flight runs to finish (a "done" event per sent line, with an idle
// timeout), then interrupts the process so the launcher unmounts and
// exits 0.
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
	hlAnswer  askAnswers // current mount's "ask-answers" service; nil = no asks
	hlAsk     *hlAskState
	hlPending atomic.Int64
	hlTick    = make(chan struct{}, 1)
)

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
func runHeadless(inputs chan<- string, b *broadcaster, cmds commandsView, hlog historyAppender, ask askAnswers) func() {
	events, _ := b.subscribe()
	go func() {
		for ev := range events {
			if ev.Kind == "ask" {
				// Arm the answer routing BEFORE printing, so a caller
				// that waits for the "[ask]" line can send the answer.
				hlMu.Lock()
				hlAsk = &hlAskState{id: ev.ID, options: ev.Options}
				hlMu.Unlock()
				fmt.Printf("[ask] %s\n", ev.Text)
				for i, o := range ev.Options {
					fmt.Printf("  %d. %s\n", i+1, o)
				}
				continue
			}
			if ev.Kind == "done" || ev.Kind == "error" {
				// The turn ended (or the ask timed out into a run
				// error): stop routing stdin to a dead ask.
				hlMu.Lock()
				hlAsk = nil
				hlMu.Unlock()
			}
			fmt.Printf("[%s] %s\n", ev.Kind, ev.Text)
			if ev.Kind == "done" {
				hlPending.Add(-1)
			}
			select {
			case hlTick <- struct{}{}:
			default:
			}
		}
	}()

	hlMu.Lock()
	hlInputs = inputs
	hlCmds = cmds
	hlHist = hlog
	hlAnswer = ask
	hlMu.Unlock()
	hlOnce.Do(func() { go headlessPump() })

	return func() {
		hlMu.Lock()
		hlInputs = nil
		hlCmds = nil
		hlHist = nil
		hlAnswer = nil
		hlMu.Unlock()
	}
}

func headlessPump() {
	sc := bufio.NewScanner(os.Stdin)
	sc.Buffer(make([]byte, 0, 64*1024), 1024*1024)
	for sc.Scan() {
		line := sc.Text()
		if hlAnswerPending(line) {
			continue // the line answered a pending tools.ask
		}
		if strings.HasPrefix(line, "/") && hlDispatch(line) {
			continue // dispatched: never reaches the loop/LLM
		}
		hlPending.Add(1)
		for {
			hlMu.Lock()
			ch := hlInputs
			if ch != nil {
				// Send under the lock: the disposer (which runs before the
				// loop row closes the channel) blocks until we finish.
				ch <- line
				hlMu.Unlock()
				break
			}
			hlMu.Unlock()
			// Mid-reload: wait for the remounted ui row to reattach.
			time.Sleep(50 * time.Millisecond)
		}
	}

	// EOF: drain until every sent line saw its "done", or events go idle.
	drainHeadless()
	interruptSelf()
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
		fmt.Printf("[error] %s\n", err)
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
	var act commands.UIAction
	switch {
	case errors.As(err, &act):
		out = "/" + name
	case err != nil:
		out = err.Error()
	case out == "":
		out = "/" + name
	}
	if hlog != nil {
		hlog.Append("system", map[string]any{"text": out})
	}
	fmt.Printf("[system] %s\n", out)
	if act == commands.ActionQuit {
		drainHeadless()
		interruptSelf()
	}
	return true
}

// drainHeadless waits for every sent line's "done" (with an idle
// timeout), so quitting never races an in-flight turn's output.
func drainHeadless() {
	for hlPending.Load() > 0 {
		select {
		case <-hlTick:
		case <-time.After(60 * time.Second):
			fmt.Fprintln(os.Stderr, "ui: headless: timed out waiting for loop to finish")
			hlPending.Store(0)
		}
	}
}
