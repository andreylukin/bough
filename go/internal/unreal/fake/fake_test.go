//go:build !windows

package fake_test

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/session"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore/localfile"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/unreal/fake"
	"github.com/andreylukin/bough/internal/unreal/wrap"
)

// reporter collects what a Reporter was told, so a test can assert that
// the tape complained without failing itself.
type reporter struct {
	mu   sync.Mutex
	errs []string
}

func (r *reporter) Helper() {}
func (r *reporter) Errorf(format string, args ...any) {
	r.mu.Lock()
	r.errs = append(r.errs, fmt.Sprintf(format, args...))
	r.mu.Unlock()
}

func user(s string) ullm.Item {
	return ullm.Item{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleUser, Text: s}}
}

func req(texts ...string) ullm.Request {
	var in []ullm.Item
	for _, s := range texts {
		in = append(in, user(s))
	}
	return ullm.Request{Input: in}
}

func text(r ullm.Response) string {
	var parts []string
	for _, it := range r.Output {
		if m, ok := it.Data.(ullm.Message); ok {
			parts = append(parts, m.Text)
		}
	}
	return strings.Join(parts, "")
}

func TestStepsAreFIFOAndMatched(t *testing.T) {
	t.Parallel()
	var got []agentllm.Delta
	a := fake.New(t,
		fake.Step{Want: "hello", Output: []ullm.Item{fake.Text("one")}, Deltas: []agentllm.Delta{{Kind: agentllm.DeltaText, Text: "one"}}},
		fake.Step{Match: func(r ullm.Request) error {
			if len(r.Input) != 2 {
				return errors.New("want two items")
			}
			return nil
		}, Output: []ullm.Item{fake.Text("two")}, Stop: ullm.StopMaxOutputTokens},
	)
	a.SetOptions(agentllm.Options{Sink: func(d agentllm.Delta) { got = append(got, d) }})
	r1, _ := a.Respond(agentllm.WithSeq(t.Context(), 9), req("say hello"), ullm.RequestOptions{})
	r2, _ := a.Respond(t.Context(), req("a", "b"), ullm.RequestOptions{})
	if text(r1) != "one" || text(r2) != "two" || r2.Stop != ullm.StopMaxOutputTokens {
		t.Errorf("responses %q %q %q", text(r1), text(r2), r2.Stop)
	}
	if len(got) != 1 || got[0].Seq != 9 || got[0].Attempt != 1 {
		t.Errorf("deltas = %+v, want one with Seq 9 Attempt 1", got)
	}
	if r1.Usage.InputTokens != 100 || r1.Usage.OutputTokens != 10 {
		t.Errorf("default usage = %+v", r1.Usage)
	}
	if n := len(a.Requests()); n != 2 {
		t.Errorf("recorded %d requests", n)
	}
}

// A mismatch and an exhausted tape both answer, and both are reported;
// neither hangs.
func TestMismatchAndExhaustionAnswer(t *testing.T) {
	t.Parallel()
	rep := &reporter{}
	a := fake.New(rep, fake.Step{Want: "CODE!", Output: []ullm.Item{fake.Text("x")}})
	r1, _ := a.Respond(t.Context(), req("something else"), ullm.RequestOptions{})
	r2, _ := a.Respond(t.Context(), req("more"), ullm.RequestOptions{})
	if !strings.HasPrefix(text(r1), "[script: want \"CODE!\"") || text(r2) != "[script exhausted]" {
		t.Errorf("answers %q, %q", text(r1), text(r2))
	}
	if len(rep.errs) != 2 {
		t.Errorf("reported %d problems, want 2: %q", len(rep.errs), rep.errs)
	}
	// With no reporter (llm-script) the same answers come back quietly.
	quiet := fake.New(nil)
	if r, _ := quiet.Respond(t.Context(), req("x"), ullm.RequestOptions{}); text(r) != "[script exhausted]" {
		t.Errorf("quiet exhaustion = %q", text(r))
	}
}

func TestHoldBlocksUntilReleasedOrCancelled(t *testing.T) {
	t.Parallel()
	release := make(chan struct{})
	a := fake.New(t,
		fake.Step{Hold: release, Output: []ullm.Item{fake.Text("released")}},
		fake.Step{Hold: make(chan struct{}), Output: []ullm.Item{fake.Text("never")}},
	)
	done := make(chan ullm.Response)
	go func() {
		r, _ := a.Respond(t.Context(), req("x"), ullm.RequestOptions{})
		done <- r
	}()
	if err := a.Wait(t.Context(), 1); err != nil {
		t.Fatal(err)
	}
	close(release)
	if r := <-done; text(r) != "released" {
		t.Errorf("got %q", text(r))
	}
	ctx, cancel := context.WithCancel(t.Context())
	go func() { a.Wait(t.Context(), 2); cancel() }()
	if _, err := a.Respond(ctx, req("y"), ullm.RequestOptions{}); !errors.Is(err, context.Canceled) {
		t.Errorf("a cancelled hold returns ctx.Err(), got %v", err)
	}
}

