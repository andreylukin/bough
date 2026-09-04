// Package ask is the "ask" plugin: tools.ask(question, options...)
// lets the model ask the user a question mid-code. The tool call
// BLOCKS until the UI (or headless stdin) answers via the
// "ask-answers" service, then returns the answer as the tool's normal
// return value — the model sees it as tool output. Both halves are
// durable history entries: kind "ask" {question, options, id} when
// asked, kind "ask/answer" {id, text} when answered.
package ask

import (
	"fmt"
	"sync"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
)

// Event is the "loop/event" payload for kind "ask". The ui package
// normalizes payloads reflectively (eventOf), so the field names are
// contract.
type Event struct {
	Kind    string // always "ask"
	Text    string // the question
	ID      string
	Options []string
}

// codemode is the slice of the "codemode" service we need: register
// the tool, and pause the script's interrupt timer while a call
// blocks on the user (an answer may take longer than the JS timeout).
type codemode interface {
	RegisterTool(name string, fn any)
	Pause() func()
}

// appender is the optional "history" service seam.
type appender interface {
	Append(kind string, data map[string]any) history.Entry
}

// defaultTimeout is how long an unanswered ask blocks before erroring
// (config: {timeout_minutes: N}).
const defaultTimeout = 10 * time.Minute

// Asker is the "ask-answers" service: the UI answers pending asks
// through Answer.
type Asker struct {
	mu      sync.Mutex
	seq     int64
	pending map[string]chan string
	timeout time.Duration
	code    codemode
	emit    func(Event)
	hist    appender // nil: no durable record
}

// ask is the tools.ask implementation. It records the question, emits
// the "ask" loop event for the UI, and blocks until Answer (or the
// timeout, which is an error the model sees as the tool failing).
func (a *Asker) ask(question string, options ...string) (string, error) {
	a.mu.Lock()
	a.seq++
	id := fmt.Sprintf("ask-%d", a.seq)
	ch := make(chan string, 1)
	a.pending[id] = ch
	a.mu.Unlock()

	if a.hist != nil {
		a.hist.Append("ask", map[string]any{"question": question, "options": options, "id": id})
	}
	a.emit(Event{Kind: "ask", Text: question, ID: id, Options: options})

	resume := a.code.Pause()
	defer resume()
	select {
	case text := <-ch:
		return text, nil
	case <-time.After(a.timeout):
		a.mu.Lock()
		delete(a.pending, id)
		a.mu.Unlock()
		return "", fmt.Errorf("ask: no answer after %s", a.timeout)
	}
}

// Answer resolves the pending ask id with text: the history gets an
// "ask/answer" entry and the blocked tool call returns text. An
// unknown (or already-resolved/timed-out) id is an error.
func (a *Asker) Answer(id, text string) error {
	a.mu.Lock()
	ch, ok := a.pending[id]
	delete(a.pending, id)
	a.mu.Unlock()
	if !ok {
		return fmt.Errorf("ask: no pending ask %q", id)
	}
	if a.hist != nil {
		a.hist.Append("ask/answer", map[string]any{"id": id, "text": text})
	}
	ch <- text // buffered: never blocks the UI
	return nil
}

type plugin struct{}

func init() {
	kernel.Register("ask", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "ask" }
func (plugin) Inject() []string { return []string{"codemode"} }

// Apply registers tools.ask and provides "ask-answers". Config:
// {timeout_minutes: int} (default 10). The "history" service is an
// optional seam, like the loop's.
func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	code, err := kernel.Get[codemode](ctx, "codemode")
	if err != nil {
		return err
	}
	timeout := defaultTimeout
	if v, has := cfg["timeout_minutes"]; has {
		n, ok := asInt(v)
		if !ok || n <= 0 {
			return fmt.Errorf("ask: timeout_minutes must be a positive integer, got %v", v)
		}
		timeout = time.Duration(n) * time.Minute
	}
	a := &Asker{
		pending: map[string]chan string{},
		timeout: timeout,
		code:    code,
		emit:    func(ev Event) { ctx.Emit("loop/event", ev) },
	}
	if h, err := kernel.Get[appender](ctx, "history"); err == nil {
		a.hist = h
	}
	// The loop documents tools.ask (and the separate-arguments nudge)
	// in its system prompt when it sees this "ask-answers" service —
	// NOT via a "cognition" provider here: two chaining cognition
	// providers (this plus todo's) Get+Provide the same single-slot
	// service and reload each other forever.
	code.RegisterTool("ask", a.ask)
	if d, ok := code.(interface{ Describe(name, line string) }); ok {
		d.Describe("ask", `tools.ask(question, ...options) -> string: ask the USER a question and block until they answer. Pass each option as a separate argument so they render as clickable choices.`)
	}
	ctx.Provide("ask-answers", a)
	return nil
}

// asInt accepts the integer shapes YAML and JS configs arrive as.
func asInt(v any) (int, bool) {
	switch n := v.(type) {
	case int:
		return n, true
	case int64:
		return int(n), true
	case float64:
		if n == float64(int(n)) {
			return int(n), true
		}
	}
	return 0, false
}
