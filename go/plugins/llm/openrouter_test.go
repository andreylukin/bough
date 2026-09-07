package llm

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
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

// A jsTool model's native call comes back as the fence the loop reads,
// streamed in argument pieces and in a non-streaming body alike.
func TestOpenrouterJSToolCallBecomesFence(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var req map[string]any
		_ = json.NewDecoder(r.Body).Decode(&req)
		if tools, _ := req["tools"].([]any); len(tools) != 1 || req["tool_choice"] != "auto" {
			t.Errorf("request must declare the js tool: %v %v", req["tools"], req["tool_choice"])
		}
		if req["stream"] == true {
			w.Header().Set("Content-Type", "text/event-stream")
			for _, line := range []string{
				`{"choices":[{"delta":{"content":"Looking."}}]}`,
				`{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"js","arguments":"{\"code\":\"console.log(1"}}]}}]}`,
				`{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":")\"}"}}]}}]}`,
				`{"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}`,
			} {
				fmt.Fprintf(w, "data: %s\n\n", line)
			}
			fmt.Fprint(w, "data: [DONE]\n\n")
			return
		}
		fmt.Fprint(w, `{"choices":[{"message":{"role":"assistant","content":"Ok.","tool_calls":[{"function":{"name":"js","arguments":"{\"code\":\"tools.view('a')\"}"}}]}}]}`)
	}))
	defer srv.Close()
	o := &openrouterLLM{model: "google/gemini-3.8-flash", endpoint: srv.URL, key: "k", jsTool: true}
	o.once.Do(func() {})
	var streamed strings.Builder
	got, err := o.Stream(context.Background(), "sys", []Message{{Role: "user", Content: "hi"}}, func(d string) { streamed.WriteString(d) })
	if err != nil {
		t.Fatal(err)
	}
	if want := "Looking.\n```js\nconsole.log(1)\n```\n"; got != want || streamed.String() != want {
		t.Fatalf("stream: %q / %q", got, streamed.String())
	}
	got, err = o.Complete(context.Background(), "sys", []Message{{Role: "user", Content: "hi"}})
	if err != nil {
		t.Fatal(err)
	}
	if want := "Ok.\n```js\ntools.view('a')\n```\n"; got != want {
		t.Fatalf("complete: %q", got)
	}
}
