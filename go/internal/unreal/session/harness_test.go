//go:build !windows

package session

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/unreal/fake"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

// fakeSource is an llm row whose engine adapter is a view of one fake
// tape: the session, its rebuilt coordinators and its children all
// consume the same script in request order.
type fakeSource struct{ a *fake.Adapter }

func (s fakeSource) AgentAdapter(o agentllm.Options) (agentllm.Adapter, error) {
	return s.a.View(o), nil
}

// events records what the session emits, in order.
type events struct {
	mu  sync.Mutex
	evs []ev
}

type ev struct {
	kind, text string
	data       map[string]any
}

func (e *events) emit(kind, text string, data map[string]any) {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.evs = append(e.evs, ev{kind, text, data})
}

func (e *events) kinds() []string {
	e.mu.Lock()
	defer e.mu.Unlock()
	var out []string
	for _, x := range e.evs {
		out = append(out, x.kind)
	}
	return out
}

// testkit is the tool set the session tests run against: the real
// agent-tools registry, so toolreg and boughcall run as in production.
type testkit struct {
	reg     agenttools.Registry
	release chan struct{} // closes every hold
	answer  chan string   // the ask tool's answers
	mu      sync.Mutex
	calls   []string
}

func newTestkit(t *testing.T) *testkit {
	k := &testkit{reg: agenttools.NewRegistry(), release: make(chan struct{}), answer: make(chan string, 4)}
	must := func(tl agenttools.Tool) {
		t.Helper()
		if _, err := k.reg.Register(tl); err != nil {
			t.Fatal(err)
		}
	}
	obj := agenttools.Object(nil, map[string]any{"text": agenttools.Prop("string", "text")})
	detail := func(args json.RawMessage) string {
		var a struct{ Text string }
		_ = json.Unmarshal(args, &a)
		return a.Text
	}
	must(agenttools.Tool{Name: "echo", Description: "echo text", Schema: obj, Detail: detail,
		Call: func(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
			k.note("echo")
			var a struct{ Text string }
			_ = json.Unmarshal(c.Args, &a)
			return agenttools.Result{Text: "echoed " + a.Text, Data: map[string]any{"exit": 0}}, nil
		}})
	must(agenttools.Tool{Name: "fail", Description: "always fails", Schema: obj,
		Call: func(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
			return agenttools.Result{Error: "it broke"}, nil
		}})
	must(agenttools.Tool{Name: "hold", Description: "runs until released", Schema: obj, Detail: detail,
		Call: func(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
			k.note("hold")
			c.Emit("working…\n")
			select {
			case <-k.release:
				return agenttools.Result{Text: "held and released"}, nil
			case <-ctx.Done():
				return agenttools.Result{}, ctx.Err()
			}
		}})
	must(agenttools.Tool{Name: "ask", Description: "ask the user", Schema: obj, Blocking: true,
		Call: func(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
			select {
			case a := <-k.answer:
				return agenttools.Result{Text: "the user answered: " + a}, nil
			case <-ctx.Done():
				return agenttools.Result{}, ctx.Err()
			}
		}})
	return k
}

func (k *testkit) note(name string) {
	k.mu.Lock()
	k.calls = append(k.calls, name)
	k.mu.Unlock()
}

// rig is one session under test.
type rig struct {
	t     *testing.T
	dir   string
	hist  *history.Store
	evs   *events
	kit   *testkit
	fake  *fake.Adapter
	rt    *Runtime
	usage *llm.Usage
}

type rigOpt func(*Deps)

func settle(d time.Duration) rigOpt { return func(dp *Deps) { dp.Config.TurnSettle = d } }

func newRig(t *testing.T, steps ...fake.Step) *rig {
	return newRigWith(t, nil, steps...)
}

