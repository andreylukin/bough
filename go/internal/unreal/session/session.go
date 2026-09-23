//go:build !windows

// Package session is one bough session on the unreal-agent harness
// (go/docs/unreal-engine.md §2, §9, §12): the store, the operation
// manager, the coordinator, the Gate the coordinator sees as its model,
// and the actor that turns what the harness records into bough's turns.
//
// A harness "turn" is one model request; a bough turn is input → done
// and may span many of them, because every tool call runs
// asynchronously and wakes the model when it finishes. Everything here
// exists to keep exactly one `done` per bough turn while that happens:
// one actor goroutine sees every change in one order, a pure mirror of
// the coordinator's own scheduling rule says when it has nothing left to
// do, and the Gate lets a cancel stop the model without ending Run.
package session

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/unreallabsai/unreal-agent/harness/operation"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore/localfile"
	"github.com/unreallabsai/unreal-agent/harness/tool"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/schema"
	"github.com/andreylukin/bough/internal/unreal/ops"
	"github.com/andreylukin/bough/internal/unreal/project"
	"github.com/andreylukin/bough/internal/unreal/prompt"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

// Config is the engine row's config (§6.1), parsed by plugins/engine.
type Config struct {
	Tools           string        // "native" | "both" (both adds run_js)
	TurnSettle      time.Duration // idle wait on foreground calls before they become jobs
	Heartbeat       time.Duration // harness ToolHeartbeatInterval; 0 = off
	CallTimeout     time.Duration // deadline for every bough.call
	MaxOutput       int           // bytes the model reads per result
	RowOutput       int           // bytes of output kept on a call history entry
	MaxSteps        int           // model requests per bough turn
	MaxCostUSD      float64       // 0 = off
	StopRetries     int           // stop-schema misses re-asked per turn
	SteerInterrupts bool          // a steer cancels the in-flight request
	SystemPrompt    string        // replaces the engine preamble
	TaskGuidance    string        // guidance text; "" = off
	Trace           bool          // request/response bodies → <store>/trace/<sid>.jsonl
}

// Defaults are §6.1's.
func (c Config) withDefaults() Config {
	if c.Tools == "" {
		c.Tools = "native"
	}
	if c.TurnSettle <= 0 {
		c.TurnSettle = 60 * time.Second
	}
	if c.CallTimeout <= 0 {
		c.CallTimeout = 10 * time.Minute
	}
	if c.MaxOutput <= 0 {
		c.MaxOutput = 40000
	}
	if c.RowOutput <= 0 {
		c.RowOutput = 8192
	}
	if c.MaxSteps <= 0 {
		c.MaxSteps = 100
	}
	return c
}

// Deps is everything the session reads from the kernel. Services arrive
// as funcs read at the moment they are needed, never held: a Get during
// the row's Apply would make each one a remount edge.
type Deps struct {
	SessionID   string // bough session id
	HistoryPath string
	Store       string // store dir; "" = ~/.bough/engine
	Scratch     func() string
	Cwd         string
	Config      Config

	LLM         func() (agentllm.Source, string, error) // lazy "llm" (else an error naming the row)
	Usage       func() llm.Usage                        // lazy "usage", else llm's UsageReporter
	Tools       agenttools.Registry
	Hooks       func() agenttools.Hooks // may return nil
	Lifecycle   func() Lifecycle        // may return nil
	Prompt      func() prompt.Parts     // everything but Preamble/Guidance, read lazily
	Orb         func() (root string, ok bool)
	Redact      func(string) string
	Jobs        func() Jobs
	Stats       func() TurnStats
	Checkpoints func() Checkpointer
	Skills      func() Skills
	History     History
	Emit        func(kind, text string, data map[string]any)

	// Tests only: override what toolreg/boughcall/project would build.
	Registry  func(sid string) (tool.Registry, []operation.RemoteJobHandler)
	Projector func(prefix, worker string) *project.Projector

	// Schema is the lazy "stop-schema" key: the shape a turn's final
	// reply must have. Additive to the frozen §5.7 set.
	Schema func() schema.Schema
}

type Lifecycle interface {
	SessionStart(ctx context.Context) (context string)
	PromptSubmit(ctx context.Context, text string) (out string, block string, contexts []string)
	Stop(ctx context.Context, reply string) (continueWith string)
	Drain() []map[string]any // hook fire records → "hook" entries
}

// Jobs is tools-basic's job-notices service plus Adopt (§8.4). An Adopt
// that returns id 0 means the row cannot number engine calls; the actor
// then records the job entries itself.
type Jobs interface {
	Take() []string
	Wake() <-chan struct{}
	Adopt(cmd, call string, kill func()) (id int, finish func(exit *int, stopped bool))
}

type TurnStats interface {
	Take() (files []string, exit int, ran bool)
}

type Checkpointer interface {
	Snapshot() string
	Pin(seq int64, tree string)
	Changed(before string) []string
}

type Skills interface{ Inject(input string) []string }

type History interface {
	Append(kind string, data map[string]any) history.Entry
	Entries() []history.Entry
	Path() string
}