func TestErrStep(t *testing.T) {
	t.Parallel()
	boom := errors.New("boom")
	a := fake.New(t, fake.Step{Err: boom})
	if _, err := a.Respond(t.Context(), req("x"), ullm.RequestOptions{}); !errors.Is(err, boom) {
		t.Errorf("err = %v", err)
	}
}

// Views share one tape: a parent and a subagent consume it in request
// order, each with its own sink.
func TestViewsShareTheTape(t *testing.T) {
	t.Parallel()
	a := fake.New(t, fake.Step{Output: []ullm.Item{fake.Text("first")}}, fake.Step{Output: []ullm.Item{fake.Text("second")}})
	var parent, child []agentllm.Delta
	pv := a.View(agentllm.Options{Sink: func(d agentllm.Delta) { parent = append(parent, d) }})
	cv := a.View(agentllm.Options{Sink: func(d agentllm.Delta) { child = append(child, d) }})
	r1, _ := pv.Respond(t.Context(), req("x"), ullm.RequestOptions{})
	r2, _ := cv.Respond(t.Context(), req("y"), ullm.RequestOptions{})
	if text(r1) != "first" || text(r2) != "second" || len(a.Requests()) != 2 {
		t.Errorf("views: %q %q", text(r1), text(r2))
	}
}

const script = `{"steps": [
  {"want": "CODE!", "think": "plan", "calls": [{"id": "c1", "name": "bash", "args": {"command": "echo hi"}}]},
  {"text": "ran it", "usage": {"input": 1200, "cached": 1000, "output": 40}},
  {"want": "slow", "hold_ms": 30, "text": "done waiting"},
  {"calls": [{"name": "view", "args": {"path": "a"}}, {"name": "bash", "args": "{broken"}]},
  {"error": "overloaded"},
  {"error": "overflow"},
  {"stop": "refused"}
]}`

func TestLoadTheJSONForm(t *testing.T) {
	t.Parallel()
	path := filepath.Join(t.TempDir(), "s.json")
	if err := os.WriteFile(path, []byte(script), 0o644); err != nil {
		t.Fatal(err)
	}
	steps, err := fake.Load(path)
	if err != nil {
		t.Fatal(err)
	}
	if len(steps) != 7 {
		t.Fatalf("got %d steps", len(steps))
	}
	if steps[0].Want != "CODE!" || len(steps[0].Output) != 2 || steps[0].Output[1].Data.(ullm.ToolCall).Arguments != `{"command": "echo hi"}` {
		t.Errorf("step 1 = %+v", steps[0])
	}
	if u := steps[1].Usage; u.InputTokens != 1200 || u.CachedInputTokens != 1000 || u.OutputTokens != 40 {
		t.Errorf("step 2 usage = %+v", u)
	}
	var words []string
	for _, d := range steps[1].Deltas {
		words = append(words, d.Text)
	}
	if strings.Join(words, "|") != "ran |it" {
		t.Errorf("text streams word by word: %q", words)
	}
	c1 := steps[3].Output[0].Data.(ullm.ToolCall)
	c2 := steps[3].Output[1].Data.(ullm.ToolCall)
	if c1.CallID != "call_4_1" || c2.CallID != "call_4_2" || c2.Arguments != "{broken" {
		t.Errorf("default ids and verbatim string args: %+v %+v", c1, c2)
	}
	if steps[4].Err == nil || !errors.Is(steps[5].Err, agentllm.ErrContextOverflow) || steps[6].Stop != ullm.StopRefused {
		t.Errorf("error and stop steps: %v %v %q", steps[4].Err, steps[5].Err, steps[6].Stop)
	}

	// The whole tape plays through an adapter, holds included.
	a := fake.New(t, steps[:3]...)
	start := time.Now()
	a.Respond(t.Context(), req("CODE! please"), ullm.RequestOptions{})
	a.Respond(t.Context(), req("x"), ullm.RequestOptions{})
	a.Respond(t.Context(), req("slow one"), ullm.RequestOptions{})
	if time.Since(start) < 30*time.Millisecond {
		t.Error("hold_ms did not hold")
	}
}

