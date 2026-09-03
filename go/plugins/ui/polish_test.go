package ui

// Clickable/collapsible interaction tests (mouse hit-testing from the
// rendered frame, focus cursor, identity across appends, resize) and
// the transcript-polish contracts: executed-code dedupe, hard
// newlines, block spacing.

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

// frameRow returns the first screen row whose (ANSI-stripped) text
// contains substr, or -1.
func frameRow(d *drv, substr string) int {
	for i, l := range strings.Split(d.plain(), "\n") {
		if strings.Contains(l, substr) {
			return i
		}
	}
	return -1
}

func (d *drv) click(y int) {
	d.feed(tea.MouseClickMsg{X: 0, Y: y, Button: tea.MouseLeft})
	d.feed(tea.MouseReleaseMsg{X: 0, Y: y, Button: tea.MouseLeft})
}

func TestClickToggleCoordsFromRenderedFrame(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "hello")
	d.event("result", nLines(10))

	row := frameRow(d, "▸ result (10 lines)")
	if row < 0 {
		t.Fatalf("collapsed header not on screen:\n%s", d.plain())
	}
	d.click(row)
	if d.m.blocks[1].collapsed {
		t.Fatal("click on the header should expand the result")
	}
	row = frameRow(d, "▾ result (10 lines)")
	if row < 0 {
		t.Fatalf("expanded header not on screen:\n%s", d.plain())
	}
	d.click(row + 3) // inside the boxed body
	if !d.m.blocks[1].collapsed {
		t.Fatal("click inside the expanded body should collapse it")
	}

	// A click below the transcript (status bar / composer) is ignored.
	d.click(d.m.vp.Height() + 1)
	if !d.m.blocks[1].collapsed {
		t.Error("click below the viewport must not toggle anything")
	}
}

func TestFocusCursorStyledByFocusToken(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", nLines(10))
	before := d.view()
	d.press(keyTab()) // focus the block
	after := d.view()
	if d.m.focusID != d.m.blocks[0].id {
		t.Fatal("tab should focus the only collapsible block")
	}
	if stripANSI(before) != stripANSI(after) {
		t.Fatal("focusing must not change the plain text")
	}
	if before == after {
		t.Error("focused header should restyle with the focus token")
	}
}

func TestCollapseIdentityStableAcrossAppends(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", nLines(10))
	d.event("result", nLines(10))
	d.press(keyTab()) // focus the newest: the second result
	d.press(keyEnter())
	if d.m.blocks[1].collapsed {
		t.Fatal("second result should be expanded")
	}
	id := d.m.focusID

	d.event("code", nLines(10))
	d.event("result", nLines(10))
	if d.m.blocks[1].collapsed {
		t.Error("append must not re-collapse the expanded block")
	}
	if d.m.focusID != id {
		t.Error("append must not move the block cursor")
	}
	d.press(keyTab()) // cursor steps one older from where it was, not to the new blocks
	if d.m.focusID != d.m.blocks[0].id {
		t.Errorf("tab after appends should focus block 1, focusID=%d", d.m.focusID)
	}
}

func TestResizeKeepsClickHitRanges(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "sized")
	d.event("result", nLines(10))
	d.feed(windowSize(120, 40))

	row := frameRow(d, "▸ result (10 lines)")
	if row < 0 {
		t.Fatalf("collapsed header not on screen after resize:\n%s", d.plain())
	}
	d.click(row)
	if d.m.blocks[1].collapsed {
		t.Error("click after resize should hit the recomputed range")
	}
}

// --- transcript polish ---

func TestDedupeExecutedCodeNoOrphanHeader(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "```js\nconsole.log('DOUBLE')\n```")
	d.event("code", "console.log('DOUBLE')")
	if len(d.m.blocks) != 1 || d.m.blocks[0].kind != "code" {
		t.Fatalf("fence-only assistant block should be dropped, blocks=%+v", d.m.blocks)
	}
	if p := d.plain(); strings.Contains(p, "●") {
		t.Errorf("orphan assistant header survived dedupe:\n%s", p)
	}
}

