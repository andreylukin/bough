package models

import (
	"encoding/json"
	"math"
	"net/http"
	"net/http/httptest"
	"testing"
)

// The embedded snapshot is the offline answer: it must carry the four
// providers bough ships plugins for, with prices and context windows.
func TestSnapshotCoversTheShippedProviders(t *testing.T) {
	c := fromSnapshot()
	for _, plugin := range []string{"llm-anthropic", "llm-openai", "llm-openrouter", "llm-cerebras"} {
		if n := len(c[Provider(plugin)]); n == 0 {
			t.Fatalf("%s has no models in the snapshot", plugin)
		}
	}
	// The models this session actually runs on.
	for _, tc := range []struct{ plugin, model string }{
		{"llm-openrouter", "z-ai/glm-5.3-flash"},
		{"llm-anthropic", "claude-opus-5"},
		{"llm-openai", "gpt-5.6-sol"},
	} {
		m, ok := Lookup(tc.plugin, tc.model)
		if !ok || m.Input <= 0 || m.Context <= 0 {
			t.Fatalf("%s/%s = %+v (ok=%v)", tc.plugin, tc.model, m, ok)
		}
	}
}

// A dated snapshot id prices like the model it is a snapshot of.
func TestLookupFallsBackToThePrefix(t *testing.T) {
	m, ok := Lookup("llm-anthropic", "claude-haiku-4-5-20251001")
	if !ok || m.Input <= 0 {
		t.Fatalf("dated id did not resolve: %+v", m)
	}
	if _, ok := Lookup("llm-anthropic", "not-a-model"); ok {
		t.Fatal("an unknown id must not resolve")
	}
	if _, ok := Lookup("llm-echo", "anything"); ok {
		t.Fatal("a provider with no catalogue must not resolve")
	}
}

// Tiers price by input size: the highest tier a request passes wins.
func TestCostAppliesTiers(t *testing.T) {
	m := Model{Input: 4, Output: 20, Tiers: []Tier{{Over: 272_000, Input: 8, Output: 30}}}
	if got, want := m.Cost(100_000, 10_000), 100_000*4.0/1e6+10_000*20.0/1e6; math.Abs(got-want) > 1e-9 {
		t.Fatalf("under the tier: %f, want %f", got, want)
	}
	if got, want := m.Cost(300_000, 10_000), 300_000*8.0/1e6+10_000*30.0/1e6; math.Abs(got-want) > 1e-9 {
		t.Fatalf("over the tier: %f, want %f", got, want)
	}
	flat := Model{Input: 3, Output: 15}
	if got, want := flat.Cost(1_000_000, 0), 3.0; math.Abs(got-want) > 1e-9 {
		t.Fatalf("no tiers: %f, want %f", got, want)
	}
}

// List is newest first, and caps.
func TestListNewestFirst(t *testing.T) {
	ids := List("llm-anthropic", 3)
	if len(ids) != 3 {
		t.Fatalf("cap ignored: %v", ids)
	}
	c := fromSnapshot()["anthropic"]
	for i := 1; i < len(ids); i++ {
		if c[ids[i-1]].Release < c[ids[i]].Release {
			t.Fatalf("not newest first: %v", ids)
		}
	}
	if got := List("llm-echo", 5); len(got) != 0 {
		t.Fatalf("a provider with no catalogue lists nothing, got %v", got)
	}
}

// Trim keeps the shipped providers and drops the 200-odd resellers,
// with the fields bough reads.
func TestTrimShapesTheUpstream(t *testing.T) {
	up := `{
	  "anthropic": {"models": {"claude-x": {
	      "cost": {"input": 5, "output": 25, "tiers": [{"input": 8, "output": 30, "tier": {"size": 200000}}]},
	      "limit": {"context": 1000000}, "release_date": "2026-02-17",
	      "reasoning_options": [{"type": "effort", "values": ["low", "high"]}]}}},
	  "some-reseller": {"models": {"claude-x": {"cost": {"input": 99, "output": 99}}}}
	}`
	c, err := Trim([]byte(up))
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := c["some-reseller"]; ok {
		t.Fatalf("a reseller survived the trim: %v", c)
	}
	m := c["anthropic"]["claude-x"]
	if m.Input != 5 || m.Output != 25 || m.Context != 1_000_000 || m.Release != "2026-02-17" {
		t.Fatalf("trimmed model = %+v", m)
	}
	if len(m.Tiers) != 1 || m.Tiers[0].Over != 200_000 || m.Tiers[0].Input != 8 {
		t.Fatalf("tiers = %+v", m.Tiers)
	}
	if len(m.Efforts) != 2 || m.Efforts[0] != "low" {
		t.Fatalf("efforts = %v", m.Efforts)
	}
}

// A refresh reads the live catalogue; a failing one leaves what we had.
func TestFetchTrimsAndErrors(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		json.NewEncoder(w).Encode(map[string]any{
			"openai": map[string]any{"models": map[string]any{
				"gpt-x": map[string]any{"cost": map[string]any{"input": 1.0, "output": 2.0},
					"limit": map[string]any{"context": 123}}}},
		})
	}))
	defer srv.Close()
	c, err := fetch(t.Context(), srv.URL)
	if err != nil {
		t.Fatal(err)
	}
	if m := c["openai"]["gpt-x"]; m.Input != 1 || m.Context != 123 {
		t.Fatalf("fetched = %+v", m)
	}

	bad := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusInternalServerError)
	}))
	defer bad.Close()
	if _, err := fetch(t.Context(), bad.URL); err == nil {
		t.Fatal("a 500 must be an error, not an empty catalogue")
	}
}

// merge keeps a provider the live copy is missing rather than losing it.
func TestMergeKeepsMissingProviders(t *testing.T) {
	old := Catalogue{"anthropic": {"a": {Input: 1}}, "cerebras": {"c": {Input: 2}}}
	live := Catalogue{"anthropic": {"a2": {Input: 3}}}
	got := merge(old, live)
	if _, ok := got["cerebras"]; !ok {
		t.Fatalf("cerebras was dropped: %v", got)
	}
	if _, ok := got["anthropic"]["a2"]; !ok {
		t.Fatalf("anthropic was not replaced: %v", got)
	}
}
