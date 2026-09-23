//go:build !windows

package responses

// Probes P5-P7 of go/docs/unreal-engine.md §15.3 on the Responses side.
// They log what the provider did rather than asserting it: the answers
// decide settings (late_results, reasoning replay), and the results are
// recorded in internal/messagesapi's package doc.
//
//	BOUGH_LIVE=1 BOUGH_PROBES=1 go test -run TestProbe -v ./internal/unreal/responses/

import (
	"context"
	"errors"
	"net/http"
	"os"
	"strings"
	"testing"
	"time"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/llm/responsesapi"
)

func probe(t *testing.T, env string) string {
	t.Helper()
	if os.Getenv("BOUGH_PROBES") != "1" {
		t.Skip("set BOUGH_PROBES=1 (and BOUGH_LIVE=1) to run the provider probes")
	}
	return live(t, env)
}

func probeAdapter(t *testing.T, kind, key, model, effort string) *adapter {
	t.Helper()
	a, err := New(Config{
		Kind: kind, APIKey: func() (string, error) { return key, nil },
		Model: func() string { return model }, Effort: func() string { return effort },
		HTTP: &http.Client{}, MaxAttempts: 1,
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { a.Close() })
	return a.(*adapter)
}

func outcome(resp ullm.Response, err error) string {
	if err != nil {
		var api *responsesapi.APIError
		if errors.As(err, &api) {
			return "error " + http.StatusText(api.StatusCode) + ": " + api.Message
		}
		return "error: " + err.Error()
	}
	var text []string
	for _, it := range resp.Output {
		if m, ok := it.Data.(ullm.Message); ok {
			text = append(text, m.Text)
		}
	}
	return "ok: " + strings.Join(text, " ")
}

func msgItem(role ullm.Role, s string) ullm.Item {
	return ullm.Item{Type: ullm.ItemMessage, Data: ullm.Message{Role: role, Text: s}}
}

func resultItem(id, s string) ullm.Item {
	return ullm.Item{Type: ullm.ItemToolResult, Data: ullm.ToolResult{CallID: id, Output: []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: s}}}}
}

// P5: a second function_call_output for one call id, sent natively to an
// Anthropic model behind OpenRouter. 400 = late_results must stay text;
// ok and naming the final value = native is safe; ok and naming only
// the placeholder = OpenRouter drops the second one.
func TestProbeP5DuplicateResultOpenRouterAnthropic(t *testing.T) {
	key := probe(t, "OPENROUTER_API_KEY")
	a := probeAdapter(t, "openrouter", key, "anthropic/claude-sonnet-5", "")
	ctx, cancel := context.WithTimeout(t.Context(), 2*time.Minute)
	defer cancel()
	req := ullm.Request{
		Input: []ullm.Item{
			msgItem(ullm.RoleSystem, "You are a test harness. Answer in one short sentence."),
			msgItem(ullm.RoleUser, "Run get_value, then tell me what it returned."),
			{Type: ullm.ItemToolCall, Data: ullm.ToolCall{CallID: "toolu_p5", Name: "get_value", Arguments: "{}"}},
			resultItem("toolu_p5", "This call is still running. Its result arrives later."),
			msgItem(ullm.RoleAssistant, "It is still running; I will wait."),
			resultItem("toolu_p5", "the value is tangerine-42"),
			msgItem(ullm.RoleUser, "It finished. What did get_value return?"),
		},
		Tools: []ullm.Tool{{Type: ullm.ToolFunction, Name: "get_value", Description: "Get the value.", Parameters: map[string]any{"type": "object", "properties": map[string]any{}}}},
	}
	resp, err := a.Respond(ctx, req, ullm.RequestOptions{})
	t.Logf("P5 duplicate function_call_output, openrouter anthropic/claude-sonnet-5: %s", outcome(resp, err))
}

