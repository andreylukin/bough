// Package cost is the "cost" plugin: a dollar figure in the status bar
// for every provider, not only the one that prices its own responses.
//
// It provides the "usage" service (llm.UsageReporter). When the llm row
// already reports a price (OpenRouter does, per response) that passes
// through untouched. Otherwise the provider's token tally is priced
// from a table: the row's `prices` config first, then the built-in list
// of first-party OpenAI and Anthropic rates, matched on the model id
// with its provider prefix and date suffix stripped. An unknown model
// stays unpriced, and the bar shows tokens as before.
package cost

import (
	"fmt"
	"maps"
	"slices"
	"strings"

	"github.com/andreylukin/bough/internal/models"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"
)

// Price is USD per million tokens.
type Price struct {
	Input  float64
	Output float64
}

// builtin is first-party list pricing, USD per million tokens, as of
// 2026-06 (Anthropic) and the GPT-5 launch (OpenAI). A `prices` entry
// in the row config overrides any of these. Keys are the canonical
// model id: no provider prefix, no date suffix.
var builtin = map[string]Price{
	// Anthropic (first-party API rates).
	"claude-fable-5-1":  {10, 50},
	"claude-fable-5":    {10, 50},
	"claude-opus-5":     {5, 25},
	"claude-opus-4-8":   {5, 25},
	"claude-opus-4-7":   {5, 25},
	"claude-opus-4-6":   {5, 25},
	"claude-sonnet-5":   {2, 10},
	"claude-sonnet-4-6": {3, 15},
	"claude-haiku-4-5":  {1, 5},
	// OpenAI.
	"gpt-5":        {1.25, 10},
	"gpt-5-mini":   {0.25, 2},
	"gpt-5-nano":   {0.05, 0.40},
	"gpt-4.1":      {2, 8},
	"gpt-4.1-mini": {0.40, 1.60},
	"gpt-4.1-nano": {0.10, 0.40},
	"gpt-4o":       {2.50, 10},
	"gpt-4o-mini":  {0.15, 0.60},
}

// contexts is the context window in tokens for the models whose limit
// is known; the status bar's "N% ctx" needs one. Keys are canonical
// model ids like builtin's. Unknown stays 0 and the bar shows no
// percentage.
var contexts = map[string]int{
	// Anthropic: the 4.6+/5 line at 1M, haiku 4.5 and the 4.x line
	// before it at 200k.
	"claude-fable-5-1":  1_000_000,
	"claude-fable-5":    1_000_000,
	"claude-opus-5":     1_000_000,
	"claude-opus-4-8":   1_000_000,
	"claude-opus-4-7":   1_000_000,
	"claude-opus-4-6":   1_000_000,
	"claude-sonnet-5":   1_000_000,
	"claude-sonnet-4-6": 1_000_000,
	"claude-haiku-4-5":  200_000,
	"claude-sonnet-4-5": 200_000,
	"claude-opus-4-5":   200_000,
	"claude-opus-4-1":   200_000,
	"claude-opus-4":     200_000,
	"claude-sonnet-4":   200_000,
	// OpenAI.
	"gpt-5.6-luna": 1_050_000,
	"gpt-5":        400_000,
	"gpt-5-mini":   400_000,
	"gpt-5-nano":   400_000,
	"gpt-4.1":      1_047_576,
	"gpt-4.1-mini": 1_047_576,
	"gpt-4.1-nano": 1_047_576,
	"gpt-4o":       128_000,
	"gpt-4o-mini":  128_000,
}

// ContextLimit is a model's context window in tokens, matched like
// Lookup (exact canonical id, then the longest key the id starts
// with); 0 when unknown. Pure.
func ContextLimit(model string) int {
	c := Canonical(model)
	if n, ok := contexts[c]; ok {
		return n
	}
	best, out := "", 0
	for k, n := range contexts {
		if strings.HasPrefix(c, k+"-") && len(k) > len(best) {
			best, out = k, n
		}
	}
	return out
}

// Canonical strips what providers hang on a model id: a "vendor/"
// prefix (OpenRouter), a "-YYYYMMDD" date suffix (Anthropic snapshots),
// and case. Pure.
func Canonical(model string) string {
	m := strings.ToLower(strings.TrimSpace(model))
	if _, after, ok := strings.CutLast(m, "/"); ok {
		m = after
	}
	if n := len(m); n > 9 && m[n-9] == '-' && allDigits(m[n-8:]) {
		m = m[:n-9]
	}
	return m
}

func allDigits(s string) bool {
	for _, r := range s {
		if r < '0' || r > '9' {
			return false
		}
	}
	return s != ""
}

// Table is a price lookup: the row's own entries over the built-ins.
type Table map[string]Price

// Lookup finds the price for a model id, exact canonical match first,
// then the longest table key the id starts with (so "gpt-5-mini-2026"
// and "claude-opus-5-fast" still price). ok=false when nothing fits.
func (t Table) Lookup(model string) (Price, bool) {
	c := Canonical(model)
	if c == "" {
		return Price{}, false
	}
	if p, ok := t[c]; ok {
		return p, true
	}
	if p, ok := builtin[c]; ok {
		return p, true
	}
	best, found := "", false
	var out Price
	for _, src := range []map[string]Price{t, builtin} {
		for k, p := range src {
			if strings.HasPrefix(c, k+"-") && len(k) > len(best) {
				best, out, found = k, p, true
			}
		}
	}
	return out, found
}

