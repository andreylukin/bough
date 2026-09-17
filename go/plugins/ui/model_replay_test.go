package ui

import (
	"testing"

	"github.com/andreylukin/bough/plugins/history"
)

// /model records its swap as a "model" entry for resume; a resumed TUI
// must not draw it as an empty block.
func TestReplaySkipsModelEntry(t *testing.T) {
	m := testModel(t)
	cfg := m.cfg.Load()
	h := histWith("/tmp/s.jsonl", "hi")
	h.entries = append(h.entries, history.Entry{Seq: 2, Kind: "model",
		Data: map[string]any{"sets": []any{"llm.model=m2"}}})
	cfg.hist = h
	m.cfg.Store(cfg)
	m.replay()
	for _, b := range m.blocks {
		if b.kind == "model" {
			t.Fatalf("model entry rendered as a block: %+v", b)
		}
	}
}
