package llm

import (
	"strings"
	"testing"

	"github.com/andreylukin/bough/kernel"
)

func TestEchoComplete(t *testing.T) {
	var e echoLLM

	got, err := e.Complete(t.Context(), "sys", []Message{
		{Role: "user", Content: "hello"},
		{Role: "assistant", Content: "hi"},
		{Role: "user", Content: "world"},
	})
	if err != nil {
		t.Fatal(err)
	}
	if got != "echo: world" {
		t.Errorf("got %q, want %q", got, "echo: world")
	}

	got, err = e.Complete(t.Context(), "", []Message{
		{Role: "user", Content: "please CODE! now"},
	})
	if err != nil {
		t.Fatal(err)
	}
	want := "```js\ntools.bash(\"echo hi from codemode\")\n```"
	if got != want {
		t.Errorf("got %q, want %q", got, want)
	}
}

func TestEchoProvidesLLM(t *testing.T) {
	ctx := kernel.NewContext()
	if err := (&echoPlugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	svc, err := kernel.Get[LLM](ctx, "llm")
	if err != nil {
		t.Fatal(err)
	}
	got, err := svc.Complete(t.Context(), "", []Message{{Role: "user", Content: "x"}})
	if err != nil {
		t.Fatal(err)
	}
	if got != "echo: x" {
		t.Errorf("got %q", got)
	}
}

func TestUsageStrings(t *testing.T) {
	if got := (Usage{}).Short(); got != "" {
		t.Errorf("empty usage Short = %q", got)
	}
	u := Usage{InputTokens: 1500, OutputTokens: 20}
	if got := u.Short(); got != "1.5k tok" {
		t.Errorf("unpriced Short = %q", got)
	}
	u.Priced, u.Cost = true, 0.0123
	if got := u.Short(); got != "$0.0123 · 1.5k tok" {
		t.Errorf("priced Short = %q", got)
	}
	if got := u.String(); got != "1.5k in · 20 out · $0.0123" {
		t.Errorf("String = %q", got)
	}
}

func TestOpenrouterErrHidesRawBody(t *testing.T) {
	body := []byte(`{"error":{"message":"foo/bar is not a valid model ID","code":400},"user_id":"user_secret"}`)
	err := openrouterErr(400, "foo/bar", body)
	if !strings.Contains(err.Error(), `model "foo/bar" not found on openrouter`) ||
		strings.Contains(err.Error(), "user_secret") {
		t.Errorf("400 error = %v", err)
	}
	if err := openrouterErr(500, "m", []byte("nope")); !strings.Contains(err.Error(), "HTTP 500") {
		t.Errorf("500 error = %v", err)
	}
}

func TestCerebrasErrHidesRawBody(t *testing.T) {
	body := []byte(`{"message":"Model qwen-9 does not exist","type":"not_found_error","user_id":"u_1"}`)
	err := cerebrasErr(404, "qwen-9", body)
	if !strings.Contains(err.Error(), `model "qwen-9" not found on cerebras`) ||
		!strings.Contains(err.Error(), "does not exist") || strings.Contains(err.Error(), "u_1") {
		t.Fatalf("got %v", err)
	}
	if err := cerebrasErr(500, "m", []byte("nope")); !strings.Contains(err.Error(), "HTTP 500") {
		t.Fatalf("got %v", err)
	}
}
