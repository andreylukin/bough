// Package title is the "session-title" plugin: a small model names the
// session and says in a few sentences what it is about, so `bough
// sessions`, the picker and the web sidebar read as a list of jobs rather
// than a list of opening sentences.
//
// A conversation drifts: the first message says "the gate is red", the
// tenth is shipping a refactor. So the name is not settled after the
// first turn but revisited as the session grows (turns 1, 3, 8, then
// every 10th), each time from everything the person asked plus the latest
// reply. Each naming is a "title" history entry carrying the summary and
// the turn it was written at; readers take the last one, and a resumed
// session carries on the schedule from it. That is a handful of cheap
// calls over a long session, never one per turn.
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

// Prompt asks for two labelled lines rather than JSON: small models break
// JSON far more often than they break a "Title:" prefix.
const Prompt = `You name coding sessions. Read what the user asked across the whole conversation (and the assistant's latest reply) and answer with exactly two lines:
Title: 3 to 7 words naming what the session is about overall, like a good branch name in prose — not just its first request. No quotes, no trailing period, no "session" or "task".
Summary: two or three plain sentences saying what the user is working on, what has been done, and where it stands now.
No preamble, nothing else.`

// maxInput bounds what the namer reads; maxPerInput keeps one pasted log
// from crowding out every other request in it.
const (
	maxInput    = 6000
	maxPerInput = 400
	maxSummary  = 400
)

// History is the seam: read the entries, append the title.
type History interface {
	Entries() []history.Entry
	Append(kind string, data map[string]any) history.Entry
}

// Titler names a session, and renames it as the conversation grows.
type Titler struct {
	llm  llm.LLM
	hist History
	emit func(kind, text string)
	ctx  context.Context

	mu      sync.Mutex
	running bool
}

// due reports whether a session that has finished turns turns, and was
// last named at turn named (0 = never), should be named now.
func due(turns, named int) bool {
	if turns <= named || turns == 0 {
		return false
	}
	for _, at := range []int{1, 3, 8} {
		if named < at && turns >= at {
			return true
		}
	}
	return turns >= 10 && turns/10 > named/10
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

// digest is what the namer reads: every request the person made, each cut
// short, and the latest reply. "" when nothing has been asked yet.
func digest(entries []history.Entry) string {
	var asks []string
	reply := ""
	for _, e := range entries {
		text, _ := e.Data["text"].(string)
		switch e.Kind {
		case "input":
			if t := strings.TrimSpace(text); t != "" {
				if r := []rune(t); len(r) > maxPerInput {
					t = string(r[:maxPerInput]) + "…"
				}
				asks = append(asks, "- "+t)
			}
		case "assistant":
			if strings.TrimSpace(text) != "" {
				reply = text
			}
		}
	}
	if len(asks) == 0 {
		return ""
	}
	// The newest requests matter most for where the session stands, so a
	// long session keeps its first request and as many recent ones as fit.
	body := strings.Join(asks, "\n")
	for len(body) > maxInput && len(asks) > 2 {
		asks = append(asks[:1], asks[2:]...)
		body = asks[0] + "\n…\n" + strings.Join(asks[1:], "\n")
	}
	out := "What the user asked, in order:\n" + body
	if reply != "" {
		if r := []rune(reply); len(r) > 1500 {
			reply = string(r[:1500]) + "…"
		}
		out += "\n\nThe assistant's latest reply:\n" + reply
	}
	return out
}

// schedule reads how many turns have finished and the turn the session
// was last named at.
func schedule(entries []history.Entry) (turns, named int) {
	for _, e := range entries {
		switch e.Kind {
		case "done":
			turns++
		case "title":
			// An entry from before titles recorded their turn was the
			// one-shot naming after the first turn.
			named = 1
			if n, ok := e.Data["turn"].(float64); ok {
				named = int(n)
			} else if n, ok := e.Data["turn"].(int); ok {
				named = n
			}
		}
	}
	return turns, named
}

// name runs one naming when the schedule says it is due, off the turn's
// goroutine; a naming already in flight is not doubled.
func (t *Titler) name() {
	t.mu.Lock()
	if t.running {
		t.mu.Unlock()
		return
	}
	t.running = true
	t.mu.Unlock()
	defer func() {
		t.mu.Lock()
		t.running = false
		t.mu.Unlock()
	}()

	entries := t.hist.Entries()
	turns, named := schedule(entries)
	if !due(turns, named) {
		return
	}
	input := digest(entries)
	if input == "" {
		return
	}
	ctx, cancel := context.WithTimeout(t.ctx, 30*time.Second)
	defer cancel()
	reply, err := t.llm.Complete(ctx, Prompt, []llm.Message{{Role: "user", Content: input}})
	if err != nil {
		return // an unnamed session still works; the first line stands in
	}
	title, summary := Parse(reply)
	if title == "" {
		return
	}
	data := map[string]any{"text": title, "turn": turns}
	if summary != "" {
		data["summary"] = summary
	}
	t.hist.Append("title", data)
	t.emit("title", title)
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
	t := &Titler{llm: l, hist: h, ctx: ctx}
	t.emit = func(kind, text string) {
		kctx.Emit("loop/event", loop.Event{Kind: kind, Text: text})
	}
	kctx.On("loop/event", func(p any) {
		if ev, ok := p.(loop.Event); ok && ev.Kind == "done" {
			go t.name()
		}
	})
	return nil
}
