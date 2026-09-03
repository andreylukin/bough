package cost

import (
	"context"
	"math"
	"testing"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"
)

func TestCanonicalStripsPrefixAndDate(t *testing.T) {
	cases := map[string]string{
		"openai/gpt-5-mini":          "gpt-5-mini",
		"claude-sonnet-4-6-20251114": "claude-sonnet-4-6",
		"anthropic/claude-opus-5":    "claude-opus-5",
		" GPT-5 ":                    "gpt-5",
		"z-ai/glm-5.3-flash":         "glm-5.3-flash",
		"claude-haiku-4-5-20251001":  "claude-haiku-4-5",
		"gpt-4.1-2025-04-14":         "gpt-4.1-2025-04-14", // not the 8-digit shape: left alone
		"":                           "",
	}
	for in, want := range cases {
		if got := Canonical(in); got != want {
			t.Errorf("Canonical(%q) = %q, want %q", in, got, want)
		}
	}
}

func TestLookupExactPrefixAndOverride(t *testing.T) {
	var none Table
	if p, ok := none.Lookup("claude-opus-5"); !ok || p.Input != 5 || p.Output != 25 {
		t.Fatalf("built-in opus 5: %+v %v", p, ok)
	}
	if p, ok := none.Lookup("openai/gpt-5-mini-2026-01-01"); !ok || p.Input != 0.25 {
		t.Fatalf("longest-prefix match: %+v %v", p, ok)
	}
	if p, ok := none.Lookup("gpt-5-nano"); !ok || p.Output != 0.40 {
		t.Fatalf("gpt-5-nano must not fall back to gpt-5's price: %+v %v", p, ok)
	}
	if _, ok := none.Lookup("z-ai/glm-5.3-flash"); ok {
		t.Fatal("an unknown model is unpriced, not guessed")
	}
	own := Table{"gpt-5-mini": {1, 2}, "glm-5.3-flash": {0.075, 0.25}}
	if p, _ := own.Lookup("openai/gpt-5-mini"); p.Input != 1 {
		t.Fatalf("row prices override built-ins: %+v", p)
	}
	if p, ok := own.Lookup("z-ai/glm-5.3-flash"); !ok || p.Output != 0.25 {
		t.Fatalf("row prices add models: %+v %v", p, ok)
	}
}

type stubLLM struct {
	u     llm.Usage
	model string
}

func (s *stubLLM) Complete(ctx context.Context, system string, m []llm.Message) (string, error) {
	return "", nil
}
func (s *stubLLM) Usage() llm.Usage { return s.u }
func (s *stubLLM) Model() string    { return s.model }

func TestServicePricesTokensAndPassesPricedThrough(t *testing.T) {
	l := &stubLLM{u: llm.Usage{InputTokens: 1_000_000, OutputTokens: 100_000}, model: "gpt-5-mini"}
	s := &Service{rep: l, model: l.Model, table: Table{}}
	u := s.Usage()
	if !u.Priced || math.Abs(u.Cost-(0.25+0.2)) > 1e-9 {
		t.Fatalf("priced from the table: %+v", u)
	}
	if u.Short() != "$0.4500 · 1.1M tok" && u.Short() != "$0.4500 · 1100.0k tok" {
		t.Fatalf("status bar text: %q", u.Short())
	}
	if s.Source() != "the cost table for gpt-5-mini" {
		t.Fatalf("source: %q", s.Source())
	}

	l.u.Priced, l.u.Cost = true, 0.0123 // OpenRouter: its own number wins
	if u := s.Usage(); u.Cost != 0.0123 {
		t.Fatalf("provider price must pass through: %+v", u)
	}
	if s.Source() != "the provider's own per-response price" {
		t.Fatalf("source: %q", s.Source())
	}

	l.u.Priced, l.model = false, "mystery-9000"
	if u := s.Usage(); u.Priced || u.Short() != "1.1M tok" && u.Short() != "1100.0k tok" {
		t.Fatalf("unknown model stays a token count: %+v %q", u, u.Short())
	}
}

func TestParsePrices(t *testing.T) {
	tbl, err := ParsePrices(map[string]any{"openai/gpt-5-mini": map[string]any{"input": 0.3, "output": 2}})
	if err != nil || tbl["gpt-5-mini"].Output != 2 {
		t.Fatalf("parse: %v %+v", err, tbl)
	}
	if _, err := ParsePrices(map[string]any{"x": map[string]any{"input": "cheap"}}); err == nil {
		t.Fatal("a non-numeric price must be rejected")
	}
	if _, err := ParsePrices([]any{1}); err == nil {
		t.Fatal("a non-map prices must be rejected")
	}
}

func TestMountProvidesUsageOverTheLLM(t *testing.T) {
	ctx := kernel.NewContext()
	ctx.Provide("llm", &stubLLM{u: llm.Usage{InputTokens: 2_000_000}, model: "claude-sonnet-5"})
	if err := (plugin{}).Apply(ctx, map[string]any{"prices": map[string]any{}}); err != nil {
		t.Fatal(err)
	}
	rep, err := kernel.Get[llm.UsageReporter](ctx, "usage")
	if err != nil {
		t.Fatal(err)
	}
	if u := rep.Usage(); !u.Priced || u.Cost != 4 {
		t.Fatalf("sonnet 5 at $2/Mtok over 2M in: %+v", u)
	}
	if err := (plugin{}).Apply(kernel.NewContext(), map[string]any{"nope": 1}); err == nil {
		t.Fatal("unknown config key must fail the mount")
	}
}
