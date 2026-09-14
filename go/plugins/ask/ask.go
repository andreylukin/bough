// Package ask is the "ask" plugin: tools.ask(question, options...)
// lets the model ask the user a question mid-code. The tool call
// BLOCKS until the UI (or headless stdin) answers via the
// "ask-answers" service, then returns the answer as the tool's normal
// return value — the model sees it as tool output. Both halves are
// durable history entries: kind "ask" {question, options, id} when
// asked, kind "ask/answer" {id, text} when answered.
package ask

import (
	"context"
	"fmt"
	"os"
	"regexp"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/secrets"
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
	Secret  bool // the answer is a credential: never echoed or recorded
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
	pending map[string]pend
	timeout time.Duration
	code    codemode
	emit    func(Event)
	hist    appender // nil: no durable record
	project string   // session-project; "" in a local session
}

// pend is one blocked ask: its answer channel, and whether the answer
// is a secret that must never reach history.
type pend struct {
	ch     chan string
	secret bool
}

// userHome is a test seam, as in plugins/orb.
var userHome = os.UserHomeDir

var secretName = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_]*$`)

// ask is the tools.ask implementation. It records the question, emits
// the "ask" loop event for the UI, and blocks until Answer (or the
// timeout, which is an error the model sees as the tool failing).
func (a *Asker) ask(question string, options ...string) (string, error) {
	return a.put(question, false, options...)
}

// put asks one question; secret marks the answer as a credential.
func (a *Asker) put(question string, secret bool, options ...string) (string, error) {
	if strings.TrimSpace(question) == "" {
		return "", fmt.Errorf("ask: question is empty")
	}
	kept := options[:0:0]
	for _, o := range options {
		if strings.TrimSpace(o) != "" {
			kept = append(kept, o)
		}
	}
	options = kept
	a.mu.Lock()
	a.seq++
	id := fmt.Sprintf("ask-%d", a.seq)
	ch := make(chan string, 1)
	a.pending[id] = pend{ch: ch, secret: secret}
	a.mu.Unlock()

	if a.hist != nil {
		data := map[string]any{"question": question, "options": options, "id": id}
		if secret {
			data["secret"] = true
		}
		a.hist.Append("ask", data)
	}
	a.emit(Event{Kind: "ask", Text: question, ID: id, Options: options, Secret: secret})

	// The run's context: a cancelled turn (ctrl+c) must release the
	// blocked call — goja cannot interrupt a Go host call.
	done := context.Background().Done()
	if rc, ok := a.code.(interface{ RunContext() context.Context }); ok {
		done = rc.RunContext().Done()
	}
	// Park (codemode) also frees the VM while the user thinks: a
	// /model swap remounts rows that register tools on it.
	park := a.code.Pause
	if p, ok := a.code.(interface{ Park() func() }); ok {
		park = p.Park
	}
	defer park()()
	select {
	case <-done:
		a.mu.Lock()
		delete(a.pending, id)
		a.mu.Unlock()
		return "", fmt.Errorf("ask: cancelled with no answer")
	case text, ok := <-ch:
		if !ok {
			return "", fmt.Errorf("ask: cancelled with no answer")
		}
		return text, nil
	case <-time.After(a.timeout):
		a.mu.Lock()
		delete(a.pending, id)
		a.mu.Unlock()
		return "", fmt.Errorf("ask: no answer after %s", a.timeout)
	}
}

// askSecret is tools.secret: ask the user for a credential, store it
// in the keychain and reference it from project.yml. The value is
// never returned, recorded or put in an error.
func (a *Asker) askSecret(name, reason string, project ...string) (string, error) {
	if !secretName.MatchString(name) {
		return "", fmt.Errorf("secret: invalid name %q", name)
	}
	slug := a.project
	if len(project) > 0 {
		slug = project[0]
	}
	if slug == "" {
		return "", fmt.Errorf("secret: pass the project slug")
	}
	home, err := userHome()
	if err != nil {
		return "", fmt.Errorf("secret: home dir: %w", err)
	}
	if _, err := projectdef.Load(home, slug); err != nil {
		return "", fmt.Errorf("secret: %w", err)
	}
	value, err := a.put(fmt.Sprintf("Secret %s for %s: %s", name, slug, reason), true)
	if err != nil {
		return "", fmt.Errorf("secret: %w", err)
	}
	// A pasted value often carries a trailing space or newline.
	value = strings.TrimSpace(value)
	if value == "" || value == "(declined)" {
		return "", fmt.Errorf("secret: user declined")
	}
	service := secrets.Service(slug, name)
	if err := secrets.Store(service, value); err != nil {
		return "", fmt.Errorf("secret: store failed: %s", scrub(err.Error(), value))
	}
	ref := secrets.Ref(service)
	if err := projectdef.SetSecret(home, slug, name, ref); err != nil {
		return "", fmt.Errorf("secret: %s", scrub(err.Error(), value))
	}
	if a.project != slug {
		return fmt.Sprintf("stored %s as %s; applies to project %s's next command or session", name, ref, slug), nil
	}
	return fmt.Sprintf("stored %s as %s; available to the next command", name, ref), nil
}

// scrub is belt and braces: an error from a lower layer must not carry
// the value even if that layer slips.
func scrub(msg, value string) string {
	return strings.ReplaceAll(msg, value, "[secret]")
}

// Ask is ask for other rows: a question the harness itself has to put
// to the user (a Codex rule that says "prompt" before a command).
func (a *Asker) Ask(question string, options ...string) (string, error) {
	return a.ask(question, options...)
}

// Answer resolves the pending ask id with text: the history gets an
// "ask/answer" entry and the blocked tool call returns text. An
// unknown (or already-resolved/timed-out) id is an error.
func (a *Asker) Answer(id, text string) error {
	a.mu.Lock()
	p, ok := a.pending[id]
	delete(a.pending, id)
	a.mu.Unlock()
	if !ok {
		return fmt.Errorf("ask: no pending ask %q", id)
	}
	if a.hist != nil {
		if p.secret {
			a.hist.Append("ask/answer", map[string]any{"id": id, "text": "[secret stored]", "secret": true})
		} else {
			a.hist.Append("ask/answer", map[string]any{"id": id, "text": text})
		}
	}
	p.ch <- text // buffered: never blocks the UI; the raw value reaches askSecret only
	return nil
}

// Cancel fails the pending ask id: the blocked tool call returns an
// error now instead of waiting out the timeout (headless stdin closed,
// so no answer can come). An unknown id is an error.
func (a *Asker) Cancel(id string) error {
	a.mu.Lock()
	p, ok := a.pending[id]
	delete(a.pending, id)
	a.mu.Unlock()
	if !ok {
		return fmt.Errorf("ask: no pending ask %q", id)
	}
	close(p.ch)
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
		pending: map[string]pend{},
		timeout: timeout,
		code:    code,
		emit:    func(ev Event) { ctx.Emit("loop/event", ev) },
	}
	if h, err := kernel.Get[appender](ctx, "history"); err == nil {
		a.hist = h
	}
	a.project, _ = kernel.Get[string](ctx, "session-project")
	// The loop documents tools.ask (and the separate-arguments nudge)
	// in its system prompt when it sees this "ask-answers" service —
	// NOT via a "cognition" provider here: two chaining cognition
	// providers (this plus todo's) Get+Provide the same single-slot
	// service and reload each other forever.
	code.RegisterTool("ask", a.ask)
	if d, ok := code.(interface{ Describe(name, line string) }); ok {
		d.Describe("ask", `tools.ask(question, ...options) -> string: ask the USER a question and block until they answer. Pass each option as a separate argument so they render as clickable choices.`)
	}
	code.RegisterTool("secret", a.askSecret)
	if d, ok := code.(interface{ Describe(name, line string) }); ok {
		d.Describe("secret", `tools.secret(name, reason, project?) asks the user for a credential, stores it in the keychain and adds it to project.yml secrets. The value is not returned to you, but commands see it as env, so never print it.`)
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
