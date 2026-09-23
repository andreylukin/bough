//go:build !windows

package session

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/unreal/fake"
	"github.com/andreylukin/bough/plugins/history"
)

// A foreground call still running turn_settle after the model went
// quiet becomes a numbered job and the turn closes with done{running};
// its completion wakes the model in a turn of its own, done{wake}.
func TestSettleAdoptsThenWakes(t *testing.T) {
	t.Parallel()
	r := newRigWith(t, []rigOpt{settle(200 * time.Millisecond)},
		fake.Step{Want: "serve", Output: []ullmItem{fake.Call("h1", "hold", `{"text":"npm run dev"}`)}},
		fake.Step{Want: "held and released", Output: []ullmItem{fake.Text("the server stopped")}},
	)
	r.rt.Submit("serve it")
	r.waitDone(1)
	d := r.last("done")
	if d.Data["running"] != 1 && d.Data["running"] != float64(1) {
		t.Fatalf("done %v\n%s", d.Data, r.dump())
	}
	job := r.last("job")
	if job.Data["event"] != "started" || job.Data["call"] != "h1" || job.Data["cmd"] != "npm run dev" {
		t.Fatalf("job %v", job.Data)
	}
	close(r.kit.release)
	r.waitDone(2)
	if got, want := r.turnKinds(), []string{"input", "job", "done", "call", "job", "input", "assistant", "done"}; !slices.Equal(got, want) {
		t.Fatalf("kinds %v, want %v\n%s", got, want, r.dump())
	}
	wake := r.last("input")
	if wake.Data["wake"] != true || wake.Data["reason"] != "call" || !strings.Contains(wake.Data["text"].(string), "job 1000 finished: npm run dev") {
		t.Fatalf("wake input %v", wake.Data)
	}
	if r.last("done").Data["wake"] != true {
		t.Fatalf("wake done %v", r.last("done").Data)
	}
	if c := r.last("call"); c.Data["adopted"] != true {
		t.Fatalf("adopted call row %v", c.Data)
	}
}

// A provider error is a turn-level failure: error then done{stop:error},
// and Run survives it, so the next input works.
func TestProviderErrorKeepsRunAlive(t *testing.T) {
	t.Parallel()
	r := newRig(t,
		fake.Step{Want: "one", Err: errBoom},
		fake.Step{Want: "two", Output: []ullmItem{fake.Text("fine now")}},
	)
	r.rt.Submit("one")
	r.waitDone(1)
	if got, want := r.turnKinds(), []string{"input", "error", "done"}; !slices.Equal(got, want) {
		t.Fatalf("kinds %v, want %v\n%s", got, want, r.dump())
	}
	if r.last("done").Data["stop"] != "error" || !strings.Contains(r.last("error").Data["text"].(string), "provider exploded") {
		t.Fatalf("history\n%s", r.dump())
	}
	r.rt.Submit("two")
	r.waitDone(2)
	if r.last("assistant").Data["text"] != "fine now" || r.count("engine") != 1 {
		t.Fatalf("Run did not survive\n%s", r.dump())
	}
}

// An overflow is sticky for the model: the next turn fails at once
// without a provider request.
func TestOverflowIsSticky(t *testing.T) {
	t.Parallel()
	r := newRig(t, fake.Step{Want: "big", Err: fmt.Errorf("prompt too long: %w", agentllm.ErrContextOverflow)})
	r.rt.Submit("big")
	r.waitDone(1)
	r.rt.Submit("again")
	r.waitDone(2)
	if n := len(r.fake.Requests()); n != 1 {
		t.Fatalf("%d provider requests, want 1 (the overflow is sticky)", n)
	}
	if !strings.Contains(r.last("error").Data["text"].(string), "no longer fits") || r.count("error") != 2 {
		t.Fatalf("history\n%s", r.dump())
	}
}