func newRigWith(t *testing.T, opts []rigOpt, steps ...fake.Step) *rig {
	t.Helper()
	dir := t.TempDir()
	h, err := history.Open(filepath.Join(dir, "history", "s1.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	r := &rig{t: t, dir: dir, hist: h, evs: &events{}, kit: newTestkit(t), fake: fake.New(t, steps...)}
	r.open(opts...)
	return r
}

// open (re)opens a Runtime on the rig's history and store.
func (r *rig) open(opts ...rigOpt) {
	r.t.Helper()
	d := r.deps()
	for _, o := range opts {
		o(&d)
	}
	rt, err := Open(context.Background(), d)
	if err != nil {
		r.t.Fatal(err)
	}
	r.rt = rt
	r.t.Cleanup(func() {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = rt.Close(ctx)
	})
}

func (r *rig) deps() Deps {
	return Deps{
		SessionID: strings.TrimSuffix(filepath.Base(r.hist.Path()), ".jsonl"),
		Store:     filepath.Join(r.dir, "engine"),
		Scratch:   func() string { return filepath.Join(r.dir, "scratch") },
		Cwd:       r.dir,
		Config:    Config{TurnSettle: time.Minute},
		LLM: func() (agentllm.Source, string, error) {
			return fakeSource{r.fake}, "fake", nil
		},
		Tools:   r.kit.reg,
		History: r.hist,
		Emit:    r.evs.emit,
	}
}

// entries is the history as recorded, minus bookkeeping.
func (r *rig) entries() []history.Entry { return r.hist.Entries() }

func (r *rig) count(kind string) int {
	n := 0
	for _, e := range r.entries() {
		if e.Kind == kind {
			n++
		}
	}
	return n
}

// waitFor polls until cond holds, failing with the history on timeout.
func (r *rig) waitFor(what string, cond func() bool) {
	r.t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		if cond() {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	r.t.Fatalf("timed out waiting for %s\nhistory:\n%s", what, r.dump())
}

func (r *rig) waitDone(n int) {
	r.t.Helper()
	r.waitFor(fmt.Sprintf("%d done", n), func() bool { return r.count("done") >= n })
}

// waitRequests waits until the fake has seen n requests.
func (r *rig) waitRequests(n int) {
	r.t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := r.fake.Wait(ctx, n); err != nil {
		r.t.Fatalf("%v\nhistory:\n%s", err, r.dump())
	}
}

func (r *rig) dump() string {
	var b strings.Builder
	for _, e := range r.entries() {
		d := map[string]any{}
		for k, v := range e.Data {
			if k != "checkpoint" && k != "system_file" && k != "store" {
				d[k] = v
			}
		}
		j, _ := json.Marshal(d)
		fmt.Fprintf(&b, "%3d %-10s %s\n", e.Seq, e.Kind, j)
	}
	return b.String()
}

// turnKinds is the history's turn structure: the kinds that say what a
// reader sees, in order.
func (r *rig) turnKinds() []string {
	var out []string
	for _, e := range r.entries() {
		switch e.Kind {
		case "engine", "meta":
			continue
		}
		out = append(out, e.Kind)
	}
	return out
}

func (r *rig) last(kind string) history.Entry {
	es := r.entries()
	for i := len(es) - 1; i >= 0; i-- {
		if es[i].Kind == kind {
			return es[i]
		}
	}
	r.t.Fatalf("no %s entry\n%s", kind, r.dump())
	return history.Entry{}
}

// requestText is request n's last user-side text (1-based).
func (r *rig) requestText(n int) string {
	reqs := r.fake.Requests()
	if len(reqs) < n {
		r.t.Fatalf("only %d requests", len(reqs))
	}
	return fake.LastUserText(reqs[n-1].Request.Input)
}

// stays asserts cond keeps holding for d.
func (r *rig) stays(what string, d time.Duration, cond func() bool) {
	r.t.Helper()
	deadline := time.Now().Add(d)
	for time.Now().Before(deadline) {
		if !cond() {
			r.t.Fatalf("%s stopped holding\n%s", what, r.dump())
		}
		time.Sleep(10 * time.Millisecond)
	}
}

var errBoom = errors.New("provider exploded")

type (
	ullmItem    = ullm.Item
	ullmRequest = ullm.Request
)

func errf(format string, args ...any) error { return fmt.Errorf(format, args...) }
