// Package title is the "session-title" plugin: a small model keeps a
// running log of the session, one caveman line per finished turn, and
// names the session from that log, so `bough sessions`, the picker and
// the web sidebar read as a list of jobs rather than a list of opening
// sentences.
//
// Every finished turn (done, cancelled or failed) appends a
// "turn-summary" entry {text, turn}. The first one also gives a new
// session a provisional name from its first line. The real name — a
// "title" entry {text, summary, turn, final: true} written from the whole
// log — comes when the session goes quiet past its model's prompt-cache
// window (the person has walked away; nothing is being saved by waiting)
// or when the session shuts down with turns logged since the last
// naming. Readers take the last title entry. Neither kind is ever
// projected into model context.
//
// This is the other half of the llm-small row (see llm.Small): the
// canonical small-model job in every harness that has one.
package title

import (
	"context"
	"fmt"
	"strings"
	"sync"
	"time"
	"unicode"

	"github.com/charmbracelet/x/ansi"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
	"github.com/andreylukin/bough/plugins/loop"
)

const maxSummary = 400

// History is the seam: read the entries, append the log and the title.
type History interface {
	Entries() []history.Entry
	Append(kind string, data map[string]any) history.Entry
}

// Titler keeps a session's running log and names the session from it.
type Titler struct {
	llm  llm.LLM
	hist History
	emit func(kind, text string)
	ctx  context.Context
	// ttl is the main model's cache window, read when a turn ends (the
	// model can change mid-session); after schedules the quiet check and
	// returns its stop. Both are seams for tests.
	ttl   func() time.Duration
	after func(time.Duration, func()) (stop func() bool)

	work   sync.Mutex // one model call sequence at a time: never a line twice
	tmu    sync.Mutex
	timer  func() bool
	closed bool          // under tmu: shut down, arm nothing more
	cancel func()        // cancels ctx: shutdown frees an in-flight call
	grace  time.Duration // shutdown's whole budget; 0 means 10s
}

// Clean trims what a small model tends to wrap around a title — and a
// stop block, for a provider that answers every call the way it ends a
// turn.
func Clean(s string) string {
	if answer, ok := loop.StopAnswer(s); ok {
		s = answer
	}
	s = strings.TrimSpace(s)
	if i := strings.IndexByte(s, '\n'); i >= 0 {
		s = s[:i] // a chatty model explains underneath; take the name
	}
	s = strings.TrimSpace(strings.TrimPrefix(s, "Title:"))
	// The name lands in the terminal's OSC 2 title and the status bar:
	// drop escape sequences whole, then any stray control or bidi rune.
	s = oneLine(s)
	s = strings.Trim(s, ` "'*.`)
	if r := []rune(s); len(r) > 60 {
		s = strings.TrimSpace(string(r[:60])) + "…" // runes: a byte cut splits one
	}
	return s
}

// oneLine strips escapes, control and bidi runes, and collapses spaces.
func oneLine(s string) string {
	s = strings.Map(func(r rune) rune {
		if unicode.IsControl(r) || unicode.Is(unicode.Bidi_Control, r) {
			return -1
		}
		return r
	}, ansi.Strip(s))
	return strings.Join(strings.Fields(s), " ")
}

// Parse takes the title and summary out of a reply. A model that ignored
// the format and answered with a bare name still yields that name.
func Parse(reply string) (title, summary string) {
	if answer, ok := loop.StopAnswer(reply); ok {
		reply = answer
	}
	var rest []string
	for _, l := range strings.Split(reply, "\n") {
		t := strings.TrimSpace(strings.Trim(strings.TrimSpace(l), "*"))
		switch {
		case strings.HasPrefix(t, "Title:"):
			title = Clean(strings.TrimPrefix(t, "Title:"))
		case strings.HasPrefix(t, "Summary:"):
			summary = strings.TrimSpace(strings.TrimPrefix(t, "Summary:"))
		case summary != "" && t != "":
			rest = append(rest, t) // a summary that wrapped onto more lines
		}
	}
	if len(rest) > 0 {
		summary += " " + strings.Join(rest, " ")
	}
	if title == "" {
		title = Clean(reply)
	}
	summary = oneLine(summary)
	if r := []rune(summary); len(r) > maxSummary {
		summary = strings.TrimSpace(string(r[:maxSummary])) + "…"
	}
	return title, summary
}

// call runs one small-model completion under a bound.
func call(ctx context.Context, l llm.LLM, system, input string) (string, error) {
	ctx, cancel := context.WithTimeout(ctx, 60*time.Second)
	defer cancel()
	return l.Complete(ctx, system, []llm.Message{{Role: "user", Content: input}})
}

