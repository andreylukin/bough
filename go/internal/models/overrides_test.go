package models

import (
	"math"
	"testing"
)

func near(a, b float64) bool { return math.Abs(a-b) < 1e-9 }

// A one-hour write is billed at 2x input, the 5-minute one at the
// model's write rate; both are counted inside the write total.
func TestCostCached1h(t *testing.T) {
	t.Parallel()
	m := Model{Input: 4, Output: 20, CacheRead: 0.2, CacheWrite: 5}
	// 1M in: 600k read, 300k written (200k of it 1h), 100k fresh; 10k out.
	got := m.CostCached1h(1_000_000, 10_000, 600_000, 300_000, 200_000)
	want := (100_000*4.0 + 600_000*0.2 + 100_000*5.0 + 200_000*8.0 + 10_000*20.0) / 1e6
	if !near(got, want) {
		t.Errorf("CostCached1h = %v, want %v", got, want)
	}
	if !near(m.CostCached(1_000_000, 10_000, 600_000, 300_000), m.CostCached1h(1_000_000, 10_000, 600_000, 300_000, 0)) {
		t.Error("CostCached is CostCached1h with no 1h writes")
	}
	m.CacheWrite1h = 9
	if got := m.CostCached1h(100, 0, 0, 100, 100); !near(got, 100*9.0/1e6) {
		t.Errorf("an explicit 1h rate wins: %v", got)
	}
	// A 1h count larger than the write total cannot bill more than it.
	if got := m.CostCached1h(100, 0, 0, 50, 80); !near(got, (50*4.0+50*9.0)/1e6) {
		t.Errorf("1h clamped to the write total: %v", got)
	}
}

func TestOverridesFillAndKeepCatalogueFields(t *testing.T) {
	t.Parallel()
	base := Catalogue{
		"anthropic": {
			"claude-opus-5-5": {Release: "2026-09-20", Input: 99},
			"claude-sonnet-5": {Input: 2, Output: 10, CacheWrite: 2.5},
		},
		"openai": {"gpt-5.6-sol": {Input: 1.25}},
	}
	got := withOverrides(base)
	o := got["anthropic"]["claude-opus-5-5"]
	if o.Input != 4 || o.Output != 20 || o.CacheRead != 0.20 || o.CacheWrite != 5 || o.Context != 1_000_000 || len(o.Efforts) != 5 {
		t.Errorf("override not applied: %+v", o)
	}
	if o.Release != "2026-09-20" {
		t.Errorf("a field the override leaves zero keeps the catalogue's: %q", o.Release)
	}
	if o.CacheWrite1h != 8 || got["anthropic"]["claude-sonnet-5"].CacheWrite1h != 4 {
		t.Errorf("1h write rate should default to 2x input on anthropic models: %v %v", o.CacheWrite1h, got["anthropic"]["claude-sonnet-5"].CacheWrite1h)
	}
	if got["openai"]["gpt-5.6-sol"].CacheWrite1h != 0 {
		t.Error("only anthropic models get the 1h default")
	}
	if base["anthropic"]["claude-opus-5-5"].Input != 99 {
		t.Error("the input catalogue was modified")
	}
	if _, ok := withOverrides(Catalogue{})["anthropic"]["claude-opus-5-5"]; !ok {
		t.Error("an override adds a model the catalogue lacks")
	}
}
