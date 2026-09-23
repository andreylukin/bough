package session

import (
	"context"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/unreal/fake"
	"github.com/andreylukin/bough/plugins/history"
)

func TestTextTurn(t *testing.T) {
	t.Parallel()
	r := newRig(t, fake.Step{Want: "hi there", Output: []ullmItem{fake.Text("hello back")}})
	r.rt.Submit("hi there")
	r.waitDone(1)
	if got, want := r.turnKinds(), []string{"input", "assistant", "done"}; !slices.Equal(got, want) {
		t.Fatalf("kinds %v, want %v\n%s", got, want, r.dump())
	}
	if a := r.last("assistant"); a.Data["text"] != "hello back" || a.Data["provider"] != "fake" || a.Data["model"] != "fake-model" {
		t.Fatalf("assistant %v", a.Data)
	}
	in := r.last("input")
	if in.Data["input_id"] == nil {
		t.Fatalf("input has no input_id: %v", in.Data)
	}
	eng := r.last("engine")
	if eng.Data["session"] != "s1" || eng.Data["pin"] != "v0.1.1" || eng.Data["format"] != float64(2) && eng.Data["format"] != 2 {
		t.Fatalf("engine entry %v", eng.Data)
	}
	if d := r.last("done"); d.Data["engine_turn"] == nil || d.Data["usage"] != nil && d.Data["wake"] != nil {
		t.Fatalf("done %v", d.Data)
	}
	// The entry is written before its event is emitted.
	r.waitFor("the done event", func() bool { return slices.Contains(r.evs.kinds(), "done") })
	if !slices.Contains(r.evs.kinds(), "assistant") {
		t.Fatalf("events %v", r.evs.kinds())
	}
	if _, err := os.Stat(filepath.Join(r.dir, "engine", "s1.system.md")); err != nil {
		t.Fatalf("no frozen system prompt: %v", err)
	}
	fake.AssertAppendOnly(t, r.fake.Requests())
}

// A call still running when the grace window ends shows the model the
// placeholder beside the result that did arrive; the model ends its
// reply, and the turn stays open (no done) until the slow call finishes
// and the model has read its result.
func TestCallInProgressThenFinal(t *testing.T) {
	t.Parallel()
	r := newRig(t,
		fake.Step{Want: "run it", Output: []ullmItem{
			fake.Call("c0", "echo", `{"text":"quick"}`),
			fake.Call("c1", "hold", `{"text":"long job"}`),
		}},
		fake.Step{Match: func(req ullmRequest) error {
			last := fake.LastUserText(req.Input)
			if !strings.Contains(last, "echoed quick") || !strings.Contains(last, "still running") {
				return errf("want the quick result and the placeholder, got %q", last)
			}
			return nil
		}, Output: []ullmItem{fake.Text("waiting for it")}},
		fake.Step{Want: "held and released", Output: []ullmItem{fake.Text("it finished")}},
	)
	r.rt.Submit("run it")
	r.waitRequests(2)
	r.waitFor("the placeholder reply", func() bool { return r.count("assistant") == 1 })
	r.stays("no done while the call runs", 300*time.Millisecond, func() bool { return r.count("done") == 0 })
	close(r.kit.release)
	r.waitDone(1)
	if got, want := r.turnKinds(), []string{"input", "call", "assistant", "call", "assistant", "done"}; !slices.Equal(got, want) {
		t.Fatalf("kinds %v, want %v\n%s", got, want, r.dump())
	}
	call := r.last("call")
	if call.Data["tool"] != "hold" || call.Data["id"] != "c1" || call.Data["late"] != true || call.Data["text"] != "long job" {
		t.Fatalf("call row %v", call.Data)
	}
	if !strings.Contains(call.Data["output"].(string), "held and released") {
		t.Fatalf("call output %v", call.Data["output"])
	}
	if r.last("done").Data["running"] != nil {
		t.Fatalf("done says calls were left running: %v", r.last("done").Data)
	}
	if !slices.Contains(r.evs.kinds(), "call-delta") {
		t.Fatalf("no call-delta event: %v", r.evs.kinds())
	}
	fake.AssertAppendOnly(t, r.fake.Requests())
}

