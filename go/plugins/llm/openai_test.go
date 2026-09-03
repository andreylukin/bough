package llm

import (
	"strings"
	"testing"
)

func TestOpenaiBody(t *testing.T) {
	o := &openaiLLM{model: "gpt-5.6-sol", effort: "xhigh"}
	b := o.body("sys", []Message{{Role: "user", Content: "hi"}, {Role: "assistant", Content: "yo"}}, true)
	if b["model"] != "gpt-5.6-sol" || b["instructions"] != "sys" || b["store"] != false || b["stream"] != true {
		t.Fatalf("body = %v", b)
	}
	if r := b["reasoning"].(map[string]any); r["effort"] != "xhigh" {
		t.Fatalf("reasoning = %v", r)
	}
	in := b["input"].([]map[string]any)
	if len(in) != 2 || in[0]["role"] != "user" || in[1]["role"] != "assistant" || in[1]["content"] != "yo" {
		t.Fatalf("input = %v", in)
	}
	if _, has := (&openaiLLM{model: "m"}).body("", nil, false)["reasoning"]; has {
		t.Fatal("no effort should mean no reasoning field")
	}
}

func TestOpenaiReadStream(t *testing.T) {
	body := strings.Join([]string{
		`event: response.created`,
		`data: {"type":"response.created","response":{"status":"in_progress"}}`,
		``,
		`data: {"type":"response.output_text.delta","delta":"Hel"}`,
		`data: {"type":"response.output_text.delta","delta":"lo"}`,
		`data: {"type":"response.completed","response":{"status":"completed","output":[{"type":"reasoning"},{"type":"message","content":[{"type":"output_text","text":"Hello"}]}],"usage":{"input_tokens":7,"output_tokens":3}}}`,
	}, "\n")
	o := &openaiLLM{model: "m"}
	var got []string
	out, err := o.readStream(strings.NewReader(body), func(d string) { got = append(got, d) })
	if err != nil || out != "Hello" || strings.Join(got, "|") != "Hel|lo" {
		t.Fatalf("readStream = (%q, %v) deltas=%v", out, err, got)
	}
	if u := o.Usage(); u.InputTokens != 7 || u.OutputTokens != 3 || u.LastInputTokens != 7 || u.Priced {
		t.Fatalf("usage = %+v", u)
	}
	_, err = o.readStream(strings.NewReader(`data: {"type":"response.failed","response":{"status":"failed","error":{"message":"quota"}}}`), func(string) {})
	if err == nil || !strings.Contains(err.Error(), "quota") {
		t.Fatalf("failed event = %v", err)
	}
	_, err = o.readStream(strings.NewReader(`data: {"type":"response.output_text.delta","delta":"x"}`), func(string) {})
	if err == nil || !strings.Contains(err.Error(), "without response.completed") {
		t.Fatalf("cut stream = %v", err)
	}
}

func TestOpenaiErr(t *testing.T) {
	err := openaiErr(404, "nope", []byte(`{"error":{"message":"The model nope does not exist","type":"x","user_id":"u"}}`))
	if !strings.Contains(err.Error(), `model "nope" not found`) || strings.Contains(err.Error(), "user_id") {
		t.Fatalf("404 = %v", err)
	}
	if err := openaiErr(429, "m", []byte(`{"error":{"message":"rate limited"}}`)); !strings.Contains(err.Error(), "HTTP 429: rate limited") {
		t.Fatalf("429 = %v", err)
	}
}