type Children interface {
	Run(ctx context.Context, req ChildRequest) (ChildResult, error)
}

type ChildRequest struct {
	Task, Worker string
	System       string // extra system text (the worker section)
	MaxSteps     int
	Allow        []string // tool allowlist; nil = parent tools minus spawn, agent, stop_agent, ask, secret
}

type ChildResult struct {
	Reply  string
	Status string // done | budget | cancelled | error
	Steps  int
}

// SID is the harness session id for a bough session id: localfile
// accepts only [A-Za-z0-9-], so every other byte becomes '-'. UUIDv7 ids
// pass through; the fallback 20060102T150405.000Z-pid loses its dot.
func SID(boughID string) string {
	b := []byte(boughID)
	for i, c := range b {
		if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '-') {
			b[i] = '-'
		}
	}
	return string(b)
}

// DefaultStore is where the harness stores live when the row does not
// say: under $HOME, so a test's temp HOME keeps them hermetic.
func DefaultStore() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return filepath.Join(".bough", "engine")
	}
	return filepath.Join(home, ".bough", "engine")
}

// Runtime is one bough session. It outlives coordinator restarts and
// engine-row remounts; Close ends it.
type Runtime struct {
	d   Deps
	cfg Config
	sid string

	ctx    context.Context // the session's: the store, ops and actor live on it
	cancel context.CancelFunc

	store *localfile.Store
	ops   *ops.Manager
	gate  *Gate
	sync  *syncMirror

	// handlers are the remote job handlers the operation manager routes
	// to; closed with the session.
	handlers []operation.RemoteJobHandler
	testReg  tool.Registry

	q      *fifo
	exited chan struct{} // the actor goroutine has returned

	// The rest belongs to the actor goroutine.
	a actorState

	ctxMu   sync.Mutex
	ctxText string // /context, refreshed by the actor at each build

	// steerable mirrors "a turn is open and not cancelling" for Steer,
	// which must answer without waiting on the actor; submits counts
	// Submit lines posted and not yet taken.
	steerable atomic.Bool
	submits   atomic.Int64

	closeOnce sync.Once
}

// Open resumes, forks or seeds the harness session behind d and catches
// its history up with the store. No coordinator is built: that waits for
// the next input, so opening an interrupted session never calls the
// model on its own.
func Open(ctx context.Context, d Deps) (*Runtime, error) {
	if d.History == nil {
		return nil, errors.New("engine-unreal: no history service")
	}
	if d.Emit == nil {
		d.Emit = func(string, string, map[string]any) {}
	}
	if d.Store == "" {
		d.Store = DefaultStore()
	}
	if d.SessionID == "" {
		d.SessionID = strings.TrimSuffix(filepath.Base(d.History.Path()), ".jsonl")
	}
	if d.HistoryPath == "" {
		d.HistoryPath = d.History.Path()
	}
	cfg := d.Config.withDefaults()
	store, err := localfile.New(d.Store)
	if err != nil {
		return nil, fmt.Errorf("engine-unreal: session store: %w", err)
	}
	sctx, cancel := context.WithCancel(context.Background())
	r := &Runtime{
		d:      d,
		cfg:    cfg,
		sid:    SID(d.SessionID),
		ctx:    sctx,
		cancel: cancel,
		store:  store,
		sync:   &syncMirror{m: newMirror()},
		q:      newFIFO(),
		exited: make(chan struct{}),
	}
	r.a.init(r)
	r.gate = newGate(r, "", nil)
	r.ops = r.newOps(sctx, "")
	// The observer is added once, before any Run: it only applies the
	// sync mirror and queues the item, so a slow history write can never
	// stall the coordinator goroutine it runs on.
	store.AddObserver(r.observe)
	if err := r.open(ctx); err != nil {
		cancel()
		return nil, err
	}
	go r.loop()
	go r.watch()
	return r, nil
}

// newOps builds the session-lifetime operation manager around the
// bough.call handler (or the test registry's handlers).
func (r *Runtime) newOps(ctx context.Context, worker string) *ops.Manager {
	var hs []operation.RemoteJobHandler
	if r.d.Registry != nil && worker == "" {
		reg, handlers := r.d.Registry(r.sid)
		r.testReg = reg
		hs = handlers
	} else if worker == "" {
		hs = []operation.RemoteJobHandler{r.newHandler(worker, func(id, text string) {
			r.post(func() { r.a.onProgress(id, text) })
		})}
	}
	r.handlers = append(r.handlers, hs...)
	return ops.New(ctx, operation.NewLocalOperationManager(ctx, hs...), nil)
}

// SessionID is the harness session id.
func (r *Runtime) SessionID() string { return r.sid }

// StorePath is the harness store file of this session.
func (r *Runtime) StorePath() string {
	return filepath.Join(r.d.Store, r.sid+".session.jsonl")
}

// Submit is one line from the inputs chan: a turn of its own, queued
// behind the turn in flight like the loop's serial input.
func (r *Runtime) Submit(line string) {
	r.submits.Add(1)
	if !r.post(func() { r.submits.Add(-1); r.a.submit(line) }) {
		r.submits.Add(-1)
	}
}

