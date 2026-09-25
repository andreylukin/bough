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
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/secrets"
	"github.com/andreylukin/bough/internal/stepgate"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
)

// Event is the "loop/event" payload for kind "ask". The ui package
// normalizes payloads reflectively (eventOf), so the field names are
// contract.
type Event struct {
	Kind    string // "ask", or "ask/end" when it returned unanswered
	Text    string // the question
	ID      string
	Options []string
	Secret  bool // the answer is a credential: never echoed or recorded
	// Data is {"call": id} for a native ask or secret: the engine call
	// it blocks. Its siblings in the same reply end while it is open, so
	// only that call's end is the ask's; a code-mode ask (nil) ends with
	// its block's result.
	Data map[string]any
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
	// native lets one native ask or secret be open at a time; see
	// oneAtATime.
	native  chan struct{}
	timeout time.Duration
	code    codemode
	emit    func(Event)
	hist    appender // nil: no durable record
	project string   // session-project; "" in a local session
	// expireDir is BOUGH_TEST_ASK_EXPIRE_DIR: a file named for an open
	// ask's id there times that ask out now. Test use only: a model test
	// needs the timeout at a step it picks, and the real one is minutes.
	expireDir string
}

// pend is one blocked ask: its answer channel, and whether the answer
// is a secret that must never reach history.
type pend struct {
	ch     chan string
	secret bool
}

// userHome is a test seam, as in plugins/orb.
var userHome = os.UserHomeDir

// gate is the BOUGH_TEST_STEP_GATE hook, nil unless a model test set
// it: it holds an ask at each step a late answer can race, and notes
// the state no API shows (tests/model/specs/ask_timeout_vs_answer.fizz).
var gate = stepgate.Here()

