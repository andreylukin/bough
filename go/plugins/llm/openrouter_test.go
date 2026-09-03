package llm

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
)

// An effort config rides along as OpenRouter's reasoning.effort; the
// default sends no reasoning object at all.
func TestOpenrouterEffortInBody(t *testing.T) {
	for _, effort := range []string{"", "high"} {
		var got map[string]any
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			_ = json.NewDecoder(r.Body).Decode(&got)
			_, _ = w.Write([]byte(`{"choices":[{"message":{"content":"ok"}}],"usage":{"prompt_tokens":1,"completion_tokens":1,"cost":0}}`))
		}))
		defer srv.Close()
		o := &openrouterLLM{model: "m", effort: effort, key: "k"}
		o.once.Do(func() {})
		o.endpoint = srv.URL
		if _, err := o.Complete(t.Context(), "sys", []Message{{Role: "user", Content: "hi"}}); err != nil {
			t.Fatalf("effort %q: %v", effort, err)
		}
		r, has := got["reasoning"]
		if effort == "" && has {
			t.Fatalf("no effort configured, but body has reasoning %v", r)
		}
		if effort != "" && (!has || r.(map[string]any)["effort"] != effort) {
			t.Fatalf("effort %q not in body: %v", effort, got)
		}
	}
}
