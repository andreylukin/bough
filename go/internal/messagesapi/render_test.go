//go:build !windows

package messagesapi

import (
	stdjson "encoding/json"
	"path/filepath"
	"strings"
	"testing"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/unreal/fake"
)

const pngURL = "data:image/png;base64,iVBORw0KGgo="
const jpegURL = "data:image/jpeg;base64,/9j/4AAQ"

func call(id, name, args string) ullm.Item { return fake.Call(id, name, args) }

func result(id string, outs ...ullm.ToolResultOutput) ullm.Item {
	return ullm.Item{Type: ullm.ItemToolResult, Data: ullm.ToolResult{CallID: id, Output: outs}}
}

func text(s string) ullm.ToolResultOutput {
	return ullm.ToolResultOutput{Kind: ullm.ToolResultText, Value: s}
}

func image(v string) ullm.ToolResultOutput {
	return ullm.ToolResultOutput{Kind: ullm.ToolResultImage, Value: v}
}

func assistant(s string) ullm.Item { return fake.Text(s) }

const placeholder = "This call is still running. Its result arrives later."

var bash = ullm.Tool{Type: ullm.ToolFunction, Name: "bash", Description: "Run a command.", Parameters: map[string]any{
	"type": "object", "properties": map[string]any{"command": map[string]any{"type": "string"}}, "required": []any{"command"},
}}

// renderCases are the shapes the design names in §15.1: the late-result
// rule, missing results, dropped reasoning, arguments, images, markers,
// and runs merged across an empty response.
var renderCases = map[string]ullm.Request{
	"placeholder_then_late": {Input: []ullm.Item{
		system("sys"), userText("run it"),
		call("toolu_1", "bash", `{"command":"sleep 5"}`),
		result("toolu_1", text(placeholder)),
		assistant("Waiting for it."),
		result("toolu_1", text("done after 5s")),
	}, Tools: []ullm.Tool{bash}},
	"parallel_one_late": {Input: []ullm.Item{
		system("sys"), userText("two things"),
		fake.Think("plan both"),
		call("toolu_a", "bash", `{"command":"ls"}`),
		call("toolu_b", "bash", `{"command":"make"}`),
		result("toolu_a", text("a.go b.go")),
		result("toolu_b", text(placeholder)),
		assistant("ls is back, make is running."),
		userText("any news?"),
		result("toolu_b", text("make: ok")),
	}, Tools: []ullm.Tool{bash}},
	"missing_result": {Input: []ullm.Item{
		system("sys"), userText("go"),
		call("toolu_x", "bash", `{"command":"true"}`),
		call("toolu_y", "bash", `{"command":"false"}`),
		result("toolu_x", text("ok")),
	}},
	"foreign_reasoning_dropped": {Input: []ullm.Item{
		system("sys"), userText("hi"),
		{Type: ullm.ItemReasoning, Data: ullm.Reasoning{Summary: []string{"gpt thoughts"}, Raw: []byte(`{"type":"reasoning","encrypted_content":"gAAAA"}`)}},
		{Type: ullm.ItemReasoning, Data: ullm.Reasoning{Raw: []byte(`{"type":"thinking","thinking":"unsigned"}`)}},
		{Type: ullm.ItemReasoning, Data: ullm.Reasoning{Raw: []byte(`{"type":"redacted_thinking","data":"EmwKAhgB"}`)}},
		assistant("hello"),
		userText("again"),
	}},
	"invalid_args": {Input: []ullm.Item{
		system("sys"), userText("go"),
		call("toolu_i", "bash", `{"command": "echo hi"`),
		call("toolu_j", "bash", `["not","an","object"]`),
		result("toolu_i", text("Error: arguments must be a JSON object")),
		result("toolu_j", text("Error: arguments must be a JSON object")),
	}},
	"images": {Input: []ullm.Item{
		system("sys"), userText("look"),
		call("toolu_p", "view_image", `{"path":"a.png"}`),
		call("toolu_q", "view_image", `{"path":"b.jpg"}`),
		result("toolu_p", image(pngURL)),
		result("toolu_q", text(placeholder)),
		assistant("One is here."),
		result("toolu_q", text("b.jpg"), image(jpegURL)),
	}},
	"unsupported_image": {Input: []ullm.Item{
		system("sys"), userText("look"),
		call("toolu_w", "view_image", `{"path":"a.webp"}`),
		result("toolu_w", image("data:image/webp;base64,UklGRg==")),
	}},
	"markers": {Input: []ullm.Item{
		system("sys"), userText("one"), assistant("first"),
		userText("two"), assistant("second"),
		userText("three"),
	}},
	"merged_after_muted": {Input: []ullm.Item{
		// A muted (empty) response adds no items, so the user-side runs
		// on either side of it are one message on the wire.
		system("sys"), userText("start"),
		call("toolu_m", "bash", `{"command":"sleep 60"}`),
		result("toolu_m", text(placeholder)),
		userText("steer: also check the logs"),
		result("toolu_m", text("slept")),
	}},
	"late_system_message": {Input: []ullm.Item{
		system("sys"), userText("one"), assistant("ok"),
		{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleSystem, Text: "AGENTS.md changed"}},
		userText("two"),
	}},
}