// max_steps mutes the request past the budget and ends the turn with a
// system note and done{stop:max_steps}.
func TestMaxSteps(t *testing.T) {
	t.Parallel()
	r := newRigWith(t, []rigOpt{func(d *Deps) { d.Config.MaxSteps = 2 }},
		fake.Step{Want: "loop", Output: []ullmItem{fake.Call("e1", "echo", `{"text":"a"}`)}},
		fake.Step{Output: []ullmItem{fake.Call("e2", "echo", `{"text":"b"}`)}},
	)
	r.rt.Submit("loop forever")
	r.waitDone(1)
	if n := len(r.fake.Requests()); n != 2 {
		t.Fatalf("%d requests, want 2", n)
	}
	if r.last("done").Data["stop"] != "max_steps" || !strings.Contains(r.last("system").Data["text"].(string), "step budget reached (max_steps 2)") {
		t.Fatalf("history\n%s", r.dump())
	}
}

// fakeJobs is job-notices without numbering (Adopt returns 0).
type fakeJobs struct {
	mu   sync.Mutex
	news []string
	wake chan struct{}
}

func (j *fakeJobs) Take() []string {
	j.mu.Lock()
	defer j.mu.Unlock()
	n := j.news
	j.news = nil
	return n
}
func (j *fakeJobs) Wake() <-chan struct{} { return j.wake }
func (j *fakeJobs) Adopt(string, string, func()) (int, func(*int, bool)) {
	return 0, nil
}
func (j *fakeJobs) notify(s string) {
	j.mu.Lock()
	j.news = append(j.news, s)
	j.mu.Unlock()
	select {
	case j.wake <- struct{}{}:
	default:
	}
}

// A background notice while idle starts a turn of its own.
func TestNoticeWake(t *testing.T) {
	t.Parallel()
	jobs := &fakeJobs{wake: make(chan struct{}, 1)}
	r := newRigWith(t, []rigOpt{func(d *Deps) { d.Jobs = func() Jobs { return jobs } }},
		fake.Step{Want: "job 7 finished", Output: []ullmItem{fake.Text("noted the build")}},
	)
	jobs.notify("job 7 finished: make build (exit 0)")
	r.waitDone(1)
	in := r.last("input")
	if in.Data["wake"] != true || in.Data["reason"] != "notice" || !strings.HasPrefix(in.Data["text"].(string), "[background job]") {
		t.Fatalf("input %v", in.Data)
	}
	if r.last("done").Data["wake"] != true {
		t.Fatalf("done %v", r.last("done").Data)
	}
}

// /model mid-session: the llm row is a new service value, so the next
// request goes through a new adapter, with no coordinator restart.
func TestModelSwapNoRestart(t *testing.T) {
	t.Parallel()
	r := newRig(t, fake.Step{Want: "first", Output: []ullmItem{fake.Text("from A")}})
	b := fake.New(t, fake.Step{Match: func(req ullmRequest) error {
		if !strings.Contains(fake.Render(req), "from A") {
			return errf("the new adapter does not get the conversation")
		}
		return nil
	}, Output: []ullmItem{fake.Text("from B")}})
	var mu sync.Mutex
	cur := fakeSource{r.fake}
	r.rt.d.LLM = func() (agentllm.Source, string, error) {
		mu.Lock()
		defer mu.Unlock()
		return cur, "fake", nil
	}
	r.rt.Submit("first")
	r.waitDone(1)
	mu.Lock()
	cur = fakeSource{b}
	mu.Unlock()
	r.rt.Submit("second")
	r.waitDone(2)
	if r.last("assistant").Data["text"] != "from B" || r.count("engine") != 1 {
		t.Fatalf("history\n%s", r.dump())
	}
}

