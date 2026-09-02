package llm

import (
	"context"
	"testing"

	"github.com/andreylukin/bough/kernel"
)

func TestEchoComplete(t *testing.T) {
	var e echoLLM

	got, err := e.Complete(context.Background(), "sys", []Message{
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

	got, err = e.Complete(context.Background(), "", []Message{
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
	got, err := svc.Complete(context.Background(), "", []Message{{Role: "user", Content: "x"}})
	if err != nil {
		t.Fatal(err)
	}
	if got != "echo: x" {
		t.Errorf("got %q", got)
	}
}