// Render goldens: the full request JSON and the betas, per case, on the
// default model (Sonnet 5, adaptive + summarized) at effort high.
func TestRenderGolden(t *testing.T) {
	t.Parallel()
	for name, req := range renderCases {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			req.Model = ullm.Model{ID: "claude-sonnet-5", ReasoningEffort: ullm.ReasoningEffortHigh}
			p, err := Render(req, Spec("claude-sonnet-5"), "1h")
			if err != nil {
				t.Fatal(err)
			}
			b, err := stdjson.Marshal(p)
			if err != nil {
				t.Fatal(err)
			}
			out := pretty(t, b)
			out = append(out, []byte("--- betas "+strings.Join(betaStrings(p.Betas), ",")+"\n")...)
			golden(t, filepath.Join("testdata", "render", name+".json"), out)
		})
	}
}

// The rules the goldens encode, stated as assertions.
func TestRenderRules(t *testing.T) {
	t.Parallel()
	render := func(name string) string {
		t.Helper()
		req := renderCases[name]
		req.Model.ID = "claude-sonnet-5"
		p, err := Render(req, Spec("claude-sonnet-5"), "1h")
		if err != nil {
			t.Fatal(err)
		}
		b, err := stdjson.Marshal(p)
		if err != nil {
			t.Fatal(err)
		}
		// The SDK escapes <, > and & itself; read the text as sent.
		u := func(hex string) string { return "\\" + "u" + hex }
		return strings.NewReplacer(u("003c"), "<", u("003e"), ">", u("0026"), "&").Replace(string(b))
	}
	if s := render("placeholder_then_late"); strings.Count(s, `"type":"tool_result"`) != 1 || !strings.Contains(s, `<tool_result call_id=\"toolu_1\" name=\"bash\">\ndone after 5s\n</tool_result>`) {
		t.Errorf("the late result is text after the placeholder, which stays:\n%s", s)
	}
	if s := render("missing_result"); !strings.Contains(s, `"is_error":true`) || !strings.Contains(s, missingResult) {
		t.Errorf("a call with no result gets a synthesized error result:\n%s", s)
	}
	if s := render("foreign_reasoning_dropped"); strings.Contains(s, "gAAAA") || strings.Contains(s, "unsigned") || !strings.Contains(s, "EmwKAhgB") {
		t.Errorf("only signed and redacted thinking survive:\n%s", s)
	}
	if s := render("invalid_args"); !strings.Contains(s, `{"invalid_arguments":"{\"command\": \"echo hi\""}`) {
		t.Errorf("invalid arguments are wrapped, not dropped:\n%s", s)
	}
	if s := render("unsupported_image"); !strings.Contains(s, "[image omitted: unsupported image/webp]") {
		t.Errorf("an unsupported image becomes a note:\n%s", s)
	}
	if s := render("markers"); strings.Count(s, `"cache_control"`) != 3 {
		t.Errorf("want exactly three markers (system, previous tail, tail):\n%s", s)
	}
}

func betaStrings[T ~string](bs []T) []string {
	out := make([]string, len(bs))
	for i, b := range bs {
		out[i] = string(b)
	}
	return out
}

func TestRenderRefusesPrefillAndEmpty(t *testing.T) {
	t.Parallel()
	if _, err := Render(ullm.Request{Input: []ullm.Item{system("s"), userText("a"), assistant("b")}}, Spec("claude-sonnet-5"), "1h"); err == nil {
		t.Error("a request ending on the assistant is prefill, a 400")
	}
	if _, err := Render(ullm.Request{Input: []ullm.Item{system("s")}}, Spec("claude-sonnet-5"), "1h"); err == nil {
		t.Error("a request with no messages is refused")
	}
	if _, err := Render(ullm.Request{Input: []ullm.Item{system("s"), assistant("b"), userText("a")}}, Spec("claude-sonnet-5"), "1h"); err == nil {
		t.Error("the first message must be the user's")
	}
	if _, err := Render(ullm.Request{Input: []ullm.Item{system("s"), userText("a")}, Tools: []ullm.Tool{{Type: ullm.ToolHosted, Name: "web_search"}}}, Spec("claude-sonnet-5"), "1h"); err == nil {
		t.Error("a hosted tool is refused")
	}
}