// A tool registered mid-session changes the tools block: the
// coordinator restarts at idle and the next request carries the tool.
func TestToolSetChangeRestartsAtIdle(t *testing.T) {
	t.Parallel()
	r := newRig(t,
		fake.Step{Want: "first", Output: []ullmItem{fake.Text("ok")}},
		fake.Step{Match: func(req ullmRequest) error {
			for _, tl := range req.Tools {
				if tl.Name == "wordcount" {
					return nil
				}
			}
			return errf("the new tool is not offered")
		}, Output: []ullmItem{fake.Text("ok again")}},
	)
	r.rt.Submit("first")
	r.waitDone(1)
	if _, err := r.kit.reg.Register(agenttools.Tool{Name: "wordcount", Description: "count words",
		Schema: agenttools.Object(nil, map[string]any{}),
		Call: func(context.Context, agenttools.Call) (agenttools.Result, error) {
			return agenttools.Result{Text: "0"}, nil
		}}); err != nil {
		t.Fatal(err)
	}
	r.waitFor("the restart", func() bool { return r.count("engine") == 2 })
	r.rt.Submit("second")
	r.waitDone(2)
}

// A /model swap remounts the rows that read the llm, and their tools
// unregister and register again: a set that comes back as it was is not
// a change, so the coordinator keeps running and no error is recorded.
func TestToolSetBlipDoesNotRestart(t *testing.T) {
	t.Parallel()
	r := newRig(t,
		fake.Step{Want: "first", Output: []ullmItem{fake.Text("ok")}},
		fake.Step{Want: "second", Output: []ullmItem{fake.Text("ok again")}},
	)
	r.rt.Submit("first")
	r.waitDone(1)
	echo, ok := r.kit.reg.Lookup("echo")
	if !ok {
		t.Fatal("no echo tool")
	}
	// Three blips, each a tool that appears and is gone 20ms later.
	for range 3 {
		off, err := r.kit.reg.Register(agenttools.Tool{Name: "echo_blip", Description: echo.Description,
			Schema: echo.Schema, Call: echo.Call})
		if err != nil {
			t.Fatal(err)
		}
		time.Sleep(20 * time.Millisecond)
		off()
	}
	r.stays("no restart", toolSettle+500*time.Millisecond, func() bool { return r.count("engine") == 1 })
	r.rt.Submit("second")
	r.waitDone(2)
	if r.count("engine") != 1 || r.count("error") != 0 {
		t.Fatalf("history\n%s", r.dump())
	}
}

// A subagent runs on a child coordinator and writes sub:* rows.
func TestSubagent(t *testing.T) {
	t.Parallel()
	r := newRig(t,
		fake.Step{Want: "count the files", Output: []ullmItem{fake.Call("x1", "echo", `{"text":"ls"}`)}},
		fake.Step{Want: "echoed ls", Output: []ullmItem{fake.Text("there are 3")}},
	)
	res, err := r.rt.Children().Run(context.Background(), ChildRequest{Task: "count the files", Worker: "1", MaxSteps: 5})
	if err != nil {
		t.Fatal(err)
	}
	if res.Status != "done" || res.Reply != "there are 3" || res.Steps != 2 {
		t.Fatalf("result %+v\n%s", res, r.dump())
	}
	if got, want := r.turnKinds(), []string{"sub:start", "sub:call", "sub:assistant", "sub:done"}; !slices.Equal(got, want) {
		t.Fatalf("kinds %v, want %v\n%s", got, want, r.dump())
	}
	for _, e := range r.entries() {
		if w, ok := e.Data["worker"].(int); !ok || w != 1 {
			t.Fatalf("%s has worker %#v, want the number 1", e.Kind, e.Data["worker"])
		}
	}
	if st := r.last("sub:done").Data["status"]; st != "ok" {
		t.Fatalf("sub:done status %v, want ok as the loop writes it", st)
	}
	for in, want := range map[[2]string]string{
		{"done", "Status: failed\nFindings: none"}: "failed",
		{"done", "**Status:** ok"}:                 "ok",
		{"budget", ""}:                             "error",
		{"cancelled", ""}:                          "cancelled",
	} {
		if got := cardStatus(in[0], in[1]); got != want {
			t.Errorf("cardStatus(%q, %q) = %q, want %q", in[0], in[1], got, want)
		}
	}
	for _, rq := range r.fake.Requests() {
		for _, tl := range rq.Request.Tools {
			if tl.Name == "ask" {
				t.Fatal("a child was offered ask")
			}
		}
	}
}