// Cost prices a token count. Pure.
func (p Price) Cost(in, out int) float64 {
	return float64(in)*p.Input/1e6 + float64(out)*p.Output/1e6
}

// Service is the "usage" provider: the llm's tally, priced.
type Service struct {
	rep    llm.UsageReporter
	model  func() string
	plugin func() string // the llm row's plugin, for the catalogue
	table  Table
}

// llmPlugin is the llm row's plugin name, "" when the service was
// built without one (a bare Service in a test).
func (s *Service) llmPlugin() string {
	if s.plugin == nil {
		return ""
	}
	return s.plugin()
}

// lookup finds the price for the current model and says where it came
// from: the row's own `prices` first (a person overriding on purpose),
// then the model catalogue (models.dev, refreshed weekly), then the
// built-in table that predates it.
func (s *Service) lookup() (models.Model, string, bool) {
	model := s.model()
	if p, ok := s.table.Lookup(model); ok && len(s.table) > 0 {
		if _, mine := s.table[Canonical(model)]; mine {
			return models.Model{Input: p.Input, Output: p.Output}, "the cost row's prices", true
		}
	}
	if m, ok := models.Lookup(s.llmPlugin(), model); ok && m.Input > 0 {
		return m, "the model catalogue (models.dev)", true
	}
	if p, ok := s.table.Lookup(model); ok {
		return models.Model{Input: p.Input, Output: p.Output}, "the built-in table for " + Canonical(model), true
	}
	return models.Model{}, "", false
}

// Usage implements llm.UsageReporter.
func (s *Service) Usage() llm.Usage {
	u := s.rep.Usage()
	if u.Priced {
		return u
	}
	if m, _, ok := s.lookup(); ok {
		// Tiered rates by input size (gpt-5.6-sol doubles above 272k),
		// and the prompt cache priced as a cache: a cached read is a
		// tenth of the input rate, so billing it as fresh input would
		// hide the saving caching exists for.
		u.Cost = m.CostCached(u.InputTokens, u.OutputTokens, u.CacheReadTokens, u.CacheCreationTokens)
		u.Priced = true
	}
	return u
}

// ContextLimit is the current model's context window in tokens (0 =
// unknown), for the status bar's context percentage.
func (s *Service) ContextLimit() int {
	if m, ok := models.Lookup(s.llmPlugin(), s.model()); ok && m.Context > 0 {
		return m.Context
	}
	return ContextLimit(s.model())
}

// Source says where the price came from, for /cost.
func (s *Service) Source() string {
	if s.rep.Usage().Priced {
		return "the provider's own per-response price"
	}
	if _, src, ok := s.lookup(); ok {
		return src
	}
	return "no price known for " + s.model() + " (add it under the cost row's prices)"
}

// ParsePrices reads `prices: {model: {input: $/Mtok, output: $/Mtok}}`.
func ParsePrices(v any) (Table, error) {
	t := Table{}
	if v == nil {
		return t, nil
	}
	raw, ok := v.(map[string]any)
	if !ok {
		return nil, fmt.Errorf("cost: prices is %T, want a map of model → {input, output}", v)
	}
	for model, pv := range raw {
		pm, ok := pv.(map[string]any)
		if !ok {
			return nil, fmt.Errorf("cost: prices.%s is %T, want {input, output}", model, pv)
		}
		in, err := num(pm["input"])
		if err != nil {
			return nil, fmt.Errorf("cost: prices.%s.input: %w", model, err)
		}
		out, err := num(pm["output"])
		if err != nil {
			return nil, fmt.Errorf("cost: prices.%s.output: %w", model, err)
		}
		t[Canonical(model)] = Price{in, out}
	}
	return t, nil
}

func num(v any) (float64, error) {
	switch n := v.(type) {
	case int:
		return float64(n), nil
	case int64:
		return float64(n), nil
	case float64:
		return n, nil
	}
	return 0, fmt.Errorf("want a number, got %v", v)
}

// Known lists the built-in model ids, for the help text.
func Known() []string {
	out := slices.Sorted(maps.Keys(builtin))
	return out
}

type plugin struct{}

func init() {
	kernel.Register("cost", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "cost" }
func (plugin) Inject() []string { return []string{"llm"} }

// Apply provides "usage" over the llm row. An llm that reports no
// usage at all (echo, a js provider) makes the row a no-op mount: the
// status bar then shows nothing, as before.
func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	for k := range cfg {
		if k != "prices" {
			return fmt.Errorf("cost: unknown config key %q", k)
		}
	}
	table, err := ParsePrices(cfg["prices"])
	if err != nil {
		return err
	}
	rep, err := kernel.Get[llm.UsageReporter](ctx, "llm")
	if err != nil {
		return nil
	}
	model := func() string { return "" }
	if m, err := kernel.Get[llm.Modeler](ctx, "llm"); err == nil {
		model = m.Model
	}
	// Read at call time, not now: /model swaps the row mid-session.
	plugin := func() string {
		for _, r := range ctx.Desired() {
			if r.ID == "llm" {
				return r.Plugin
			}
		}
		return ""
	}
	ctx.Provide("usage", &Service{rep: rep, model: model, plugin: plugin, table: table})
	return nil
}