// Thinking and effort per model family (§7.2 table and §7.5).
func TestRenderThinkingPerModel(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		model, level string
		want         []string // substrings of the request JSON
		not          []string
		betas        string
	}{
		{"claude-opus-5-5", "", []string{`"effort":"high"`, `"display":"updates"`, `"type":"adaptive"`}, nil, "thinking-display-updates-2026-08-18"},
		{"claude-opus-5-5", "off", []string{`"effort":"low"`}, []string{`"disabled"`}, "thinking-display-updates-2026-08-18"},
		{"claude-opus-5", "max", []string{`"effort":"max"`, `"display":"summarized"`}, nil, ""},
		{"claude-sonnet-5", "", []string{`"type":"adaptive"`}, []string{`"effort"`}, ""},
		{"claude-opus-4-6", "xhigh", []string{`"effort":"high"`, `"thinking":{"type":"adaptive"}`}, []string{`"display"`}, ""},
		{"claude-opus-4-5", "medium", []string{`"budget_tokens":8192`, `"effort":"medium"`}, nil, ""},
		{"claude-haiku-4-5", "xhigh", []string{`"budget_tokens":32768`}, []string{`"effort"`}, "interleaved-thinking-2025-05-14"},
		{"claude-haiku-4-5", "", nil, []string{`"thinking"`, `"effort"`}, "interleaved-thinking-2025-05-14"},
		{"claude-haiku-4-5", "off", nil, []string{`"thinking"`}, "interleaved-thinking-2025-05-14"},
		{"claude-future-9", "high", []string{`"effort":"high"`, `"display":"summarized"`}, nil, ""},
	} {
		spec := Spec(tc.model)
		req := simpleRequest()
		req.Model = ullm.Model{ID: tc.model, ReasoningEffort: EffortFor(tc.level, spec)}
		p, err := Render(req, spec, "1h")
		if err != nil {
			t.Fatal(err)
		}
		b, _ := stdjson.Marshal(p)
		s := string(b)
		for _, w := range tc.want {
			if !strings.Contains(s, w) {
				t.Errorf("%s at %q: want %s in %s", tc.model, tc.level, w, s)
			}
		}
		for _, w := range tc.not {
			if strings.Contains(s, w) {
				t.Errorf("%s at %q: do not want %s in %s", tc.model, tc.level, w, s)
			}
		}
		if got := strings.Join(betaStrings(p.Betas), ","); got != tc.betas {
			t.Errorf("%s at %q: betas %q, want %q", tc.model, tc.level, got, tc.betas)
		}
	}
}

// Budgets stay under max_tokens, and a cap too small to think in sends
// no thinking at all.
func TestBudgetCappedByMaxTokens(t *testing.T) {
	t.Parallel()
	spec := Spec("claude-haiku-4-5")
	req := simpleRequest()
	n := int64(4000)
	req.Model = ullm.Model{ID: "claude-haiku-4-5", ReasoningEffort: ullm.ReasoningEffortHigh, MaxOutputTokens: &n}
	p, _ := Render(req, spec, "1h")
	if p.Thinking.OfEnabled == nil || p.Thinking.OfEnabled.BudgetTokens != 3999 {
		t.Errorf("budget = %+v, want 3999", p.Thinking.OfEnabled)
	}
	n = 1000
	p, _ = Render(req, spec, "1h")
	if p.Thinking.OfEnabled != nil {
		t.Errorf("no room to think below 1025 max_tokens: %+v", p.Thinking.OfEnabled)
	}
	big := int64(500000)
	req.Model.MaxOutputTokens = &big
	p, _ = Render(req, spec, "1h")
	if p.MaxTokens != 64000 {
		t.Errorf("max_tokens = %d, want the model's cap 64000", p.MaxTokens)
	}
}

func TestSpecPrefixes(t *testing.T) {
	t.Parallel()
	for model, family := range map[string]string{
		"claude-opus-5-5":          "claude-opus-5-5",
		"claude-opus-5":            "claude-opus-5",
		"claude-opus-5-20260401":   "claude-opus-5",
		"claude-fable-5-1":         "claude-fable-5-1",
		"claude-fable-5":           "claude-fable-5",
		"claude-haiku-4-5-2025100": "claude-haiku-4-5",
		"claude-unknown":           "claude-opus-5",
	} {
		if got := Spec(model); got.Family != family || got.ID != model {
			t.Errorf("Spec(%q) = %s/%s, want family %s", model, got.ID, got.Family, family)
		}
	}
}

func TestClamp(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		want     string
		accepted []string
		out      string
	}{
		{"xhigh", effortsNoX, "high"},
		{"max", effortsThree, "high"},
		{"max", allEfforts, "max"},
		{"low", []string{"medium", "high"}, "medium"},
		{"high", nil, "high"},
	} {
		if got := Clamp(tc.want, tc.accepted); got != tc.out {
			t.Errorf("Clamp(%q, %v) = %q, want %q", tc.want, tc.accepted, got, tc.out)
		}
	}
}
