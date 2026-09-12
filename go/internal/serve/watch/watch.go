// Package watch runs the watchers that let an outside event wake a
// session: a .js file says what to run and how often, and what change
// in that command's output is worth interrupting the agent for. The
// engine here is pure — no HTTP, no supervisor — so the safety rules
// (dedupe, rate limit, never interrupt a busy session) are testable
// against fakes.
package watch

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"
)

// Evaluator runs a watcher file body as function(event){...}.
// plugins/codemode.CodeMode satisfies this; a bare VM with no tools is
// enough, because a watcher only inspects the event it is handed.
type Evaluator interface {
	RunHook(ctx context.Context, body string, event map[string]any) (map[string]any, error)
}

// Execer runs the watcher's shell command and returns its stdout.
type Execer interface {
	Run(ctx context.Context, cmd string) (string, error)
}

// Waker delivers a wake to a session. It is only ever called for a
// session the Idler says is idle.
type Waker interface {
	Wake(session, text string) error
}

// Idler answers whether a session is mid-turn. A busy session is never
// interrupted: its wake waits in the queue until this returns false.
type Idler interface {
	Idle(session string) bool
}

// WatcherStatus is one row of the "watchers" list in GET /api/hooks.
type WatcherStatus struct {
	Name     string     `json:"name"`
	Path     string     `json:"path"`
	Every    string     `json:"every"`
	Failing  bool       `json:"failing"`
	Error    string     `json:"error"`
	LastRun  *time.Time `json:"lastRun"`
	LastWoke *time.Time `json:"lastWoke"`
}

// watcher is one loaded file plus everything the safety rules need to
// remember about it between polls.
type watcher struct {
	name, path, body string
	every            time.Duration
	run              string

	failing bool
	err     string

	lastRun  *time.Time
	lastWoke *time.Time
	due      time.Time

	prev     any    // last parsed command output, handed back as event.prev
	lastText string // last wake text, for dedupe
	queued   []string
}

// Engine loads watchers from a directory and polls them. Callers drive
// it with Tick so tests can move the clock instead of sleeping; Run is
// the production wrapper that ticks on a timer.
type Engine struct {
	Dir     string
	Session string

	Eval Evaluator
	Exec Execer
	Wake Waker
	Busy Idler

	// Now is the clock. Tests replace it; zero means time.Now.
	Now func() time.Time
	// MinGap is the per-watcher wake floor: a flapping check cannot
	// wake the session more often than this.
	MinGap time.Duration

	mu sync.Mutex
	ws []*watcher
}

func (e *Engine) now() time.Time {
	if e.Now != nil {
		return e.Now()
	}
	return time.Now()
}

func (e *Engine) gap() time.Duration {
	if e.MinGap > 0 {
		return e.MinGap
	}
	return 5 * time.Minute
}

// ErrNotLoopback is why the engine refuses to run: watchers execute
// shell commands and serve has no auth, so a server reachable from off
// the machine must not offer them.
var ErrNotLoopback = errors.New("watch: refusing to run watchers, server is not bound to loopback")

// CheckLoopback reports whether addr ("host:port" as passed to
// net.Listen) is loopback-only.
func CheckLoopback(addr string) error {
	host, _, err := net.SplitHostPort(addr)
	if err != nil {
		host = addr
	}
	// An empty host ("" from ":7684") binds every interface, so it is
	// the opposite of loopback-only, not a shorthand for it.
	if host == "localhost" {
		return nil
	}
	ip := net.ParseIP(host)
	if ip == nil || !ip.IsLoopback() {
		return fmt.Errorf("%w (%s)", ErrNotLoopback, addr)
	}
	return nil
}

// Load reads *.js from Dir and bootstraps each with phase:"config".
// A file whose config is unusable stays in the list as failing with the
// reason, because a watcher that silently vanished is worse than one
// that says why it is broken.
func (e *Engine) Load(ctx context.Context) error {
	names, err := filepath.Glob(filepath.Join(e.Dir, "*.js"))
	if err != nil {
		return err
	}
	sort.Strings(names)
	now := e.now()

	var ws []*watcher
	for _, path := range names {
		body, err := os.ReadFile(path)
		w := &watcher{name: filepath.Base(path), path: path, body: string(body)}
		if err != nil {
			w.failing, w.err = true, err.Error()
			ws = append(ws, w)
			continue
		}
		if err := e.bootstrap(ctx, w); err != nil {
			w.failing, w.err = true, err.Error()
		} else {
			w.due = now
		}
		ws = append(ws, w)
	}

	e.mu.Lock()
	e.ws = ws
	e.mu.Unlock()
	return nil
}

