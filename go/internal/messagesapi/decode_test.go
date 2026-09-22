package messagesapi

import (
	"encoding/json/v2"
	"fmt"
	"path/filepath"
	"strings"
	"testing"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
)

func deltaLog(ds []agentllm.Delta) string {
	var b strings.Builder
	var kind agentllm.DeltaKind
	for _, d := range ds {
		if d.Kind == agentllm.DeltaToolStart {
			fmt.Fprintf(&b, "\n[tool_start %s %s]", d.Name, d.CallID)
			kind = ""
			continue
		}
		if d.Kind != kind {
			fmt.Fprintf(&b, "\n[%s] ", d.Kind)
			kind = d.Kind
		}
		b.WriteString(d.Text)
	}
	return strings.TrimPrefix(b.String(), "\n")
}

// Every recorded stream decodes to the harness response the design's
// Decode table gives, and streams the deltas the UI shows. The goldens
// pin the whole response; the assertions below name the rules.
func TestDecodeGolden(t *testing.T) {
	t.Parallel()
	for _, name := range []string{"thinking_text", "redacted", "tool_use", "max_tokens_tool", "refusal_before", "refusal_mid", "fallback", "updates"} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			ta := newTestAdapter(t, Config{}, reply{sse: name + ".sse"})
			resp, err := ta.Respond(agentllm.WithSeq(t.Context(), 4), simpleRequest(), ullm.RequestOptions{})
			if err != nil {
				t.Fatal(err)
			}
			b, err := json.Marshal(resp, json.Deterministic(true))
			if err != nil {
				t.Fatal(err)
			}
			out := pretty(t, b)
			out = append(out, []byte("--- deltas\n"+deltaLog(ta.deltas)+"\n")...)
			golden(t, filepath.Join("testdata", "decode", name+".json"), out)
			for _, d := range ta.deltas {
				if d.Seq != 4 || d.Attempt != 1 {
					t.Errorf("delta %+v: want Seq 4 Attempt 1", d)
				}
			}
			checkDecodeRules(t, name, resp)
		})
	}
}

func calls(r ullm.Response) []string {
	var out []string
	for _, it := range r.Output {
		if c, ok := it.Data.(ullm.ToolCall); ok {
			out = append(out, c.CallID+" "+c.Arguments)
		}
	}
	return out
}

func checkDecodeRules(t *testing.T, name string, r ullm.Response) {
	t.Helper()
	switch name {
	case "thinking_text":
		if r.Usage.InputTokens != 130 || r.Usage.CachedInputTokens != 100 || r.Usage.CacheWriteInputTokens != 20 || r.Usage.OutputTokens != 50 || r.Usage.ReasoningTokens != 30 {
			t.Errorf("usage should be inclusive of the cache: %+v", r.Usage)
		}
		if !strings.Contains(string(r.Usage.Raw), `"ephemeral_1h_input_tokens":20`) {
			t.Errorf("usage Raw should keep the 1h split: %s", r.Usage.Raw)
		}
		rs, ok := r.Output[0].Data.(ullm.Reasoning)
		if !ok || len(rs.Summary) != 1 || rs.Summary[0] != "The user wants a greeting." {
			t.Errorf("signed thinking should become Reasoning with its text as summary: %+v", r.Output[0])
		}
	case "tool_use":
		if got := calls(r); len(got) != 2 || got[0] != `toolu_01A {"command": "ls -la", "background": false}` {
			t.Errorf("tool_use arguments must be the accumulated input verbatim: %q", got)
		}
		if r.Stop != ullm.StopComplete {
			t.Errorf("stop tool_use is complete, got %q", r.Stop)
		}
	case "max_tokens_tool":
		if r.Stop != ullm.StopMaxOutputTokens || len(calls(r)) != 0 {
			t.Errorf("a call cut off by max_tokens is dropped: stop %q calls %q", r.Stop, calls(r))
		}
	case "refusal_before", "refusal_mid":
		if r.Stop != ullm.StopRefused || r.Failure == nil || r.Failure.Code != "refusal:cyber" || len(calls(r)) != 0 {
			t.Errorf("a refusal drops every call and names its category: %+v", r)
		}
	case "fallback":
		got := calls(r)
		if len(got) != 1 || !strings.HasPrefix(got[0], "toolu_01Kept") {
			t.Errorf("calls before the fallback block are dropped, after it kept: %q", got)
		}
		for _, it := range r.Output {
			if rs, ok := it.Data.(ullm.Reasoning); ok && strings.Contains(string(rs.Raw), "sigDeclined") {
				t.Errorf("the declined model's thinking must not be echoed back")
			}
		}
		if !strings.Contains(fmt.Sprint(r.Output), "Partial text kept.") {
			t.Errorf("text before the fallback block is kept: %+v", r.Output)
		}
	case "updates":
		// The empty signed block is reasoning to send back; the progress
		// note is its summary.
		if len(r.Output) != 3 {
			t.Fatalf("want two reasoning items and a call, got %+v", r.Output)
		}
		if rs := r.Output[1].Data.(ullm.Reasoning); len(rs.Summary) != 1 || rs.Summary[0] != "Checking the build before the tests." {
			t.Errorf("the progress update should be the summary: %+v", rs)
		}
	}
}

// A decoded thinking block renders back to the same block, byte for
// byte, however many times the session is restored.
func TestThinkingRoundTripsThroughRender(t *testing.T) {
	t.Parallel()
	ta := newTestAdapter(t, Config{}, reply{sse: "thinking_text.sse"})
	resp, err := ta.Respond(t.Context(), simpleRequest(), ullm.RequestOptions{})
	if err != nil {
		t.Fatal(err)
	}
	// The store holds items as JSON; go through it as a restore does.
	b, err := json.Marshal(resp.Output)
	if err != nil {
		t.Fatal(err)
	}
	var restored []ullm.Item
	if err := json.Unmarshal(b, &restored); err != nil {
		t.Fatal(err)
	}
	req := simpleRequest()
	req.Model.ID = "claude-sonnet-5"
	req.Input = append(req.Input, restored...)
	req.Input = append(req.Input, userText("again"))
	p, err := Render(req, Spec("claude-sonnet-5"), "1h")
	if err != nil {
		t.Fatal(err)
	}
	asst := p.Messages[1].Content[0].OfThinking
	if asst == nil || asst.Thinking != "The user wants a greeting." || asst.Signature != "EqQBCgIYAhIMsig1" {
		t.Fatalf("thinking did not round trip: %+v", p.Messages[1].Content[0])
	}
}
