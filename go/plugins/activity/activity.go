// Package activity is the "activity" plugin: while the agent works, a
// small model says what it is doing in a few words, and the ui puts
// that at the bottom of the screen. A transcript of collapsed rows
// tells you what happened; this tells you what is happening NOW,
// without expanding anything.
//
// One cheap call per program the agent runs (see llm.Small), skipped
// while a previous one is still in flight, and never on the agent's
// own model: without an llm-small row the plugin stays quiet rather
// than spending the conversation's model on a status line.
package activity

import (
	"context"
	"fmt"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"
	"github.com/andreylukin/bough/plugins/loop"
)

// Prompt asks for a label, not a sentence: it shares one line with the
// token counts.
const Prompt = `You are labelling what a coding agent is doing right now, for a status line.

Read the program it just started running and answer with 2 to 5 words, present participle, lower case, no full stop: what it is doing, in the user's terms.

Good: "reading the loop plugin", "running the test suite", "grepping for the flag", "writing the migration"
Bad: "executing code", "calling tools.bash", "performing an operation", anything longer than 5 words.

Answer with the label alone.`

// maxCode bounds what the labeller reads; the intent is at the top.
const maxCode = 1500

// timeout keeps a slow label from outliving the step it describes.
const timeout = 15 * time.Second

// Activity turns code events into status-line labels.
type Activity struct {
	llm  llm.LLM
	emit func(text string)
	ctx  context.Context

	mu   sync.Mutex
	busy bool
}

// Clean trims a small model's label down to what fits a status line
// (and unwraps a stop block, for a provider that answers every call
// the way it ends a turn).
func Clean(s string) string {
	if answer, ok := loop.StopAnswer(s); ok {
		s = answer
	}
	s = strings.TrimSpace(s)
	if i := strings.IndexByte(s, '\n'); i >= 0 {
		s = s[:i]
	}
	s = strings.Trim(s, ` "'*.`)
	if s == "" {
		return ""
	}
	if f := strings.Fields(s); len(f) > 6 {
		return "" // it wrote a sentence: better nothing than a wall
	}
	return strings.ToLower(s)
}

// label runs one call for one program.
func (a *Activity) label(code string) {
	a.mu.Lock()
	if a.busy {
		a.mu.Unlock()
		return // the last label has not landed; this step goes unnamed
	}
	a.busy = true
	a.mu.Unlock()
	defer func() {
		a.mu.Lock()
		a.busy = false
		a.mu.Unlock()
	}()

	if len(code) > maxCode {
		code = code[:maxCode]
	}
	ctx, cancel := context.WithTimeout(a.ctx, timeout)
	defer cancel()
	reply, err := a.llm.Complete(ctx, Prompt, []llm.Message{{Role: "user", Content: code}})
	if err != nil {
		return // a missing label is not worth an error row
	}
	if l := Clean(reply); l != "" {
		a.emit(l)
	}
}

type plugin struct{}

func init() {
	kernel.Register("activity", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "activity" }
func (plugin) Inject() []string { return []string{"llm"} }

func (plugin) Apply(kctx *kernel.Context, cfg map[string]any) error {
	for k := range cfg {
		return fmt.Errorf("activity: unknown config key %q", k)
	}
	// Deliberately NOT llm.Small's fallback: a status line is not worth
	// the agent's model, in money or in latency.
	l, err := kernel.Get[llm.LLM](kctx, llm.SmallKey)
	if err != nil {
		return nil // no small model: no labels, no complaint
	}
	ctx, cancel := context.WithCancel(context.Background())
	kctx.Effect(cancel)
	a := &Activity{llm: l, ctx: ctx}
	a.emit = func(text string) {
		kctx.Emit("loop/event", loop.Event{Kind: "activity", Text: text})
	}
	kctx.On("loop/event", func(p any) {
		ev, ok := p.(loop.Event)
		if !ok {
			return
		}
		switch ev.Kind {
		case "code":
			go a.label(ev.Text)
		case "done":
			a.emit("") // the turn is over; the line goes back to the usual
		}
	})
	return nil
}