// logTurns writes the log line of every finished turn not yet logged.
// A session that predates the log (or was backfilled only in part) gets
// just its latest turn: catching up a long history is `bough summarize`'s
// job, not a turn's. Caller holds work.
func (t *Titler) logTurns(ctx context.Context) {
	entries := t.hist.Entries()
	ts := Turns(entries)
	lines, logged := turnLog(entries)
	named := false
	for _, e := range entries {
		named = named || e.Kind == "title"
	}
	for _, n := range pending(ts, logged) {
		reply, err := call(ctx, t.llm, TurnPrompt, TurnInput(lines, ts[n-1]))
		if err != nil {
			return // the next turn picks it up
		}
		line := CleanLine(reply)
		if line == "" {
			return
		}
		t.hist.Append("turn-summary", map[string]any{"text": line, "turn": n})
		lines = append(lines, fmt.Sprintf("%d. %s", n, line))
		if n == 1 && !named {
			// A new session is never untitled while it waits for its name.
			if name := provisional(line); name != "" {
				t.hist.Append("title", map[string]any{"text": name, "turn": 1})
				t.emit("title", name)
				named = true
			}
		}
	}
}

// finalName names the session from its whole log, when turns were
// logged since the last final naming. Caller holds work.
func (t *Titler) finalName(ctx context.Context) {
	entries := t.hist.Entries()
	lines, logged := turnLog(entries)
	if logged == 0 || logged <= lastFinal(entries) {
		return
	}
	reply, err := call(ctx, t.llm, FinalPrompt, "Running log:\n"+strings.Join(lines, "\n"))
	if err != nil {
		return // the provisional (or an older) name stands
	}
	title, summary := Parse(reply)
	if title == "" {
		return
	}
	data := map[string]any{"text": title, "turn": logged, "final": true}
	if summary != "" {
		data["summary"] = summary
	}
	t.hist.Append("title", data)
	t.emit("title", title)
}

// turnDone logs the turn off the turn's goroutine, then restarts the
// quiet timer.
func (t *Titler) turnDone() {
	t.work.Lock()
	t.logTurns(t.ctx)
	t.work.Unlock()
	t.tmu.Lock()
	defer t.tmu.Unlock()
	if t.closed {
		return
	}
	if t.timer != nil {
		t.timer()
	}
	t.timer = t.after(t.ttl(), t.quiet)
}

// quiet fires when nothing happened for a cache window after a turn. A
// turn that has started since (an input with no end yet) cancels it:
// that turn's end restarts the timer.
func (t *Titler) quiet() {
	t.tmu.Lock()
	closed := t.closed
	t.tmu.Unlock()
	if closed {
		return
	}
	if ts := Turns(t.hist.Entries()); len(ts) > 0 && ts[len(ts)-1].End == "" {
		return
	}
	t.work.Lock()
	defer t.work.Unlock()
	t.finalName(t.ctx)
}

// shutdown names the session one last time before it closes, all of it
// under one short deadline: an in-flight turn call is cancelled first so
// the close never waits out a model.
func (t *Titler) shutdown() {
	t.tmu.Lock()
	t.closed = true
	if t.timer != nil {
		t.timer()
		t.timer = nil
	}
	t.tmu.Unlock()
	if t.cancel != nil {
		t.cancel()
	}
	grace := t.grace
	if grace == 0 {
		grace = 10 * time.Second
	}
	ctx, cancel := context.WithTimeout(context.Background(), grace)
	defer cancel()
	t.work.Lock()
	defer t.work.Unlock()
	t.logTurns(ctx)
	t.finalName(ctx)
}

type plugin struct{}

func init() {
	kernel.Register("session-title", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "session-title" }
func (plugin) Inject() []string { return []string{"llm", "history"} }

func (plugin) Apply(kctx *kernel.Context, cfg map[string]any) error {
	for k := range cfg {
		return fmt.Errorf("session-title: unknown config key %q", k)
	}
	l, _ := llm.Small(kctx)
	if l == nil {
		return fmt.Errorf("session-title: no llm service")
	}
	h, err := kernel.Get[History](kctx, "history")
	if err != nil {
		return fmt.Errorf("session-title: needs the history service")
	}
	ctx, cancel := context.WithCancel(context.Background())
	kctx.Effect(cancel)
	t := &Titler{llm: l, hist: h, ctx: ctx, cancel: cancel,
		ttl: func() time.Duration {
			main, _ := kernel.Get[llm.LLM](kctx, "llm")
			if main == nil {
				return llm.CacheTTL("")
			}
			return llm.CacheTTL(llm.Name(main))
		},
		after: func(d time.Duration, f func()) func() bool { return time.AfterFunc(d, f).Stop },
	}
	t.emit = func(kind, text string) {
		kctx.Emit("loop/event", loop.Event{Kind: kind, Text: text})
	}
	kctx.On("loop/event", func(p any) {
		if ev, ok := p.(loop.Event); ok && ev.Kind == "done" {
			go t.turnDone()
		}
	})
	// Registered after cancel, so it runs first (LIFO), and before
	// history closes (rows unmount in reverse). It cancels ctx itself and
	// runs its own work under a fresh short deadline.
	kctx.Effect(t.shutdown)
	return nil
}
