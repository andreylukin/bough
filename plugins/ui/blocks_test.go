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

func TestRenderCodeBoxTag(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("code", `tools.bash("echo hi")`)
	p := d.plain()
	if !strings.Contains(p, "╭─ js ") {
		t.Errorf("code block missing js tag in top border:\n%s", p)
	}
	if !strings.Contains(p, `tools.bash("echo hi")`) {
		t.Errorf("code text missing:\n%s", p)
	}
	if !strings.Contains(p, "╰") || !strings.Contains(p, "╮") {
		t.Errorf("code block missing rounded border:\n%s", p)
	}
}

func TestRenderResultBoxTag(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", "hi from codemode")
	p := d.plain()
	if !strings.Contains(p, "╭─ result ") {
		t.Errorf("result block missing result tag:\n%s", p)
	}
	if !strings.Contains(p, "hi from codemode") {
		t.Errorf("result text missing:\n%s", p)
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
	if !strings.Contains(p, "… 12 more lines") {
		t.Errorf("collapsed result missing '… 12 more lines' hint:\n%s", p)
	}
}

func TestCollapseHintNamesToggleKey(t *testing.T) {
	t.Parallel()
	d := newDrv(t, 80, 24, cfgWith(t, nil, map[string]string{"collapse_toggle": "ctrl+t"}, nil))
	d.event("result", nLines(20))
	if p := d.plain(); !strings.Contains(p, "(ctrl+t)") {
		t.Errorf("collapse hint should name the bound key:\n%s", p)
	}
}

func TestBoundaryResultDoesNotCollapse(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", nLines(collapseAt)) // exactly 12 lines
	if d.m.blocks[0].collapsed {
		t.Error("12-line result should not collapse (threshold is >12)")
	}
	d.event("result", nLines(collapseAt+1))
	if !d.m.blocks[1].collapsed {
		t.Error("13-line result should collapse")
	}
}

func TestCollapseToggleExpands(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", nLines(20))
	d.press(keyTab())
	if d.m.blocks[0].collapsed {
		t.Fatal("tab should expand the collapsed result")
	}
	if p := d.plain(); strings.Contains(p, "more lines") {
		t.Errorf("expanded result still shows collapse hint:\n%s", p)
	}
}

func TestCollapseToggleTogglesBack(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", nLines(20))
	d.press(keyTab())
	d.press(keyTab())
	if !d.m.blocks[0].collapsed {
		t.Error("second tab should re-collapse")
	}
	if p := d.plain(); !strings.Contains(p, "… 12 more lines") {
		t.Errorf("re-collapsed result missing hint:\n%s", p)
	}
}

func TestCollapseToggleHitsLastResult(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", nLines(20))
	d.event("result", nLines(30))
	d.press(keyTab())
	if d.m.blocks[0].collapsed == false {
		t.Error("first result should stay collapsed")
	}
	if d.m.blocks[1].collapsed {
		t.Error("last result should have been expanded")
	}
}

func TestCollapseToggleNoResultsIsNoop(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "no results here")
	d.press(keyTab()) // must not panic or alter blocks
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