var secretName = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_]*$`)

// ask is the tools.ask implementation. It records the question, emits
// the "ask" loop event for the UI, and blocks until Answer (or the
// timeout, which is an error the model sees as the tool failing).
func (a *Asker) ask(question string, options ...string) (string, error) {
	return a.put(question, false, options...)
}

// put asks one question from a codemode block; secret marks the answer
// as a credential.
func (a *Asker) put(question string, secret bool, options ...string) (string, error) {
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
	return a.putIn(done, park, "", question, secret, options...)
}

// putIn asks one question and blocks until the answer, the timeout, or
// done. park (nil for a native call, which holds no VM) is released for
// the wait. call is the native call's id, "" from a code block.
func (a *Asker) putIn(done <-chan struct{}, park func() func(), call, question string, secret bool, options ...string) (string, error) {
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
	gate.Note("pending", "true")
	gate.Note("call", "open")

	if a.hist != nil {
		data := map[string]any{"question": question, "options": options, "id": id}
		if secret {
			data["secret"] = true
		}
		if call != "" {
			data["call"] = call
		}
		a.hist.Append("ask", data)
	}
	ev := Event{Kind: "ask", Text: question, ID: id, Options: options, Secret: secret}
	if call != "" {
		ev.Data = map[string]any{"call": call}
	}
	a.emit(ev)

	if park != nil {
		defer park()()
	}
	timeout := time.After(a.timeout)
	if a.expireDir != "" {
		stop := make(chan struct{})
		defer close(stop)
		timeout = expireOn(filepath.Join(a.expireDir, id), timeout, stop)
	}
	answers := (<-chan string)(ch)
	if gate != nil {
		// Test hook: the select may take the answer only when the test
		// says, so it can fire the timeout with an answer already in ch.
		stop := make(chan struct{})
		defer close(stop)
		answers = stepgate.Relay(gate, "recv", ch, stop)
	}
	select {
	case <-done:
		a.giveUp(id, ch)
		a.ended(id, "Cancelled with no answer")
		return "", fmt.Errorf("ask: cancelled with no answer")
	case text, ok := <-answers:
		if !ok {
			a.ended(id, "Cancelled with no answer")
			return "", fmt.Errorf("ask: cancelled with no answer")
		}
		return text, nil
	case <-timeout:
		a.giveUp(id, ch)
		a.ended(id, fmt.Sprintf("No answer after %s", a.timeout))
		return "", fmt.Errorf("ask: no answer after %s", a.timeout)
	}
}

// giveUp is the done and timeout branches' delete, which runs only
// after the select chose them: an Answer in between still takes the
// entry and sends on ch, which nothing reads any more.
func (a *Asker) giveUp(id string, ch chan string) {
	gate.Note("call", "fired")
	gate.Hold("giveup")()
	a.mu.Lock()
	delete(a.pending, id)
	a.mu.Unlock()
	gate.Note("pending", "false")
	if gate != nil && len(ch) > 0 {
		gate.Note("unread", "true")
	}
}

// expireOn fires when timeout does or when file appears (consuming it),
// until stop closes. The poll only runs under BOUGH_TEST_ASK_EXPIRE_DIR.
func expireOn(file string, timeout <-chan time.Time, stop <-chan struct{}) <-chan time.Time {
	out := make(chan time.Time, 1)
	go func() {
		tick := time.NewTicker(10 * time.Millisecond)
		defer tick.Stop()
		for {
			select {
			case <-stop:
				return
			case now := <-timeout:
				out <- now
				return
			case now := <-tick.C:
				if os.Remove(file) == nil {
					out <- now
					return
				}
			}
		}
	}()
	return out
}

// testHold is the other half of the BOUGH_TEST_ASK_EXPIRE_DIR seam: an
// ask whose question contains <key> waits while hold-<stage>-<key> is in
// that dir, and fails without being put when the file says "skip". A
// model test uses it to make each of several calls the engine starts at
// once reach the person at a step it picks ("req", before the ask queues
// for the one answer slot) and to pick which queued ask gets the slot
// ("grant"). Test use only, like expireDir.
func (a *Asker) testHold(done <-chan struct{}, stage, question string) error {
	if a.expireDir == "" {
		return nil
	}
	for {
		held := false
		ents, _ := os.ReadDir(a.expireDir)
		for _, e := range ents {
			key, ok := strings.CutPrefix(e.Name(), "hold-"+stage+"-")
			if !ok || key == "" || !strings.Contains(question, key) {
				continue
			}
			if b, _ := os.ReadFile(filepath.Join(a.expireDir, e.Name())); string(b) == "skip" {
				return fmt.Errorf("ask: withdrawn before it was put")
			}
			held = true
		}
		if !held {
			return nil
		}
		select {
		case <-done:
			return fmt.Errorf("ask: cancelled with no answer")
		case <-time.After(10 * time.Millisecond):
		}
	}
}

// askSecret is tools.secret: ask the user for a credential, store it
// in the keychain and reference it from project.yml. The value is
// never returned, recorded or put in an error.
func (a *Asker) askSecret(name, reason string, project ...string) (string, error) {
	return a.secretVia(func(q string) (string, error) { return a.put(q, true) }, name, reason, project...)
}

// secretVia is askSecret with the question put through ask: a codemode
// block's put, or a native call's.
func (a *Asker) secretVia(ask func(question string) (string, error), name, reason string, project ...string) (string, error) {
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
	value, err := ask(fmt.Sprintf("Secret %s for %s: %s", name, slug, reason))
	if err != nil {
		return "", fmt.Errorf("secret: %w", err)
	}
	// A pasted value often carries a trailing space or newline.
	value = strings.TrimSpace(value)
	if value == "" || value == "(declined)" {
		return "", fmt.Errorf("secret: user declined")
	}
	gate.Note("call", "got")
	gate.Hold("store")()
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

// AskContext is Ask for a caller with a context of its own: a rule's
// approval of a bash call, from a block or an engine's native call.
// ctx's end (Stop) releases it, where Ask waits on the codemode run,
// which is Background outside a run_js. And it takes the one answer
// slot the native ask and secret share (oneAtATime): the engine runs a
// reply's calls at once, and an approval put up beside an open ask
// replaced it on screen, so the answer typed for the question shown
// went to the other one.
func (a *Asker) AskContext(ctx context.Context, question string, options ...string) (string, error) {
	// From a block, the call holds the VM: let go of it for the whole
	// wait, the queue included, as tools.ask does.
	if c, ok := a.code.(interface {
		Owned() bool
		Park() func()
	}); ok && c.Owned() {
		defer c.Park()()
	}
	if err := a.testHold(ctx.Done(), "req", question); err != nil {
		return "", err
	}
	release, err := a.oneAtATime(ctx.Done(), question)
	if err != nil {
		return "", err
	}
	defer release()
	return a.putIn(ctx.Done(), nil, question, false, options...)
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
	gate.Note("pending", "false")
	gate.Note("chan", "sent")
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
	gate.Note("pending", "false")
	gate.Note("chan", "closed")
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
		pending:   map[string]pend{},
		timeout:   timeout,
		code:      code,
		expireDir: os.Getenv("BOUGH_TEST_ASK_EXPIRE_DIR"),
		emit:      func(ev Event) { ctx.Emit("loop/event", ev) },
	}
	if h, err := kernel.Get[appender](ctx, "history"); err == nil {
		a.hist = h
		// Ids are unique in the session's history, not per Asker: a
		// remounted row or a respawned process continues after the asks
		// the transcript has. A restart at ask-1 let a draft kept for the
		// old ask-1 (the page keys it by id) answer a new question.
		if r, ok := h.(interface{ Entries() []history.Entry }); ok {
			a.seq = lastAskSeq(r.Entries())
		}
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
	if at, err := kernel.Get[agenttools.Registry](ctx, "agent-tools"); err == nil {
		off, err := agenttools.RegisterAll(at, a.nativeTools()...)
		if err != nil {
			return fmt.Errorf("ask: %w", err)
		}
		ctx.Effect(off)
	}
	// A disposed Asker can never be answered (the ui routes to the new
	// one), so its open asks end now rather than at the timeout: the
	// blocked call returns an error and its recorded end tells serve.
	ctx.Effect(a.cancelAll)
	ctx.Provide("ask-answers", a)
	return nil
}

// cancelAll fails every pending ask, as Cancel does one.
func (a *Asker) cancelAll() {
	a.mu.Lock()
	pending := a.pending
	a.pending = map[string]pend{}
	a.mu.Unlock()
	for _, p := range pending {
		close(p.ch)
	}
}

// lastAskSeq is the highest n of an "ask-n" id in entries.
func lastAskSeq(entries []history.Entry) int64 {
	var last int64
	for _, e := range entries {
		if e.Kind != "ask" {
			continue
		}
		id, _ := e.Data["id"].(string)
		if n, err := strconv.ParseInt(strings.TrimPrefix(id, "ask-"), 10, 64); err == nil && n > last {
			last = n
		}
	}
	return last
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

func (a *Asker) ended(id, text string) {
	if a.hist != nil {
		a.hist.Append("ask/end", map[string]any{"id": id, "text": text})
	}
	a.emit(Event{Kind: "ask/end", Text: text, ID: id})
}