func TestLoadErrorsNameTheStep(t *testing.T) {
	t.Parallel()
	for body, want := range map[string]string{
		`{"steps": []}`: "no steps",
		`{"steps": [{"text": "a"}, {"bogus": 1}]}`: "step 2: unknown keys",
		`{"steps": [{"calls": [{"args": {}}]}]}`:   "step 1: call 1 has no name",
		`{"steps": [{"stop": "sideways"}]}`:        "stop must be",
		`{"steps": [{"want": "only a want"}]}`:     "needs text",
		`not json`:                                 "fake: t.json",
	} {
		if _, err := fake.Parse([]byte(body), "t.json"); err == nil || !strings.Contains(err.Error(), want) {
			t.Errorf("%s: err = %v, want it to mention %q", body, err, want)
		}
	}
}

// A recorded session becomes a tape: one step per provider response,
// muted replies skipped, recorded errors kept, envelopes removed.
func TestFromStore(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	st, err := localfile.New(dir)
	if err != nil {
		t.Fatal(err)
	}
	ctx := t.Context()
	sid := session.ID("s-1")
	if _, err := st.Create(ctx, sid); err != nil {
		t.Fatal(err)
	}
	sealed := ullm.Item{Type: ullm.ItemReasoning, ProviderID: "anthropic|x", Data: ullm.Reasoning{Raw: wrap.Seal("anthropic", []byte(`{"type":"thinking","thinking":"t","signature":"s"}`))}}
	var prev session.TurnID
	for i, resp := range []ullm.Response{
		{ID: "msg_1", Stop: ullm.StopComplete, Output: []ullm.Item{sealed, fake.Call("toolu_1", "bash", `{"command":"ls"}`)}},
		{ID: "bough-muted-2", Stop: ullm.StopComplete},
		{ID: "bough-error-3", Stop: ullm.StopComplete},
		{ID: "msg_4", Stop: ullm.StopComplete, Output: []ullm.Item{fake.Text("done")}},
	} {
		turn := session.TurnID(fmt.Sprintf("t%d", i))
		if err := st.AppendTurn(ctx, sid, session.Turn{ID: turn, PreviousTurnID: prev, Type: session.TurnRegular}); err != nil {
			t.Fatal(err)
		}
		prev = turn
		if err := st.AppendModelResponse(ctx, sid, sessionstore.ModelResponse{TurnID: turn, Response: resp}); err != nil {
			t.Fatal(err)
		}
	}
	steps, err := fake.FromStore(dir, string(sid))
	if err != nil {
		t.Fatal(err)
	}
	if len(steps) != 3 || steps[1].Err == nil {
		t.Fatalf("steps = %+v", steps)
	}
	r := steps[0].Output[0]
	if r.ProviderID != "x" || string(r.Data.(ullm.Reasoning).Raw) != `{"type":"thinking","thinking":"t","signature":"s"}` {
		t.Errorf("the envelope should be removed: %+v", r)
	}
	if _, err := fake.FromStore(dir, "missing"); err == nil {
		t.Error("a missing session is an error")
	}
}

func TestAssertAppendOnly(t *testing.T) {
	t.Parallel()
	call := fake.Call("c1", "bash", "{}")
	res := ullm.Item{Type: ullm.ItemToolResult, Data: ullm.ToolResult{CallID: "c1", Output: []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: "ok"}}}}
	good := []fake.Recorded{
		{Request: ullm.Request{Input: []ullm.Item{user("a")}}},
		{Request: ullm.Request{Input: []ullm.Item{user("a"), call, res}}},
		{Request: ullm.Request{Input: []ullm.Item{user("a"), call, res, fake.Text("x"), user("b")}}},
	}
	rep := &reporter{}
	fake.AssertAppendOnly(rep, good)
	if len(rep.errs) != 0 {
		t.Errorf("a clean sequence was flagged: %q", rep.errs)
	}
	rewritten := []fake.Recorded{
		{Request: ullm.Request{Input: []ullm.Item{user("a"), call, res, fake.Text("x"), user("b")}}},
		{Request: ullm.Request{Input: []ullm.Item{user("a"), call, res, fake.Text("CHANGED"), user("b"), user("c")}}},
	}
	rep = &reporter{}
	fake.AssertAppendOnly(rep, rewritten)
	if len(rep.errs) != 1 || !strings.Contains(rep.errs[0], "changed") {
		t.Errorf("a rewrite should be flagged once: %q", rep.errs)
	}
	missing := []fake.Recorded{{Request: ullm.Request{Input: []ullm.Item{user("a"), call, user("b")}}}}
	rep = &reporter{}
	fake.AssertAppendOnly(rep, missing)
	if len(rep.errs) != 1 || !strings.Contains(rep.errs[0], "no result") {
		t.Errorf("a call with no result should be flagged: %q", rep.errs)
	}
}
