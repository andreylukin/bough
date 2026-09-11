package ui

import (
	"strings"
	"testing"
)

// A URL in command output (/artifacts) is a terminal link.
func TestSystemBlockLinksURLs(t *testing.T) {
	m := testModel(t)
	m.blocks = append(m.blocks, block{kind: "system", text: "opened http://localhost:7683/artifacts/s/p."})
	out := m.render(&m.blocks[len(m.blocks)-1], m.cfg.Load())
	if !strings.Contains(out, "\x1b]8;;http://localhost:7683/artifacts/s/p\x07http://localhost:7683/artifacts/s/p\x1b]8;;\x07") {
		t.Fatalf("no OSC 8 link: %q", out)
	}
}
