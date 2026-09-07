package loop

import (
	"testing"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

// A done entry carries what its turn spent — the tally's movement since
// the last done — and a resumed session adds those up.
func TestUsageDeltaAndSum(t *testing.T) {
	t.Parallel()
	prev := llm.Usage{InputTokens: 100, OutputTokens: 10, CacheReadTokens: 80, Cost: 0.5, Priced: true, LastInputTokens: 100}
	now := llm.Usage{InputTokens: 350, OutputTokens: 40, CacheReadTokens: 300, CacheCreationTokens: 20, Cost: 0.8, Priced: true, LastInputTokens: 250}
	d := UsageDelta(prev, now)
	if d["in"] != 250 || d["out"] != 30 || d["cache_read"] != 220 || d["cache_write"] != 20 || d["last_in"] != 250 {
		t.Fatalf("delta = %v", d)
	}
	if c, _ := d["cost"].(float64); c < 0.2999 || c > 0.3001 {
		t.Fatalf("cost delta = %v", d["cost"])
	}
	if UsageDelta(now, now) != nil {
		t.Fatal("a turn that moved nothing stamps nothing")
	}
	// Through JSON the counts come back as float64s.
	entries := []history.Entry{
		{Kind: "done", Data: map[string]any{"usage": map[string]any{"in": 100.0, "out": 10.0, "cache_read": 80.0, "last_in": 100.0, "cost": 0.5}}},
		{Kind: "done", Data: map[string]any{"files": []any{}}}, // an old entry, before usage was stamped
		{Kind: "done", Data: map[string]any{"usage": d}},
	}
	u := SumUsage(entries)
	if u.InputTokens != 350 || u.OutputTokens != 40 || u.CacheReadTokens != 300 || u.CacheCreationTokens != 20 || u.LastInputTokens != 250 || !u.Priced {
		t.Fatalf("sum = %+v", u)
	}
	if u.Cost < 0.7999 || u.Cost > 0.8001 {
		t.Fatalf("summed cost = %v", u.Cost)
	}
}