func TestDedupeKeepsSurroundingAssistantText(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "before words\n```js\nX = 1\n```\nafter words")
	d.event("code", "X = 1\n")
	if len(d.m.blocks) != 2 {
		t.Fatalf("want assistant+code, got %+v", d.m.blocks)
	}
	if txt := d.m.blocks[0].text; strings.Contains(txt, "```") ||
		!strings.Contains(txt, "before words") || strings.Contains(txt, "after words") {
		t.Errorf("fence not stripped cleanly (prose after the fence waits for the result): %q", txt)
	}
	d.event("result", "1")
	if len(d.m.blocks) != 3 {
		t.Fatalf("prose after the fence waits for the turn to end, got %+v", d.m.blocks)
	}
	d.event("done", "")
	if len(d.m.blocks) < 5 || d.m.blocks[3].kind != "assistant" || d.m.blocks[3].text != "after words" {
		t.Fatalf("prose after the fence should land before the done row, got %+v", d.m.blocks)
	}
	// The executed code renders exactly once: header preview plus box
	// (expand the collapsed block so the box shows).
	d.press(keyTab()) // newest: the result
	d.press(keyTab()) // older: the code block
	d.press(keyEnter())
	p := d.plain()
	if got := strings.Count(p, "X = 1"); got != 2 {
		t.Errorf("code should render once (header+box = 2 occurrences), got %d:\n%s", got, p)
	}
}

func TestDedupeStopsAtTurnBoundary(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "```js\nold()\n```")
	d.event("done", "")
	d.event("code", "old()") // next turn: must not eat last turn's fence
	// assistant, "turn ended without a reply" (a fence-only reply said
	// nothing), done, code.
	if len(d.m.blocks) != 4 || !strings.Contains(d.m.blocks[0].text, "old()") {
		t.Fatalf("dedupe crossed a done boundary, blocks=%+v", d.m.blocks)
	}
}

func TestAssistantHardNewlineSurvives(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "[tool output]\nhi")
	for _, l := range strings.Split(d.plain(), "\n") {
		if strings.Contains(l, "[tool output]") && strings.Contains(l, "hi") {
			t.Fatalf("single newline was eaten, joined line: %q", l)
		}
	}
	p := d.plain()
	if !strings.Contains(p, "[tool output]") || !strings.Contains(p, "hi") {
		t.Fatalf("assistant text missing:\n%s", p)
	}
}

func TestNoDoubleBlankLinesBetweenBlocks(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("user", "q1")
	d.event("assistant", "para one\n\npara two")
	d.event("code", "console.log('x')")
	d.event("result", "out")
	d.event("done", "")
	d.event("user", "q2")
	d.event("assistant", "reply")

	lines := strings.Split(d.plain(), "\n")
	body := lines[:len(lines)-2] // drop status bar + composer
	last := len(body) - 1
	for last >= 0 && strings.TrimSpace(body[last]) == "" {
		last-- // viewport bottom padding is not block spacing
	}
	blanks := 0
	for i := 0; i <= last; i++ {
		if strings.TrimSpace(body[i]) == "" {
			if blanks++; blanks > 1 {
				t.Fatalf("double blank line at row %d:\n%s", i, d.plain())
			}
		} else {
			blanks = 0
		}
	}
}

// Prose the model wrote under a fence before seeing its result is a
// guess ("Done, here's the file:"); when the model replies again after
// the result, that guess is dropped rather than shown as a second,
// contradictory answer.
func TestTrailingProseSupersededByNextReply(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "```js\nconsole.log(1)\n```\nDone, it printed 2.")
	d.event("code", "console.log(1)\n")
	d.event("result", "1")
	d.event("assistant", "Done, it printed 1.")
	d.event("done", "")
	p := d.plain()
	if strings.Contains(p, "printed 2") || !strings.Contains(p, "printed 1") {
		t.Fatalf("superseded trailing prose should be dropped:\n%s", p)
	}
}