// A loop session resumed on the engine is seeded: its transcript goes
// in ahead of the first input, and the engine entry says so.
func TestSeedFromLoopHistory(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	h, err := history.Open(filepath.Join(dir, "history", "old.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	h.Append("input", map[string]any{"text": "what is 2+2"})
	h.Append("assistant", map[string]any{"text": "it is 4"})
	h.Append("done", map[string]any{})
	r := &rig{t: t, dir: dir, hist: h, evs: &events{}, kit: newTestkit(t), fake: fake.New(t,
		fake.Step{Match: func(req ullmRequest) error {
			s := fake.LastUserText(req.Input)
			if !strings.Contains(s, "<earlier-session>") || !strings.Contains(s, "it is 4") || !strings.HasSuffix(s, "and times 3?") {
				return errf("no seed: %q", s)
			}
			return nil
		}, Output: []ullmItem{fake.Text("12")}})}
	r.open()
	r.rt.Submit("and times 3?")
	r.waitDone(2)
	if r.last("engine").Data["seeded"] != "loop" {
		t.Fatalf("engine %v", r.last("engine").Data)
	}
	// The recorded input is what the user typed, not the seed.
	if r.last("input").Data["text"] != "and times 3?" {
		t.Fatalf("input %v", r.last("input").Data)
	}
}

// hseq catch-up: rows the store has and history lost (a kill -9 between
// the two writes) are recorded at Open, once.
func TestCatchUp(t *testing.T) {
	t.Parallel()
	r := newRig(t, fake.Step{Want: "hello", Output: []ullmItem{fake.Text("hi")}})
	r.rt.Submit("hello")
	r.waitDone(1)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_ = r.rt.Close(ctx)
	// The "crashed" history: everything but the assistant row, plus a
	// subagent row stamped with its own store's (higher) sequence, which
	// must not count as how far this store's rows reached.
	dir2 := filepath.Join(t.TempDir(), "history")
	var keep []history.Entry
	for _, e := range r.entries() {
		if e.Kind != "assistant" {
			keep = append(keep, e)
		}
	}
	keep = append(keep, history.Entry{Seq: keep[len(keep)-1].Seq + 1, Kind: "sub:assistant", Data: map[string]any{"text": "child", "worker": 1, "hseq": 999}})
	path := filepath.Join(dir2, "s1.jsonl")
	writeEntries(t, path, keep)
	h, err := history.OpenExisting(path)
	if err != nil {
		t.Fatal(err)
	}
	c := &rig{t: t, dir: r.dir, hist: h, evs: &events{}, kit: r.kit, fake: r.fake}
	c.open()
	if c.count("assistant") != 1 || c.last("assistant").Data["text"] != "hi" {
		t.Fatalf("catch-up did not restore the row\n%s", c.dump())
	}
	if len(c.evs.kinds()) != 0 {
		t.Fatalf("catch-up emitted %v", c.evs.kinds())
	}
	// A second open finds nothing missing.
	ctx2, cancel2 := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel2()
	_ = c.rt.Close(ctx2)
	c.open()
	if c.count("assistant") != 1 {
		t.Fatalf("catch-up wrote twice\n%s", c.dump())
	}
}

// A call that was running when the process died is not re-run: the
// next coordinator re-Adds it and it fails as interrupted.
func TestRestartFailsAwaitingCallAsInterrupted(t *testing.T) {
	t.Parallel()
	r := newRig(t,
		fake.Step{Want: "long", Output: []ullmItem{fake.Call("h9", "hold", `{"text":"forever"}`)}},
		fake.Step{Want: "interrupted", Output: []ullmItem{fake.Text("it was interrupted")}},
	)
	r.rt.Submit("long thing")
	r.waitFor("the call to start", func() bool {
		r.kit.mu.Lock()
		defer r.kit.mu.Unlock()
		return slices.Contains(r.kit.calls, "hold")
	})
	r.waitFor("the store to hold the op as awaiting", func() bool {
		b, _ := os.ReadFile(r.rt.StorePath())
		return strings.Contains(string(b), `"Status":"awaiting"`)
	})
	// Die without the shutdown path: the store keeps the op awaiting.
	r.rt.q.close()
	r.rt.cancel()
	<-r.rt.exited
	path := r.hist.Path()
	h, err := history.OpenExisting(path)
	if err != nil {
		t.Fatal(err)
	}
	c := &rig{t: t, dir: r.dir, hist: h, evs: &events{}, kit: newTestkit(t), fake: r.fake}
	c.open()
	c.rt.Submit("go on")
	c.waitFor("the interrupted call row", func() bool {
		for _, e := range c.entries() {
			if e.Kind == "call" && strings.Contains(fmt.Sprint(e.Data["error"]), "interrupted") {
				return true
			}
		}
		return false
	})
	c.waitFor("the reply", func() bool { return c.count("assistant") >= 1 })
}

// A process that dies after the cancel, before the muted request took
// the cancelled call's result, leaves that result pending in the store.
// The next process parks it again, so the result reaches the model with
// the next input and never in a request of its own: the tape's one
// remaining step must see both.
func TestReopenAfterCancelKeepsCallsParked(t *testing.T) {
	t.Parallel()
	r := newRig(t,
		fake.Step{Want: "long", Output: []ullmItem{fake.Call("h7", "hold", `{"text":"forever"}`)}},
		fake.Step{Want: "go on", Match: func(req ullmRequest) error {
			if !strings.Contains(fake.Render(req), "interrupted") {
				return errf("the interrupted result is not in the request")
			}
			return nil
		}, Output: []ullmItem{fake.Text("carrying on")}},
	)
	r.rt.Submit("long thing")
	r.waitFor("the call to start", func() bool {
		r.kit.mu.Lock()
		defer r.kit.mu.Unlock()
		return slices.Contains(r.kit.calls, "hold")
	})
	r.waitFor("the store to hold the op as awaiting", func() bool {
		b, _ := os.ReadFile(r.rt.StorePath())
		return strings.Contains(string(b), `"Status":"awaiting"`)
	})
	r.rt.q.close()
	r.rt.cancel()
	<-r.rt.exited
	// What the history row writes when it opens a turn a dead process
	// left open.
	r.hist.Append("cancelled", map[string]any{"interrupted": true})
	h, err := history.OpenExisting(r.hist.Path())
	if err != nil {
		t.Fatal(err)
	}
	c := &rig{t: t, dir: r.dir, hist: h, evs: &events{}, kit: newTestkit(t), fake: r.fake}
	c.open()
	c.rt.gate.mu.Lock()
	muted := c.rt.gate.parked && c.rt.gate.covered([]string{"call:h7"})
	c.rt.gate.mu.Unlock()
	if !muted {
		t.Fatal("a request answering only the cancelled call would reach the model")
	}
	c.rt.Submit("go on")
	c.waitFor("the reply", func() bool { return c.count("assistant") >= 1 })
	c.stays("no request of its own", 300*time.Millisecond, func() bool { return len(c.fake.Requests()) == 2 })
	if a := c.last("assistant"); a.Data["text"] != "carrying on" {
		t.Fatalf("history\n%s", c.dump())
	}
}

func writeEntries(t *testing.T, path string, es []history.Entry) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	var b strings.Builder
	for _, e := range es {
		line, err := json.Marshal(e)
		if err != nil {
			t.Fatal(err)
		}
		b.Write(line)
		b.WriteByte('\n')
	}
	if err := os.WriteFile(path, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
}
