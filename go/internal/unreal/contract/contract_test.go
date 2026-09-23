//go:build !windows

package contract

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io/fs"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/unreallabsai/unreal-agent/harness/contextbuilder"
	"github.com/unreallabsai/unreal-agent/harness/coordinator"
	"github.com/unreallabsai/unreal-agent/harness/inbox"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/operation"
	"github.com/unreallabsai/unreal-agent/harness/session"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore/localfile"
	"github.com/unreallabsai/unreal-agent/harness/tool"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/unreal/boughcall"
	"github.com/andreylukin/bough/internal/unreal/fake"
	"github.com/andreylukin/bough/internal/unreal/ops"
	"github.com/andreylukin/bough/internal/unreal/toolreg"
)

// env is one harness session wired the way internal/unreal/session
// wires it — localfile store, toolreg, the bough.call handler, the
// session-lifetime ops.Manager — minus bough's Gate and actor, so what
// is asserted is the harness's own behaviour.
type env struct {
	t     *testing.T
	dir   string
	sid   session.ID
	store *localfile.Store
	tr    tool.Registry
	ops   *ops.Manager
	h     *boughcall.Handler
	llm   ullm.Adapter
	hold  chan struct{} // closing it finishes every hold call

	mu    sync.Mutex
	items []sessionstore.Item

	in     *inbox.Inbox
	cancel context.CancelFunc
	done   chan error
}

func newEnv(t *testing.T, llm ullm.Adapter) *env {
	t.Helper()
	e := &env{t: t, dir: t.TempDir(), sid: "contract", llm: llm, hold: make(chan struct{})}
	store, err := localfile.New(e.dir)
	if err != nil {
		t.Fatal(err)
	}
	e.store = store
	store.AddObserver(func(_ session.ID, it sessionstore.Item) {
		e.mu.Lock()
		e.items = append(e.items, it)
		e.mu.Unlock()
	})
	reg := agenttools.NewRegistry()
	obj := agenttools.Object(nil, map[string]any{"text": agenttools.Prop("string", "text")})
	for _, tl := range []agenttools.Tool{
		{Name: "echo", Description: "echo", Schema: obj, Call: func(_ context.Context, c agenttools.Call) (agenttools.Result, error) {
			return agenttools.Result{Text: "echoed " + string(c.Args)}, nil
		}},
		{Name: "hold", Description: "hold", Schema: obj, Call: func(ctx context.Context, _ agenttools.Call) (agenttools.Result, error) {
			select {
			case <-e.hold:
				return agenttools.Result{Text: "released"}, nil
			case <-ctx.Done():
				return agenttools.Result{}, ctx.Err()
			}
		}},
	} {
		if _, err := reg.Register(tl); err != nil {
			t.Fatal(err)
		}
	}
	e.tr = toolreg.New(toolreg.Config{Tools: reg.Tools()})
	e.h = boughcall.New(boughcall.Options{Tools: reg.Lookup, Session: string(e.sid), CallTimeout: time.Minute})
	ctx, cancel := context.WithCancel(context.Background())
	e.ops = ops.New(ctx, operation.NewLocalOperationManager(ctx, e.h), nil)
	t.Cleanup(func() {
		e.stop()
		cancel()
		_ = e.h.Close()
	})
	return e
}

// start builds a coordinator on the store as it is now and runs it.
func (e *env) start() {
	e.t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	restored, err := e.store.Resume(ctx, e.sid)
	if errors.Is(err, fs.ErrNotExist) {
		if _, err := e.store.Create(ctx, e.sid); err != nil {
			e.t.Fatal(err)
		}
		restored, err = e.store.Resume(ctx, e.sid)
	}
	if err != nil {
		e.t.Fatal(err)
	}
	in, err := inbox.New(ctx, restored.ExternalInputIDs)
	if err != nil {
		e.t.Fatal(err)
	}
	b := contextbuilder.NewBuilder()
	b.SetModel(ullm.Model{ID: "contract"})
	for _, d := range e.tr.StaticDefinitions() {
		b.AddTool(d.Tool)
	}
	c := coordinator.New(coordinator.Dependencies{
		SessionID: e.sid, Inbox: in, Restored: restored, Sessions: e.store,
		ContextBuilder: b, LLM: e.llm, Tools: e.tr, Operations: e.ops,
	})
	e.in, e.cancel, e.done = in, cancel, make(chan error, 1)
	go func() { e.done <- c.Run(ctx) }()
}