// Two calls in one response run in parallel and, finishing inside the
// grace window, come back to the model in one request.
func TestTwoCallsFanOut(t *testing.T) {
	t.Parallel()
	r := newRig(t,
		fake.Step{Want: "both", Output: []ullmItem{
			fake.Call("a", "echo", `{"text":"one"}`),
			fake.Call("b", "echo", `{"text":"two"}`),
		}},
		fake.Step{Match: func(req ullmRequest) error {
			last := fake.LastUserText(req.Input)
			if !strings.Contains(last, "echoed one") || !strings.Contains(last, "echoed two") {
				return errf("want both results in one request, got %q", last)
			}
			return nil
		}, Output: []ullmItem{fake.Text("both done")}},
	)
	r.rt.Submit("both please")
	r.waitDone(1)
	if len(r.fake.Requests()) != 2 {
		t.Fatalf("%d requests, want 2", len(r.fake.Requests()))
	}
	if r.count("call") != 2 || r.count("done") != 1 {
		t.Fatalf("history\n%s", r.dump())
	}
	fake.AssertAppendOnly(t, r.fake.Requests())
}

// ask blocks on the user: its call holds the turn with no settle, and
// the answer wakes the model inside the same turn.
func TestAskRoundTrip(t *testing.T) {
	t.Parallel()
	r := newRigWith(t, []rigOpt{settle(100 * time.Millisecond)},
		fake.Step{Want: "decide", Output: []ullmItem{fake.Call("q1", "ask", `{"text":"which one?"}`)}},
		fake.Step{Want: "the user answered: the blue one", Output: []ullmItem{fake.Text("blue it is")}},
	)
	r.rt.Submit("decide for me")
	r.waitRequests(1)
	// Well past turn_settle: a blocking call is never adopted.
	r.stays("the turn stays open on ask", 600*time.Millisecond, func() bool {
		return r.count("done") == 0 && r.count("job") == 0
	})
	r.kit.answer <- "the blue one"
	r.waitDone(1)
	if r.last("assistant").Data["text"] != "blue it is" || r.count("done") != 1 {
		t.Fatalf("history\n%s", r.dump())
	}
}

// Esc during a running call: the call is cancelled and its row lands
// before cancelled + done, the model is not asked about it on its own,
// and the next input reaches the provider with the cancelled result.
func TestCancelMidCall(t *testing.T) {
	t.Parallel()
	r := newRig(t,
		fake.Step{Want: "slow", Output: []ullmItem{fake.Call("h1", "hold", `{"text":"sleep 100"}`)}},
		fake.Step{Want: "next", Match: func(req ullmRequest) error {
			if !strings.Contains(fake.Render(req), "Cancelled") {
				return errf("the cancelled result is not in the request")
			}
			return nil
		}, Output: []ullmItem{fake.Text("ok, moving on")}},
	)
	r.rt.Submit("slow thing")
	r.waitRequests(1)
	r.waitFor("the call to start", func() bool {
		r.kit.mu.Lock()
		defer r.kit.mu.Unlock()
		return slices.Contains(r.kit.calls, "hold")
	})
	r.rt.Cancel()
	r.waitDone(1)
	kinds := r.turnKinds()
	if got, want := kinds, []string{"input", "call", "cancelled", "done"}; !slices.Equal(got, want) {
		t.Fatalf("kinds %v, want %v\n%s", got, want, r.dump())
	}
	if c := r.last("call"); c.Data["canceled"] != true {
		t.Fatalf("call row %v", c.Data)
	}
	r.stays("no request after the cancel", 1500*time.Millisecond, func() bool { return len(r.fake.Requests()) == 1 })
	r.rt.Submit("next thing")
	r.waitDone(2)
	if len(r.fake.Requests()) != 2 || r.last("assistant").Data["text"] != "ok, moving on" {
		t.Fatalf("history\n%s", r.dump())
	}
	fake.AssertAppendOnly(t, r.fake.Requests())
}