// Steer hands text to the bough turn in flight; false when none is open
// or about to be (the ui then sends the line as input).
//
// It answers from a flag and never waits on the actor: the ui calls it
// inside Update on Enter, and the actor runs hooks and git snapshots
// that would freeze the terminal meanwhile (the loop's Steer only
// enqueues too). A submit still on its way counts as a turn about to
// open, so a steer sent right after one lands on it, as it did when
// Steer queued behind the submit. A steer that finds the turn already
// closed when the actor gets to it becomes an input, which is what the
// ui would have done with a false.
func (r *Runtime) Steer(text string) bool {
	if !r.steerable.Load() && r.submits.Load() == 0 {
		return false
	}
	return r.post(func() {
		if !r.a.steer(text) {
			r.a.submit(text)
		}
	})
}

// Cancel is Esc / ctrl+c / SIGINT: the turn in flight ends with
// cancelled + done; adopted jobs and background bash keep running.
func (r *Runtime) Cancel() {
	r.post(func() { r.a.cancelTurn("") })
}

// Drain returns once no turn is open, no call is outstanding (adopted
// included), no request is in flight and no input is queued, or when ctx
// ends. Headless calls it at stdin EOF so adopted calls and their wake
// turns finish before the process exits.
func (r *Runtime) Drain(ctx context.Context) error {
	done := make(chan struct{})
	var once sync.Once
	if !r.post(func() { r.a.addDrain(func() { once.Do(func() { close(done) }) }) }) {
		return nil
	}
	select {
	case <-done:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	case <-r.exited:
		return nil
	}
}

// CancelCall cancels one call's operations: /jobkill of an adopted job,
// and the engine key's CancelCall.
func (r *Runtime) CancelCall(callID string) error {
	reply := make(chan error, 1)
	if !r.post(func() { reply <- r.a.cancelCall(callID) }) {
		return errors.New("engine-unreal: session closed")
	}
	select {
	case err := <-reply:
		return err
	case <-r.exited:
		return errors.New("engine-unreal: session closed")
	}
}

// Context is what /context prints.
func (r *Runtime) Context() string {
	reply := make(chan string, 1)
	if r.post(func() { reply <- r.a.contextText() }) {
		select {
		case s := <-reply:
			return s
		case <-time.After(2 * time.Second):
		case <-r.exited:
		}
	}
	r.ctxMu.Lock()
	defer r.ctxMu.Unlock()
	return r.ctxText
}

// Children runs subagents on child coordinators (§12.4).
func (r *Runtime) Children() Children { return children{r: r} }

// Close flushes the projection, records the end of a turn still open,
// stops Run and ends the session ctx.
func (r *Runtime) Close(ctx context.Context) error {
	r.closeOnce.Do(func() {
		done := make(chan struct{})
		if r.post(func() { r.a.shutdown(); close(done) }) {
			select {
			case <-done:
			case <-ctx.Done():
			}
		}
		r.q.close()
		select {
		case <-r.exited:
		case <-ctx.Done():
		}
		r.cancel()
		for _, h := range r.handlers {
			if c, ok := h.(interface{ Close() error }); ok {
				_ = c.Close()
			}
		}
	})
	return nil
}

// post queues f for the actor; false once the session is closed.
func (r *Runtime) post(f func()) bool { return r.q.push(f) }

func (r *Runtime) loop() {
	defer close(r.exited)
	for {
		fs, ok := r.q.pop(r.ctx)
		if !ok {
			return
		}
		for _, f := range fs {
			f()
			r.a.evaluate()
			r.steerable.Store(r.a.open && r.a.cancelling == nil && !r.a.closed)
		}
	}
}

// fifo is the actor's unbounded queue: the observer runs on the
// coordinator goroutine and must never block on the actor.
type fifo struct {
	mu     sync.Mutex
	q      []func()
	sig    chan struct{}
	closed bool
}

func newFIFO() *fifo { return &fifo{sig: make(chan struct{}, 1)} }

func (f *fifo) push(fn func()) bool {
	f.mu.Lock()
	if f.closed {
		f.mu.Unlock()
		return false
	}
	f.q = append(f.q, fn)
	f.mu.Unlock()
	select {
	case f.sig <- struct{}{}:
	default:
	}
	return true
}

func (f *fifo) close() {
	f.mu.Lock()
	f.closed = true
	f.mu.Unlock()
	select {
	case f.sig <- struct{}{}:
	default:
	}
}

// pop waits for work; false when the queue is closed and empty.
func (f *fifo) pop(ctx context.Context) ([]func(), bool) {
	for {
		f.mu.Lock()
		if len(f.q) > 0 {
			q := f.q
			f.q = nil
			f.mu.Unlock()
			return q, true
		}
		closed := f.closed
		f.mu.Unlock()
		if closed {
			return nil, false
		}
		select {
		case <-f.sig:
		case <-ctx.Done():
			return nil, false
		}
	}
}