// stop cancels Run's ctx — how bough restarts a coordinator, never
// StopHard — and waits for Run to return.
func (e *env) stop() {
	if e.cancel == nil {
		return
	}
	e.cancel()
	select {
	case <-e.done:
	case <-time.After(5 * time.Second):
		e.t.Error("Run did not return after its ctx was cancelled")
	}
	e.cancel = nil
}

func (e *env) submit(id, text string) {
	e.t.Helper()
	payload := jsontext.Value(fmt.Sprintf("%q", text))
	if err := e.in.Submit(context.Background(), inbox.Input{ID: inbox.ID(id), Kind: inbox.InputExternal, Payload: payload}); err != nil {
		e.t.Fatal(err)
	}
}

func (e *env) of(kind sessionstore.ItemKind) []sessionstore.Item {
	e.mu.Lock()
	defer e.mu.Unlock()
	var out []sessionstore.Item
	for _, it := range e.items {
		if it.Kind == kind {
			out = append(out, it)
		}
	}
	return out
}

func (e *env) waitFor(what string, cond func() bool) {
	e.t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for !cond() {
		if time.Now().After(deadline) {
			e.t.Fatalf("timed out waiting for %s", what)
		}
		time.Sleep(5 * time.Millisecond)
	}
}

// stays asserts cond keeps holding for d.
func (e *env) stays(what string, d time.Duration, cond func() bool) {
	e.t.Helper()
	deadline := time.Now().Add(d)
	for time.Now().Before(deadline) {
		if !cond() {
			e.t.Fatalf("%s stopped holding", what)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func results(r ullm.Request) map[string]string {
	out := map[string]string{}
	for _, it := range r.Input {
		if res, ok := it.Data.(ullm.ToolResult); ok {
			var b strings.Builder
			for _, o := range res.Output {
				b.WriteString(o.Value)
			}
			out[res.CallID] = b.String()
		}
	}
	return out
}

// 1a. Completions inside the grace window after a response are batched
// into one request.
func TestCompletionsInGraceAreOneRequest(t *testing.T) {
	t.Parallel()
	f := fake.New(t,
		fake.Step{Output: []ullm.Item{fake.Call("a", "echo", `{"text":"a"}`), fake.Call("b", "echo", `{"text":"b"}`)}},
		fake.Step{Output: []ullm.Item{fake.Text("both")}},
	)
	e := newEnv(t, f)
	e.start()
	e.submit("in1", "go")
	e.waitFor("two requests", func() bool { return len(f.Requests()) == 2 })
	e.stays("two requests", 1500*time.Millisecond, func() bool { return len(f.Requests()) == 2 })
	got := results(f.Requests()[1].Request)
	if !strings.HasPrefix(got["a"], "echoed") || !strings.HasPrefix(got["b"], "echoed") {
		t.Fatalf("second request results = %v", got)
	}
}

// 1b. A completion inside grace plus a call still running when grace
// expires gives exactly one request at about 1s, with the running call
// as a placeholder; the model is not called again until it finishes.
func TestGraceExpiryIsOneRequestWithAPlaceholder(t *testing.T) {
	t.Parallel()
	f := fake.New(t,
		fake.Step{Output: []ullm.Item{fake.Call("a", "echo", `{"text":"a"}`), fake.Call("h", "hold", `{"text":"h"}`)}},
		fake.Step{Output: []ullm.Item{fake.Text("waiting")}},
		fake.Step{Output: []ullm.Item{fake.Text("finished")}},
	)
	e := newEnv(t, f)
	e.start()
	e.submit("in1", "go")
	e.waitFor("the second request", func() bool { return len(f.Requests()) == 2 })
	turns := e.of(sessionstore.ItemTurn)
	resp := e.of(sessionstore.ItemModelResponse)
	if len(turns) != 2 || len(resp) < 1 {
		t.Fatalf("%d turns, %d responses", len(turns), len(resp))
	}
	gap := turns[1].RecordedAt.Sub(resp[0].RecordedAt)
	if gap < 800*time.Millisecond || gap > 3*time.Second {
		t.Fatalf("second request %v after the response, want about the 1s grace", gap)
	}
	got := results(f.Requests()[1].Request)
	if !strings.HasPrefix(got["a"], "echoed") {
		t.Fatalf("echo result = %q", got["a"])
	}
	if got["h"] == "" || got["h"] == "released" {
		t.Fatalf("hold should be a running placeholder, got %q", got["h"])
	}
	e.stays("no request while hold runs", 1200*time.Millisecond, func() bool { return len(f.Requests()) == 2 })
	close(e.hold)
	e.waitFor("the completion's request", func() bool { return len(f.Requests()) == 3 })
	if got := results(f.Requests()[2].Request)["h"]; got != "released" && !strings.Contains(got, "released") {
		t.Fatalf("late result = %q", got)
	}
}

// 2. A reply with no tool calls and nothing pending leaves the
// coordinator idle until the next input.
func TestTextReplyLeavesItIdle(t *testing.T) {
	t.Parallel()
	f := fake.New(t, fake.Step{Output: []ullm.Item{fake.Text("hi")}}, fake.Step{Output: []ullm.Item{fake.Text("again")}})
	e := newEnv(t, f)
	e.start()
	e.submit("in1", "hello")
	e.waitFor("one request", func() bool { return len(f.Requests()) == 1 })
	e.stays("idle", 1500*time.Millisecond, func() bool { return len(f.Requests()) == 1 })
	e.submit("in2", "more")
	e.waitFor("the next input's request", func() bool { return len(f.Requests()) == 2 })
}

// 3. An empty completed Response — what the Gate answers for a cancel,
// an error or a muted request — is persisted and treated as idle.
func TestEmptyResponseIsPersistedAndIdle(t *testing.T) {
	t.Parallel()
	f := fake.New(t, fake.Step{}, fake.Step{Output: []ullm.Item{fake.Text("back")}})
	e := newEnv(t, f)
	e.start()
	e.submit("in1", "hello")
	e.waitFor("the empty response", func() bool { return len(e.of(sessionstore.ItemModelResponse)) == 1 })
	mr := e.of(sessionstore.ItemModelResponse)[0].Data.(sessionstore.ModelResponse)
	if len(mr.Response.Output) != 0 || mr.Response.Stop != ullm.StopComplete {
		t.Fatalf("persisted %+v", mr.Response)
	}
	e.stays("idle", 1500*time.Millisecond, func() bool { return len(f.Requests()) == 1 })
	e.submit("in2", "more")
	e.waitFor("the next request", func() bool { return len(f.Requests()) == 2 })
}

// 4. LocalOperationManager.Add of a known id returns nil and emits
// nothing, even after the op finished — the reason ops.Manager exists.
func TestLocalManagerDropsAKnownAdd(t *testing.T) {
	t.Parallel()
	h := &stepHandler{updates: make(chan operation.Operation, 4), added: make(chan operation.Operation, 4)}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	m := operation.NewLocalOperationManager(ctx, h)
	op := newOp(t, "op1")
	if err := m.Add(op); err != nil {
		t.Fatal(err)
	}
	accepted := <-h.added
	h.finish(t, accepted)
	select {
	case got := <-m.Updates():
		if got.Status != operation.StatusCompleted {
			t.Fatalf("status %s", got.Status)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("no terminal update")
	}
	if err := m.Add(op); err != nil {
		t.Fatalf("re-Add = %v, want nil", err)
	}
	select {
	case got := <-m.Updates():
		t.Fatalf("re-Add emitted %s %s", got.ID, got.Status)
	case <-h.added:
		t.Fatal("re-Add reached the handler")
	case <-time.After(300 * time.Millisecond):
	}
}

// 5. Run's ctx cancelled while a call runs, the call finishing with no
// coordinator, then a new coordinator on the same ops.Manager: the
// call still reaches a terminal status and the model reads its result.
func TestRebuiltCoordinatorGetsTheResult(t *testing.T) {
	t.Parallel()
	f := fake.New(t,
		fake.Step{Output: []ullm.Item{fake.Call("h", "hold", `{"text":"h"}`)}},
		fake.Step{Output: []ullm.Item{fake.Text("got it")}},
	)
	e := newEnv(t, f)
	e.start()
	e.submit("in1", "go")
	e.waitFor("the call's first status", func() bool { return len(e.of(sessionstore.ItemToolCallStatus)) >= 1 })
	e.stop()
	close(e.hold)
	time.Sleep(100 * time.Millisecond) // the update lands with nobody reading
	e.start()
	e.waitFor("a terminal status", func() bool {
		for _, it := range e.of(sessionstore.ItemToolCallStatus) {
			st := it.Data.(sessionstore.ToolCallStatus)
			for _, op := range st.Operations {
				if op.Status == operation.StatusCompleted {
					return true
				}
			}
		}
		return false
	})
	e.waitFor("the model reading it", func() bool { return len(f.Requests()) == 2 })
	if got := results(f.Requests()[1].Request)["h"]; !strings.Contains(got, "released") {
		t.Fatalf("result = %q", got)
	}
}

// 6. Observers fire synchronously, in order, and only for appended
// items: never for a Resume or an Items read.
func TestObserversAreSynchronousInOrderAndAppendOnly(t *testing.T) {
	t.Parallel()
	e := newEnv(t, fake.New(t))
	ctx := context.Background()
	if _, err := e.store.Create(ctx, e.sid); err != nil {
		t.Fatal(err)
	}
	for i := range 3 {
		before := len(e.of(sessionstore.ItemInput))
		in := inbox.Input{ID: inbox.ID(fmt.Sprintf("in%d", i)), Kind: inbox.InputExternal, Payload: jsontext.Value(`"x"`)}
		if err := e.store.AppendInput(ctx, e.sid, in); err != nil {
			t.Fatal(err)
		}
		if len(e.of(sessionstore.ItemInput)) != before+1 {
			t.Fatalf("append %d returned before its observer ran", i)
		}
	}
	e.mu.Lock()
	seqs := make([]sessionstore.Sequence, 0, len(e.items))
	for _, it := range e.items {
		seqs = append(seqs, it.Sequence)
	}
	n := len(e.items)
	e.mu.Unlock()
	for i := 1; i < len(seqs); i++ {
		if seqs[i] <= seqs[i-1] {
			t.Fatalf("observed sequences %v, want strictly increasing", seqs)
		}
	}
	if _, err := e.store.Resume(ctx, e.sid); err != nil {
		t.Fatal(err)
	}
	if _, err := e.store.Items(ctx, e.sid, sessionstore.BeforeFirst, 100); err != nil {
		t.Fatal(err)
	}
	e.mu.Lock()
	after := len(e.items)
	e.mu.Unlock()
	if after != n {
		t.Fatalf("a read fired %d observer calls", after-n)
	}
}

// sameID answers every request with one fixed Response.ID.
type sameID struct{ *fake.Adapter }

func (s sameID) Respond(ctx context.Context, r ullm.Request, o ullm.RequestOptions) (ullm.Response, error) {
	resp, err := s.Adapter.Respond(ctx, r, o)
	resp.ID = "same"
	return resp, err
}

// 7. The coordinator never reads Response.ID: two responses with the
// same id are both persisted and both reach the next request. bough's
// Gate puts its own ids there to match a Meta to its response.
func TestResponseIDIsNotRead(t *testing.T) {
	t.Parallel()
	f := fake.New(t,
		fake.Step{Output: []ullm.Item{fake.Text("first reply")}},
		fake.Step{Output: []ullm.Item{fake.Text("second reply")}},
		fake.Step{Output: []ullm.Item{fake.Text("third reply")}},
	)
	e := newEnv(t, sameID{f})
	e.start()
	e.submit("in1", "one")
	e.waitFor("request 1", func() bool { return len(e.of(sessionstore.ItemModelResponse)) == 1 })
	e.submit("in2", "two")
	e.waitFor("request 2", func() bool { return len(e.of(sessionstore.ItemModelResponse)) == 2 })
	e.submit("in3", "three")
	e.waitFor("request 3", func() bool { return len(f.Requests()) == 3 })
	r := fake.Render(f.Requests()[2].Request)
	if !strings.Contains(r, "first reply") || !strings.Contains(r, "second reply") {
		t.Fatalf("third request lost a reply with a repeated id:\n%s", r)
	}
}

// ctxAdapter holds the first request until its ctx ends and records
// why it ended.
type ctxAdapter struct {
	*fake.Adapter
	mu    sync.Mutex
	first error
	n     int
}

func (c *ctxAdapter) Respond(ctx context.Context, r ullm.Request, o ullm.RequestOptions) (ullm.Response, error) {
	c.mu.Lock()
	c.n++
	n := c.n
	c.mu.Unlock()
	if n == 1 {
		<-ctx.Done()
		c.mu.Lock()
		c.first = ctx.Err()
		c.mu.Unlock()
		return ullm.Response{}, ctx.Err()
	}
	return c.Adapter.Respond(ctx, r, o)
}

// 9. An input arriving mid-request supersedes it: the request's ctx is
// cancelled and nothing of it is persisted.
func TestSupersededRequestIsCancelledAndNotPersisted(t *testing.T) {
	t.Parallel()
	c := &ctxAdapter{Adapter: fake.New(t, fake.Step{Output: []ullm.Item{fake.Text("answered both")}})}
	e := newEnv(t, c)
	e.start()
	e.submit("in1", "first")
	e.waitFor("the first request", func() bool { return len(e.of(sessionstore.ItemTurn)) == 1 })
	e.submit("in2", "second")
	e.waitFor("the second response", func() bool { return len(e.of(sessionstore.ItemModelResponse)) == 1 })
	c.mu.Lock()
	first := c.first
	c.mu.Unlock()
	if !errors.Is(first, context.Canceled) {
		t.Fatalf("superseded request ctx ended with %v", first)
	}
	turns := e.of(sessionstore.ItemTurn)
	mr := e.of(sessionstore.ItemModelResponse)
	if len(turns) != 2 || len(mr) != 1 {
		t.Fatalf("%d turns, %d responses; want 2 and 1", len(turns), len(mr))
	}
	if got, want := mr[0].Data.(sessionstore.ModelResponse).TurnID, turns[1].Data.(session.Turn).ID; got != want {
		t.Fatalf("the persisted response belongs to turn %s, want the second %s", got, want)
	}
}

// 10. store.Fork at a turn boundary with no running calls resumes and
// runs, and the child's first request carries the parent's exchange.
func TestForkAtATurnBoundaryResumesAndRuns(t *testing.T) {
	t.Parallel()
	f := fake.New(t, fake.Step{Output: []ullm.Item{fake.Text("parent reply")}})
	e := newEnv(t, f)
	e.start()
	e.submit("in1", "parent question")
	e.waitFor("the parent's response", func() bool { return len(e.of(sessionstore.ItemModelResponse)) == 1 })
	e.stop()
	turn := e.of(sessionstore.ItemModelResponse)[0].Data.(sessionstore.ModelResponse).TurnID
	ctx := context.Background()
	child := session.ID("contract-child")
	if _, err := e.store.Fork(ctx, child, e.sid, turn); err != nil {
		t.Fatal(err)
	}
	cf := fake.New(t, fake.Step{Output: []ullm.Item{fake.Text("child reply")}})
	ce := newEnv(t, cf)
	ce.dir, ce.store, ce.sid = e.dir, e.store, child
	ce.start()
	ce.submit("in2", "child question")
	ce.waitFor("the child's request", func() bool { return len(cf.Requests()) == 1 })
	r := fake.Render(cf.Requests()[0].Request)
	if !strings.Contains(r, "parent question") || !strings.Contains(r, "parent reply") || !strings.Contains(r, "child question") {
		t.Fatalf("child request:\n%s", r)
	}
}

// stepHandler is a remote job handler the test finishes by hand.
type stepHandler struct {
	updates chan operation.Operation
	added   chan operation.Operation
}

func (h *stepHandler) RemoteJobPlanType() operation.RemoteJobPlanType       { return "test" }
func (h *stepHandler) RemoteJobPlanVersion() operation.RemoteJobPlanVersion { return 1 }
func (h *stepHandler) AddRemoteJob(op operation.Operation) error {
	h.added <- op
	return nil
}
func (h *stepHandler) CancelRemoteJob(operation.ID, string) error   { return nil }
func (h *stepHandler) RemoteJobUpdates() <-chan operation.Operation { return h.updates }

func (h *stepHandler) finish(t *testing.T, op operation.Operation) {
	t.Helper()
	state, err := operation.DecodeRemoteJobState(op)
	if err != nil {
		t.Fatal(err)
	}
	state.TerminalResult = "done"
	step, err := operation.UpdateRemoteJob(op, state, operation.StatusCompleted)
	if err != nil {
		t.Fatal(err)
	}
	h.updates <- *step.Operation
}

func newOp(t *testing.T, id string) operation.Operation {
	t.Helper()
	spec, err := operation.NewRemoteJobSpec(operation.RemoteJobPlan{Type: "test", Version: 1, Data: jsontext.Value(`{}`)})
	if err != nil {
		t.Fatal(err)
	}
	return operation.Operation{MaxOutputLength: spec.MaxOutputLength, ID: operation.ID(id), Type: spec.Type,
		Version: spec.Version, Status: operation.StatusReady, State: spec.State}
}