// A steer while a request is in flight waits for that request's answer,
// then goes in; the turn has one done.
func TestSteer(t *testing.T) {
	t.Parallel()
	hold := make(chan struct{})
	r := newRig(t,
		fake.Step{Want: "write", Hold: hold, Output: []ullmItem{fake.Text("drafting")}},
		fake.Step{Want: "shorter please", Output: []ullmItem{fake.Text("short draft")}},
	)
	if r.rt.Steer("too early") {
		t.Fatal("steer accepted with no turn open")
	}
	r.rt.Submit("write a poem")
	r.waitRequests(1)
	if !r.rt.Steer("shorter please") {
		t.Fatal("steer refused mid-turn")
	}
	close(hold)
	r.waitDone(1)
	if got, want := r.turnKinds(), []string{"input", "input", "assistant", "assistant", "done"}; !slices.Equal(got, want) {
		t.Fatalf("kinds %v, want %v\n%s", got, want, r.dump())
	}
	var steer history.Entry
	for _, e := range r.entries() {
		if e.Kind == "input" && e.Data["steer"] == true {
			steer = e
		}
	}
	if steer.Data["text"] != "shorter please" {
		t.Fatalf("no steer input entry\n%s", r.dump())
	}
	if !slices.Contains(r.evs.kinds(), "steer") {
		t.Fatalf("no steer event")
	}
	fake.AssertAppendOnly(t, r.fake.Requests())
}

// Close and reopen: the next input continues the same harness session,
// with the earlier turn in context and no reseed.
func TestResume(t *testing.T) {
	t.Parallel()
	r := newRig(t,
		fake.Step{Want: "first", Output: []ullmItem{fake.Text("first answer")}},
		fake.Step{Want: "second", Match: func(req ullmRequest) error {
			if !strings.Contains(fake.Render(req), "first answer") {
				return errf("the earlier turn is not in context")
			}
			return nil
		}, Output: []ullmItem{fake.Text("second answer")}},
	)
	r.rt.Submit("first question")
	r.waitDone(1)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := r.rt.Close(ctx); err != nil {
		t.Fatal(err)
	}
	path := r.hist.Path()
	_ = r.hist.Close()
	h, err := history.OpenExisting(path)
	if err != nil {
		t.Fatal(err)
	}
	r.hist = h
	r.open()
	r.rt.Submit("second question")
	r.waitDone(2)
	engines := 0
	for _, e := range r.entries() {
		if e.Kind == "engine" {
			engines++
			if e.Data["session"] != "s1" || e.Data["seeded"] != nil || e.Data["forked_from"] != nil {
				t.Fatalf("engine entry %v", e.Data)
			}
		}
	}
	if engines != 2 {
		t.Fatalf("%d engine entries, want one per build\n%s", engines, r.dump())
	}
	fake.AssertAppendOnly(t, r.fake.Requests())
}

// history.Fork copies a finished turn; the child session forks the
// harness store at that turn's engine_turn, so the model sees the turns
// up to the fork point and none after.
func TestFork(t *testing.T) {
	t.Parallel()
	r := newRig(t,
		fake.Step{Want: "alpha", Output: []ullmItem{fake.Text("alpha answer")}},
		fake.Step{Want: "beta", Output: []ullmItem{fake.Text("beta answer")}},
		fake.Step{Want: "gamma", Match: func(req ullmRequest) error {
			s := fake.Render(req)
			if !strings.Contains(s, "alpha answer") || strings.Contains(s, "beta") {
				return errf("the fork's context is wrong:\n%s", s)
			}
			return nil
		}, Output: []ullmItem{fake.Text("gamma answer")}},
	)
	r.rt.Submit("alpha")
	r.waitDone(1)
	r.rt.Submit("beta")
	r.waitDone(2)
	var first int64
	for _, e := range r.entries() {
		if e.Kind == "input" {
			first = e.Seq
			break
		}
	}
	child := filepath.Join(r.dir, "history", "s2.jsonl")
	if err := history.Fork(r.hist.Path(), first, child); err != nil {
		t.Fatal(err)
	}
	h, err := history.OpenExisting(child)
	if err != nil {
		t.Fatal(err)
	}
	c := &rig{t: t, dir: r.dir, hist: h, evs: &events{}, kit: r.kit, fake: r.fake}
	c.open()
	c.rt.Submit("gamma")
	c.waitDone(2) // the copied turn's done, and gamma's
	eng := c.last("engine")
	if eng.Data["session"] != "s2" || eng.Data["forked_from"] != "s1" || eng.Data["fork_turn"] == nil {
		t.Fatalf("engine entry %v\n%s", eng.Data, c.dump())
	}
	if c.last("assistant").Data["text"] != "gamma answer" {
		t.Fatalf("history\n%s", c.dump())
	}
}

