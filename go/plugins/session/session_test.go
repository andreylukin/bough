package session

import (
	"context"
	"path/filepath"
	"testing"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

type stubLLM struct {
	u     llm.Usage
	model string
}

func (s *stubLLM) Complete(context.Context, string, []llm.Message) (string, error) { return "", nil }
func (s *stubLLM) Usage() llm.Usage                                                 { return s.u }
func (s *stubLLM) Model() string                                                    { return s.model }

type stubCost struct{ stubLLM }

func (c *stubCost) ContextLimit() int { return 200_000 }

// Info gathers what the other rows know, at call time: the history
// file names the session, its entries give title/turns/cwd, the llm
// row the model, the cost row the priced tally and the context.
func TestInfoGathersFromMountedRows(t *testing.T) {
	ctx := kernel.NewContext()
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	svc, err := kernel.Get[*Service](ctx, "session")
	if err != nil {
		t.Fatal(err)
	}
	// Nothing mounted yet: zero values, no panic.
	if i := svc.Info(); i.ID != "" || i.Model != "" || i.Turns != 0 || i.Cwd == "" {
		t.Fatalf("empty: %+v", i)
	}

	h, err := history.Open(filepath.Join(t.TempDir(), "01a0-abc.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	h.Append("meta", map[string]any{"cwd": "/work/proj"})
	h.Append("input", map[string]any{"text": "hi"})
	h.Append("done", map[string]any{})
	h.Append("title", map[string]any{"text": "Say hello"})
	h.Append("input", map[string]any{"text": "again"})
	h.Append("done", map[string]any{})
	ctx.Provide("history", h)
	ctx.Provide("llm", &stubLLM{model: "openai/gpt-5", u: llm.Usage{InputTokens: 10}})
	ctx.Provide("usage", &stubCost{stubLLM{u: llm.Usage{InputTokens: 1500, OutputTokens: 300, CacheReadTokens: 900, LastInputTokens: 50_000, Cost: 0.25, Priced: true}}})

	i := svc.Info()
	if i.ID != "01a0-abc" || i.Title != "Say hello" || i.Turns != 2 || i.Cwd != "/work/proj" || i.Started.IsZero() {
		t.Fatalf("history facts: %+v", i)
	}
	if i.Model != "openai/gpt-5" {
		t.Fatalf("model: %+v", i)
	}
	if i.Usage.In != 1500 || i.Usage.CacheRead != 900 || i.Usage.Cost != 0.25 || !i.Usage.Priced {
		t.Fatalf("usage should be the cost row's: %+v", i.Usage)
	}
	if i.ContextLimit != 200_000 || i.ContextPct != 25 {
		t.Fatalf("context: %+v", i)
	}
	m := svc.Map()
	if m["id"] != "01a0-abc" || m["turns"] != 2 || m["usage"].(map[string]any)["cost"] != 0.25 || m["context_pct"] != 25.0 {
		t.Fatalf("map: %v", m)
	}
	if err := (plugin{}).Apply(kernel.NewContext(), map[string]any{"nope": 1}); err == nil {
		t.Fatal("unknown config key must fail the mount")
	}
}
