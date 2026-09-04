package llm

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
)

// The system prompt goes out as a cache-marked text part, the same
// marker llm-anthropic sends natively; user/assistant messages stay
// plain strings.
func TestOpenrouterSystemCacheControl(t *testing.T) {
	var got struct {
		Messages []struct {
			Role    string          `json:"role"`
			Content json.RawMessage `json:"content"`
		} `json:"messages"`
	}
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_ = json.NewDecoder(r.Body).Decode(&got)
		_, _ = w.Write([]byte(`{"choices":[{"message":{"content":"ok"}}],"usage":{"prompt_tokens":1,"completion_tokens":1,"cost":0}}`))
	}))
	defer srv.Close()
	o := &openrouterLLM{model: "m", key: "k"}
	o.once.Do(func() {})
	o.endpoint = srv.URL
	if _, err := o.Complete(t.Context(), "sys", []Message{{Role: "user", Content: "hi"}}); err != nil {
		t.Fatal(err)
	}
	if len(got.Messages) != 2 || got.Messages[0].Role != "system" {
		t.Fatalf("messages = %+v", got.Messages)
	}
	var parts []orPart
	if err := json.Unmarshal(got.Messages[0].Content, &parts); err != nil {
		t.Fatalf("system content is not parts: %v (%s)", err, got.Messages[0].Content)
	}
	if len(parts) != 1 || parts[0].Text != "sys" || parts[0].CacheControl == nil || parts[0].CacheControl.Type != "ephemeral" {
		t.Fatalf("system parts = %+v", parts)
	}
	var content string
	if err := json.Unmarshal(got.Messages[1].Content, &content); err != nil {
		t.Fatalf("user content is not a string: %v", err)
	}
	if content != "hi" {
		t.Fatalf("user content = %q", content)
	}
}

// prompt_tokens_details must land in the Cache* fields — via the
// streaming path here, parse() shares addUsage with it.
func TestOpenrouterStreamUsageCache(t *testing.T) {
	body := "data: " + `{"choices":[{"delta":{"content":"ok"}}],"usage":{"prompt_tokens":100,"completion_tokens":5,"cost":0.001,"prompt_tokens_details":{"cached_tokens":90,"cache_write_tokens":10}}}` + "\n\n" + "data: [DONE]\n"
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte(body))
	}))
	defer srv.Close()
	o := &openrouterLLM{model: "m", key: "k"}
	o.once.Do(func() {})
	o.endpoint = srv.URL
	out, err := o.Stream(t.Context(), "sys", []Message{{Role: "user", Content: "hi"}}, func(string) {})
	if err != nil || out != "ok" {
		t.Fatalf("Stream = (%q, %v)", out, err)
	}
	u := o.Usage()
	if u.InputTokens != 100 || u.OutputTokens != 5 || u.CacheReadTokens != 90 || u.CacheCreationTokens != 10 {
		t.Fatalf("usage = %+v, want in 100, out 5, read 90, write 10", u)
	}
	if u.Cached() != 90 {
		t.Fatalf("Cached = %d, want 90", u.Cached())
	}
}
