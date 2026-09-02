package ui

// Semantic block rendering: role markers, boxes, collapse, markdown,
// unknown kinds.

import (
	"strings"
	"testing"
)

func TestRenderUserMarker(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("user", "hello there")
	if p := d.plain(); !strings.Contains(p, "❯ hello there") {
		t.Errorf("user block missing ❯ marker:\n%s", p)
	}
}

func TestRenderAssistantMarker(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "plain reply")
	p := d.plain()
	if !strings.Contains(p, "●") || !strings.Contains(p, "bough") {
		t.Errorf("assistant block missing ● bough header:\n%s", p)
	}
	if !strings.Contains(p, "plain reply") {
		t.Errorf("assistant text missing:\n%s", p)
	}
}

func TestRenderCodeHeaderAndBox(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("code", `tools.bash("echo hi")`)
	p := d.plain()
	if !strings.Contains(p, `▾ code js (1 line): tools.bash("echo hi")`) {
		t.Errorf("code block missing expanded disclosure header:\n%s", p)
	}
	if !strings.Contains(p, `tools.bash("echo hi")`) {
		t.Errorf("code text missing:\n%s", p)
	}
	if !strings.Contains(p, "╰") || !strings.Contains(p, "╮") {
		t.Errorf("code block missing rounded border:\n%s", p)
	}
}

func TestRenderResultHeaderAndBox(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", "hi from codemode")
	p := d.plain()
	if !strings.Contains(p, "▾ result (1 line): hi from codemode") {
		t.Errorf("result block missing expanded disclosure header:\n%s", p)
	}
	if !strings.Contains(p, "│ hi from codemode") {
		t.Errorf("result text missing from box:\n%s", p)
	}
}

func TestRenderErrorMarker(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("error", "provider exploded")
	if p := d.plain(); !strings.Contains(p, "✗ provider exploded") {
		t.Errorf("error block missing ✗ marker:\n%s", p)
	}
}

func TestRenderDoneDivider(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("done", "")
	if p := d.plain(); !strings.Contains(p, strings.Repeat("─", 40)) {
		t.Errorf("done divider missing (want 40 ─ at width 80):\n%s", p)
	}
}

func TestRenderDoneDividerNarrow(t *testing.T) {
	t.Parallel()
	d := newDrv(t, 20, 24, cfgWith(t, nil, nil, nil))
	d.event("done", "")
	p := d.plain()
	if !strings.Contains(p, strings.Repeat("─", 18)) {
		t.Errorf("narrow divider missing (want 18 ─ at width 20):\n%s", p)
	}
	if strings.Contains(p, strings.Repeat("─", 19)) {
		t.Errorf("narrow divider too wide:\n%s", p)
	}
}

func TestRenderUnknownKindIgnoredGracefully(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("telemetry", "cpu 3%") // future kind: rendered dim, no crash
	p := d.plain()
	if !strings.Contains(p, "telemetry") || !strings.Contains(p, "cpu 3%") {
		t.Errorf("unknown kind should render kind+text:\n%s", p)
	}
}

func TestTranscriptOrder(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("user", "q1")
	d.event("assistant", "a1")
	d.event("done", "")
	p := d.plain()
	iq, ia := strings.Index(p, "q1"), strings.Index(p, "a1")
	id := strings.Index(p, "────")
	if iq < 0 || ia < 0 || id < 0 || !(iq < ia && ia < id) {
		t.Errorf("blocks out of order (q1@%d a1@%d done@%d):\n%s", iq, ia, id, p)
	}
}

// --- collapse ---

func TestLongResultStartsCollapsed(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", nLines(20))
	if !d.m.blocks[0].collapsed {
		t.Fatal("20-line result should start collapsed")
	}
	p := d.plain()
	if !strings.Contains(p, "▸ result (20 lines): l0xxx") {
		t.Errorf("collapsed result missing header with line count and preview:\n%s", p)
	}
	if strings.Contains(p, "l9xxx") {
		t.Errorf("collapsed result body should be hidden:\n%s", p)
	}
}

func TestBoundaryResultDoesNotCollapse(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", nLines(collapseAt)) // exactly at the threshold
	if d.m.blocks[0].collapsed {
		t.Errorf("%d-line result should not collapse (threshold is >%d)", collapseAt, collapseAt)
	}
	d.event("result", nLines(collapseAt+1))
	if !d.m.blocks[1].collapsed {
		t.Errorf("%d-line result should collapse", collapseAt+1)
	}
}

func TestFocusEnterExpands(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", nLines(20))
	d.press(keyTab()) // block_next: focus the result
	d.press(keyEnter())
	if d.m.blocks[0].collapsed {
		t.Fatal("enter on the focused result should expand it")
	}
	d.press(keyPgUp()) // the 20-line body pushed the header off the top
	if p := d.plain(); !strings.Contains(p, "▾ result (20 lines)") {
		t.Errorf("expanded result missing ▾ header:\n%s", p)
	}
}

func TestFocusEnterTogglesBack(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", nLines(20))
	d.press(keyTab())
	d.press(keyEnter())
	d.press(keyEnter())
	if !d.m.blocks[0].collapsed {
		t.Error("second enter should re-collapse")
	}
	if p := d.plain(); !strings.Contains(p, "▸ result (20 lines)") {
		t.Errorf("re-collapsed result missing ▸ header:\n%s", p)
	}
}

func TestRemappedToggleHitsNewestBlock(t *testing.T) {
	t.Parallel()
	// collapse_toggle remapped off enter: with nothing focused it
	// toggles the newest collapsible block.
	d := newDrv(t, 80, 24, cfgWith(t, nil, map[string]string{"collapse_toggle": "ctrl+t"}, nil))
	d.event("result", nLines(20))
	d.event("result", nLines(30))
	d.press(keyCtrl('t'))
	if !d.m.blocks[0].collapsed {
		t.Error("first result should stay collapsed")
	}
	if d.m.blocks[1].collapsed {
		t.Error("newest result should have been expanded")
	}
}

func TestToggleNoResultsIsNoop(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "no results here")
	d.press(keyTab())   // no collapsible block to focus
	d.press(keyEnter()) // must not panic or alter blocks
	if len(d.m.blocks) != 1 || d.m.blocks[0].kind != "assistant" {
		t.Error("toggle with no results changed the transcript")
	}
}

// --- markdown ---

func TestMarkdownHeadingRenders(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "# Big Title\n\nbody text")
	p := d.plain()
	if !strings.Contains(p, "Big Title") || !strings.Contains(p, "body text") {
		t.Errorf("markdown heading/body not visible:\n%s", p)
	}
}

func TestMarkdownBoldRenders(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "some **very bold** words")
	if p := d.plain(); !strings.Contains(p, "very bold") {
		t.Errorf("bold text not visible (raw or styled):\n%s", p)
	}
}

func TestMarkdownCacheClearedOnResize(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "cached line")
	if len(d.m.mdCache) == 0 {
		t.Fatal("assistant render should populate the markdown cache")
	}
	d.feed(windowSize(120, 40))
	if len(d.m.mdCache) != 1 { // refresh re-rendered the one block at the new width
		t.Errorf("resize should rebuild the cache, got %d entries", len(d.m.mdCache))
	}
}
