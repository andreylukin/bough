package llm

import (
	"strings"
	"testing"
)

// readStream decodes OpenRouter's SSE: comments and blanks skipped,
// deltas concatenated in order, usage from the final chunk tallied,
// [DONE] ends it.
func TestOpenrouterReadStream(t *testing.T) {
	body := strings.Join([]string{
		": OPENROUTER PROCESSING",
		"",
		`data: {"choices":[{"delta":{"content":"Hel"}}]}`,
		`data: {"choices":[{"delta":{"content":"lo"}}]}`,
		`data: {"choices":[{"delta":{}}],"usage":{"prompt_tokens":5,"completion_tokens":2,"cost":0.001}}`,
		"data: [DONE]",
		`data: {"choices":[{"delta":{"content":"IGNORED"}}]}`,
	}, "\n")
	o := &openrouterLLM{model: "m"}
	var got []string
	out, err := o.readStream(strings.NewReader(body), func(d string) { got = append(got, d) })
	if err != nil || out != "Hello" {
		t.Fatalf("readStream = (%q, %v)", out, err)
	}
	if strings.Join(got, "|") != "Hel|lo" {
		t.Fatalf("deltas = %v", got)
	}
	if u := o.Usage(); u.InputTokens != 5 || u.OutputTokens != 2 || u.LastInputTokens != 5 || !u.Priced || u.Cost != 0.001 {
		t.Fatalf("usage = %+v", u)
	}
}

func TestOpenrouterReadStreamError(t *testing.T) {
	body := "data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\ndata: {\"error\":{\"message\":\"provider down\"}}\n"
	o := &openrouterLLM{model: "m"}
	_, err := o.readStream(strings.NewReader(body), func(string) {})
	if err == nil || !strings.Contains(err.Error(), "provider down") {
		t.Fatalf("err = %v", err)
	}
}

// The echo provider streams word by word and returns the whole reply.
func TestEchoStreams(t *testing.T) {
	var got []string
	out, err := echoLLM{}.Stream(nil, "", []Message{{Role: "user", Content: "a b"}}, func(d string) { got = append(got, d) })
	if err != nil || out != "echo: a b" || strings.Join(got, "") != out || len(got) < 3 {
		t.Fatalf("out=%q deltas=%v err=%v", out, got, err)
	}
}