// A forked session's model reads the calls it inherited with their
// results: the harness fork strips their operations, and without them
// every earlier call reached the model as a call that never answered.
func TestForkKeepsInheritedResults(t *testing.T) {
	t.Parallel()
	r := newRig(t,
		fake.Step{Want: "alpha", Output: []ullmItem{fake.Call("e1", "echo", `{"text":"PARENTRESULT"}`)}},
		fake.Step{Want: "echoed PARENTRESULT", Output: []ullmItem{fake.Text("alpha answer")}},
		fake.Step{Want: "gamma", Match: func(req ullmRequest) error {
			s := fake.Render(req)
			if !strings.Contains(s, "result e1 text:echoed PARENTRESULT") {
				return errf("the inherited call has no result:\n%s", s)
			}
			return nil
		}, Output: []ullmItem{fake.Text("gamma answer")}},
		fake.Step{Want: "delta", Match: func(req ullmRequest) error {
			if s := fake.Render(req); !strings.Contains(s, "result e1 text:echoed PARENTRESULT") {
				return errf("a reopened fork lost the inherited result:\n%s", s)
			}
			return nil
		}, Output: []ullmItem{fake.Text("delta answer")}},
	)
	r.rt.Submit("alpha")
	r.waitDone(1)
	var first int64
	for _, e := range r.entries() {
		if e.Kind == "input" {
			first = e.Seq
			break
		}
	}
	child := filepath.Join(r.dir, "history", "s2.jsonl")
	if err := history.Fork(r.hist.Path(), first, child); err != nil {
		t.Fatal(err)
	}
	h, err := history.OpenExisting(child)
	if err != nil {
		t.Fatal(err)
	}
	c := &rig{t: t, dir: r.dir, hist: h, evs: &events{}, kit: r.kit, fake: r.fake}
	c.open()
	c.rt.Submit("gamma")
	c.waitDone(2)
	if c.last("assistant").Data["text"] != "gamma answer" {
		t.Fatalf("history\n%s", c.dump())
	}
	// A new Runtime on the fork builds its coordinator from the store
	// again, which strips the inherited operations again.
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_ = c.rt.Close(ctx)
	c.open()
	c.rt.Submit("delta")
	c.waitDone(3)
	if c.last("assistant").Data["text"] != "delta answer" {
		t.Fatalf("history\n%s", c.dump())
	}
}

// A fork at a turn that closed with calls still running is refused:
// the harness fork leaves inherited running calls without results.
func TestForkRefusedOverRunningCalls(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	path := filepath.Join(dir, "history", "s3.jsonl")
	h, err := history.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	h.Append("engine", map[string]any{"session": "parent", "engine": "unreal"})
	in := h.Append("input", map[string]any{"text": "go"})
	_ = in
	h.Append("job", map[string]any{"id": 3, "event": "started", "cmd": "go test ./...", "call": "c9"})
	h.Append("done", map[string]any{"engine_turn": "t1", "running": 1})
	r := &rig{t: t, dir: dir, hist: h, evs: &events{}, kit: newTestkit(t), fake: fake.New(t)}
	r.open()
	r.rt.Submit("continue")
	r.waitDone(2)
	e := r.last("error")
	if !strings.Contains(e.Data["text"].(string), "cannot fork at a turn whose calls were still running (job 3: go test ./...)") {
		t.Fatalf("error %v", e.Data)
	}
	if n := len(r.fake.Requests()); n != 0 {
		t.Fatalf("%d provider requests after a refused fork", n)
	}
}
