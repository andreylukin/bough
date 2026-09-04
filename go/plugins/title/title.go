// Package title is the "session-title" plugin: after the first turn a
// small model names the session, so `bough sessions` and the picker
// read as a list of jobs rather than a list of opening sentences. The
// name is a "title" history entry, so it survives a resume and costs
// one cheap call per session (never per turn).
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

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
	"github.com/andreylukin/bough/plugins/loop"
)

// Prompt asks for a name, not a summary: the picker shows one line.
const Prompt = `Name this coding session in 3 to 6 words, like a good branch name in prose: what the user is trying to do. No quotes, no trailing period, no "session" or "task", no preamble — answer with the title alone.`

// maxInput bounds what the namer reads; the ask is in the first lines.
const maxInput = 2000

// History is the seam: read the entries, append the title.
type History interface {
	Entries() []history.Entry
	Append(kind string, data map[string]any) history.Entry
}

// Titler names a session once.
type Titler struct {
	llm  llm.LLM
	hist History
	emit func(kind, text string)
	ctx  context.Context

	mu   sync.Mutex
	done bool
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
	s = strings.Trim(s, ` "'*.`)
	if len(s) > 60 {
		s = strings.TrimSpace(s[:60]) + "…"
	}
	return s
}

// firstInput is the user's opening message, "" when there is none.
func firstInput(entries []history.Entry) string {
	for _, e := range entries {
		if e.Kind == "input" {
			text, _ := e.Data["text"].(string)
			return text
		}
		if e.Kind == "title" {
			return "" // already named (a resumed session)
		}
	}
	return ""
}

// name runs the one call, off the turn's goroutine.
func (t *Titler) name() {
	t.mu.Lock()
	if t.done {
		t.mu.Unlock()
		return
	}
	t.done = true
	t.mu.Unlock()

	entries := t.hist.Entries()
	for _, e := range entries {
		if e.Kind == "title" {
			return // resumed: it has a name already
		}
	}
	input := firstInput(entries)
	if strings.TrimSpace(input) == "" {
		return
	}
	if len(input) > maxInput {
		input = input[:maxInput]
	}
	ctx, cancel := context.WithTimeout(t.ctx, 30*time.Second)
	defer cancel()
	reply, err := t.llm.Complete(ctx, Prompt, []llm.Message{{Role: "user", Content: input}})
	if err != nil {
		return // an unnamed session still works; the first line stands in
	}
	if title := Clean(reply); title != "" {
		t.hist.Append("title", map[string]any{"text": title})
		t.emit("title", title)
	}
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