// P6: OpenRouter's Anthropic reasoning (reasoning_text plus signature)
// sent back through Responses: accepted untouched, and rejected when the
// signature is tampered with (which shows it is forwarded and checked).
func TestProbeP6SignatureRoundTripOpenRouterAnthropic(t *testing.T) {
	key := probe(t, "OPENROUTER_API_KEY")
	a := probeAdapter(t, "openrouter", key, "anthropic/claude-sonnet-5", "high")
	ctx, cancel := context.WithTimeout(t.Context(), 3*time.Minute)
	defer cancel()
	req := ullm.Request{Input: []ullm.Item{
		msgItem(ullm.RoleUser, "A bat and a ball cost 1.10 in total. The bat costs 1.00 more than the ball. How much is the ball? Reason first, answer in one sentence."),
	}}
	first, err := a.Respond(ctx, req, ullm.RequestOptions{})
	if err != nil {
		t.Fatalf("first request: %v", err)
	}
	var reasoning []ullm.Item
	for _, it := range first.Output {
		if it.Type == ullm.ItemReasoning {
			reasoning = append(reasoning, it)
		}
	}
	t.Logf("P6 first reply carried %d reasoning items", len(reasoning))
	if len(reasoning) == 0 {
		return
	}
	follow := func(out []ullm.Item) ullm.Request {
		in := append([]ullm.Item(nil), req.Input...)
		in = append(in, out...)
		return ullm.Request{Input: append(in, msgItem(ullm.RoleUser, "And the bat?"))}
	}
	resp, err := a.Respond(ctx, follow(first.Output), ullm.RequestOptions{})
	t.Logf("P6 reasoning sent back untouched: %s", outcome(resp, err))

	tampered := append([]ullm.Item(nil), first.Output...)
	for i, it := range tampered {
		if r, ok := it.Data.(ullm.Reasoning); ok {
			raw := strings.Replace(string(r.Raw), `"signature":"E`, `"signature":"Xbad`, 1)
			r.Raw = []byte(raw)
			tampered[i].Data = r
		}
	}
	resp, err = a.Respond(ctx, follow(tampered), ullm.RequestOptions{})
	t.Logf("P6 reasoning sent back with a tampered signature: %s", outcome(resp, err))
}

// P7: a historical call to a tool that is no longer in tools. Accepted =
// a retired tool can simply be dropped from the list; 400 = tombstoned
// names must stay listed.
func TestProbeP7RetiredToolInHistory(t *testing.T) {
	for _, tc := range []struct{ kind, env, model string }{
		{"openrouter", "OPENROUTER_API_KEY", "anthropic/claude-sonnet-5"},
		{"openrouter", "OPENROUTER_API_KEY", "openai/gpt-5.6-sol"},
		{"openai", "OPENAI_API_KEY", "gpt-5.6-sol"},
	} {
		t.Run(tc.kind+"_"+strings.ReplaceAll(tc.model, "/", "_"), func(t *testing.T) {
			key := probe(t, tc.env)
			a := probeAdapter(t, tc.kind, key, tc.model, "")
			ctx, cancel := context.WithTimeout(t.Context(), 2*time.Minute)
			defer cancel()
			req := ullm.Request{
				Input: []ullm.Item{
					msgItem(ullm.RoleSystem, "Answer in one short sentence."),
					msgItem(ullm.RoleUser, "Use old_tool."),
					{Type: ullm.ItemToolCall, Data: ullm.ToolCall{CallID: "call_p7", Name: "old_tool", Arguments: "{}"}},
					resultItem("call_p7", "old_tool says hello"),
					msgItem(ullm.RoleUser, "What did old_tool say?"),
				},
				Tools: []ullm.Tool{{Type: ullm.ToolFunction, Name: "new_tool", Description: "A different tool.", Parameters: map[string]any{"type": "object", "properties": map[string]any{}}}},
			}
			resp, err := a.Respond(ctx, req, ullm.RequestOptions{})
			t.Logf("P7 retired tool in history, %s %s: %s", tc.kind, tc.model, outcome(resp, err))
		})
	}
}