func (e *Engine) bootstrap(ctx context.Context, w *watcher) error {
	cfg, err := e.Eval.RunHook(ctx, w.body, map[string]any{"phase": "config"})
	if err != nil {
		return err
	}
	if cfg == nil {
		return errors.New(`phase "config" returned nothing, want {every, run}`)
	}
	every, _ := cfg["every"].(string)
	d, err := time.ParseDuration(every)
	if err != nil {
		return fmt.Errorf("bad every %q: %w", every, err)
	}
	if d <= 0 {
		return fmt.Errorf("bad every %q: must be positive", every)
	}
	run, _ := cfg["run"].(string)
	if strings.TrimSpace(run) == "" {
		return errors.New("config has no run command")
	}
	w.every, w.run = d, run
	return nil
}

// Tick polls every watcher that is due and delivers whatever is queued
// for an idle session. It is safe to call as often as the caller likes;
// nothing happens before a watcher's interval has elapsed.
func (e *Engine) Tick(ctx context.Context) {
	e.mu.Lock()
	ws := append([]*watcher(nil), e.ws...)
	e.mu.Unlock()

	for _, w := range ws {
		e.mu.Lock()
		due := w.every > 0 && !e.now().Before(w.due)
		e.mu.Unlock()
		if due {
			e.poll(ctx, w)
		}
	}
	e.deliver()
}

func (e *Engine) poll(ctx context.Context, w *watcher) {
	out, runErr := e.Exec.Run(ctx, w.run)

	e.mu.Lock()
	now := e.now()
	at := now
	w.lastRun = &at
	w.due = now.Add(w.every)
	if runErr != nil {
		w.failing, w.err = true, runErr.Error()
		e.mu.Unlock()
		return
	}
	prev := w.prev
	e.mu.Unlock()

	res, evalErr := e.Eval.RunHook(ctx, w.body, map[string]any{
		"phase": "check", "prev": prev, "now": parse(out),
	})

	e.mu.Lock()
	defer e.mu.Unlock()
	if evalErr != nil {
		w.failing, w.err = true, evalErr.Error()
		return
	}
	w.failing, w.err = false, ""
	w.prev = parse(out)

	text, _ := res["wake"].(string)
	if strings.TrimSpace(text) == "" {
		return
	}
	if text == w.lastText { // dedupe: the same news is not news twice
		return
	}
	if w.lastWoke != nil && e.now().Sub(*w.lastWoke) < e.gap() {
		return // rate limit: a flapping check may not wake forever
	}
	w.lastText = text
	w.queued = append(w.queued, text)
}

// deliver hands queued wakes to the session once it is idle. A busy
// session keeps its queue; nothing is dropped and nothing interrupts.
func (e *Engine) deliver() {
	if e.Busy != nil && !e.Busy.Idle(e.Session) {
		return
	}
	e.mu.Lock()
	type pending struct {
		w    *watcher
		text string
	}
	var out []pending
	for _, w := range e.ws {
		for _, t := range w.queued {
			out = append(out, pending{w, t})
		}
		w.queued = nil
	}
	e.mu.Unlock()

	for _, p := range out {
		if err := e.Wake.Wake(e.Session, p.text); err != nil {
			e.mu.Lock()
			p.w.failing, p.w.err = true, err.Error()
			e.mu.Unlock()
			continue
		}
		e.mu.Lock()
		at := e.now()
		p.w.lastWoke = &at
		e.mu.Unlock()
	}
}

// Run polls until ctx is done. It refuses to start unless the server is
// bound to loopback, because a watcher is arbitrary shell.
func (e *Engine) Run(ctx context.Context, addr string, every time.Duration) error {
	if err := CheckLoopback(addr); err != nil {
		return err
	}
	if err := e.Load(ctx); err != nil {
		return err
	}
	t := time.NewTicker(every)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-t.C:
			e.Tick(ctx)
		}
	}
}

// Status is the "watchers" list of GET /api/hooks, always non-nil.
func (e *Engine) Status() []WatcherStatus {
	e.mu.Lock()
	defer e.mu.Unlock()
	out := make([]WatcherStatus, 0, len(e.ws))
	for _, w := range e.ws {
		every := ""
		if w.every > 0 {
			every = w.every.String()
		}
		out = append(out, WatcherStatus{
			Name: w.name, Path: w.path, Every: every,
			Failing: w.failing, Error: w.err,
			LastRun: w.lastRun, LastWoke: w.lastWoke,
		})
	}
	return out
}

// parse turns the command's stdout into event.now: JSON when it parses,
// the raw string otherwise, so a watcher over `echo ok` still works.
func parse(out string) any {
	var v any
	if err := json.Unmarshal([]byte(out), &v); err == nil {
		return v
	}
	return out
}
