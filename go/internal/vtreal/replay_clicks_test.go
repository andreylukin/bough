package vtreal

// The mouse, on a real terminal, over a replayed session: every
// collapsible kind the replay seam can produce toggles on a click,
// the collapse_all/expand_all chords move all of them at once, the
// status bar opens the session picker, and the clicks that must do
// nothing (a user block, the right button, a click behind an overlay)
// do nothing. Each step re-checks the frame invariants (check) so a
// toggle that loses the composer or the status bar fails here.
//
// Kinds not reachable through this seam: thinking (the replay model
// is not a Thinker), job (no real background shell — codemode is
// replayed), spawn (subagents never run from a tape). Those kinds are covered
// click-by-click in
// plugins/ui/clicks_test.go against the in-process model.

import (
	"fmt"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

func clicksTape(t *testing.T) string {
	t.Helper()
	path, err := filepath.Abs("testdata/replay/clicks.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	return path
}

// clicksApp boots bough on the clicks tape and runs the first turn,
// so a code block and a result block are on screen.
func clicksApp(t *testing.T) *app {
	t.Helper()
	a := startCfg(t, 100, 30, replayConfig(clicksTape(t)))
	a.check("boot")
	clicksTurn(t, a, 1, "count to two")
	return a
}

// clicksTurn submits one input and waits for the nth turn to land.
func clicksTurn(t *testing.T, a *app, n int, input string) {
	t.Helper()
	a.typeText(input)
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(n, 60*time.Second) {
		t.Fatalf("turn %d (%q) never finished:\n%s", n, input, a.text())
	}
	a.check(fmt.Sprintf("turn %d", n))
}

// clicksRow returns the screen row holding a header with this glyph
// and tag, -1 when it is not on screen.
func clicksRow(a *app, glyph, tag string) int {
	for i, l := range a.lines() {
		if strings.Contains(l, glyph) && strings.Contains(l, tag) {
			return i
		}
	}
	return -1
}

// clicksToggle clicks the closed header of a block tagged tag, checks
// it opened, then clicks it shut again and checks it closed. Kinds
// that keep a disclosure header while open (code, result, error) are
// closed again from the "▾" header; the kinds that render as bare
// text when open (system, context, todo) are closed from the body row
// naming `body`. Every row is looked up fresh: a toggle scrolls the
// transcript.
func clicksToggle(t *testing.T, a *app, tag, body string) {
	t.Helper()
	a.settled()
	row := clicksRow(a, "▸", tag)
	if row < 0 {
		t.Fatalf("no closed %q header on screen:\n%s", tag, a.text())
	}
	a.click(2, row)
	a.waitUntil(func(s string) bool { return !clicksHasClosed(s, tag) },
		fmt.Sprintf("%q expanded by a click (the ▸ header gone)", tag))
	a.check(tag + " expanded")
	a.settled()
	row = clicksRow(a, "▾", tag)
	if row < 0 && body != "" {
		row = clicksRow(a, body, body)
	}
	if row < 0 {
		t.Fatalf("no row of the open %q block on screen:\n%s", tag, a.text())
	}
	a.click(2, row)
	a.waitUntil(func(s string) bool { return clicksHasClosed(s, tag) },
		fmt.Sprintf("%q collapsed by a second click (▸)", tag))
	a.check(tag + " collapsed")
}

func clicksHasOpen(screen, tag string) bool   { return clicksLineWith(screen, "▾", tag) }
func clicksHasClosed(screen, tag string) bool { return clicksLineWith(screen, "▸", tag) }

func clicksLineWith(screen, glyph, tag string) bool {
	for _, l := range strings.Split(screen, "\n") {
		if strings.Contains(l, glyph) && strings.Contains(l, tag) {
			return true
		}
	}
	return false
}

// Every collapsible kind this seam produces opens and closes on a
// click: code and result from the tape, error from a recorded
// "error: …" result, system from a slash command's output, todo from
// the todo row's list. Each kind drives its own session, so a failure
// names one kind and its screen.
func TestClicksToggleEveryKind(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name  string
		tag   string
		body  string // a body row, for the kinds that open headerless
		setup func(t *testing.T, a *app)
	}{
		{name: "code", tag: "code js"},
		{name: "result", tag: "result ("},
		{name: "context", tag: "context (", body: "context:"},
		{name: "error", tag: "boom failed", setup: func(t *testing.T, a *app) {
			clicksTurn(t, a, 2, "now break it")
			a.waitFor("boom failed")
		}},
		// Each /todo mutation leaves its own receipt: "todo (2 lines)"
		// names the second one, not the first "todo (1 line)" above it.
		{name: "todo", tag: "todo (2 lines)", body: "[ ] 2 run the test", setup: func(t *testing.T, a *app) {
			for _, item := range []string{"/todo add write the test", "/todo add run the test"} {
				a.typeText(item)
				a.key(uv.KeyEnter, 0)
				a.settled()
			}
			a.waitFor("todo (2 lines)")
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			a := clicksApp(t)
			if tc.setup != nil {
				tc.setup(t, a)
			}
			clicksToggle(t, a, tc.tag, tc.body)
		})
	}

	// A slash command's output lands expanded (it is the answer you
	// asked for), so its cycle starts the other way round: a click on
	// a body row folds it to a "▸ system (N lines)" header, and a
	// click on that header brings the text back.
	t.Run("system", func(t *testing.T) {
		t.Parallel()
		a := clicksApp(t)
		a.typeText("/help")
		a.key(uv.KeyEnter, 0)
		a.waitFor("/cost")
		a.settled()
		row := clicksRow(a, "/cost", "tokens and cost")
		if row < 0 {
			t.Fatalf("no /help body row on screen:\n%s", a.text())
		}
		a.click(2, row)
		a.waitUntil(func(s string) bool { return clicksHasClosed(s, "system (") }, "the system block folded by a click (▸)")
		a.check("system collapsed")
		a.settled()
		row = clicksRow(a, "▸", "system (")
		if row < 0 {
			t.Fatalf("no folded system header on screen:\n%s", a.text())
		}
		a.click(2, row)
		a.waitFor("tokens and cost")
		a.check("system expanded")
	})
}

// ctrl+x e opens every collapsible block and ctrl+x c closes them all
// again — the chords, not the clicks, but the same state the markers
// report.
func TestClicksCollapseAllExpandAll(t *testing.T) {
	t.Parallel()
	a := clicksApp(t)
	a.settled()

	a.key('x', uv.ModCtrl)
	a.key('e', 0)
	a.waitUntil(func(s string) bool { return strings.Contains(s, "expanded") }, "the expand_all flash")
	s := a.settled()
	if clicksHasClosed(s, "code js") || clicksHasClosed(s, "result (") {
		t.Fatalf("expand_all left a closed header (▸):\n%s", s)
	}
	if !clicksHasOpen(s, "code js") || !clicksHasOpen(s, "result (") {
		t.Fatalf("expand_all did not open code and result (▾):\n%s", s)
	}
	a.check("expand_all")

	a.key('x', uv.ModCtrl)
	a.key('c', 0)
	a.waitUntil(func(s string) bool { return strings.Contains(s, "collapsed") }, "the collapse_all flash")
	s = a.settled()
	if clicksHasOpen(s, "code js") || clicksHasOpen(s, "result (") {
		t.Fatalf("collapse_all left an open header (▾):\n%s", s)
	}
	if !clicksHasClosed(s, "code js") || !clicksHasClosed(s, "result (") {
		t.Fatalf("collapse_all did not close code and result (▸):\n%s", s)
	}
	a.check("collapse_all")
}

// A click on the user's own prompt row toggles nothing and leaves the
// draft alone: only collapsible blocks answer the mouse.
func TestClicksUserBlockIsInert(t *testing.T) {
	t.Parallel()
	a := clicksApp(t)
	a.typeText("a draft")
	a.waitFor("> a draft")
	before := a.settled()
	row := clicksRow(a, "❯", "count to two")
	if row < 0 {
		t.Fatalf("no user block on screen:\n%s", before)
	}
	a.click(2, row)
	if after := a.settled(); after != before {
		t.Fatalf("a click on the user block changed the screen:\nbefore:\n%s\nafter:\n%s", before, after)
	}
	if !strings.Contains(a.text(), "> a draft") {
		t.Fatalf("the draft did not survive the click:\n%s", a.text())
	}
	a.check("clicked the user block")
}

// The status bar names the session, so a click on it opens the
// session picker; esc closes it and the transcript comes back.
func TestClicksStatusBarOpensPicker(t *testing.T) {
	t.Parallel()
	a := clicksApp(t)
	a.settled()
	row := clicksRow(a, "? keys", "? keys")
	if row < 0 {
		t.Fatalf("no status bar on screen:\n%s", a.text())
	}
	a.click(4, row)
	a.waitFor("resume a session")
	a.key(uv.KeyEscape, 0)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "resume a session") }, "the picker to close on esc")
	a.waitFor("count to two")
	a.check("picker closed")
}

// While the picker is up the mouse is inert: a click on a transcript
// row behind it neither closes the picker nor toggles the block under
// the pointer. (bough has no click-outside-to-close; esc is the way
// out. Asserted here so the day it gains one, this test says so.)
func TestClicksBehindPickerAreInert(t *testing.T) {
	t.Parallel()
	a := clicksApp(t)
	a.settled()
	bar := clicksRow(a, "? keys", "? keys")
	a.click(4, bar)
	a.waitFor("resume a session")
	before := a.settled()
	a.click(2, 3)
	a.click(60, 8)
	if after := a.settled(); after != before {
		t.Fatalf("a click behind the picker changed the screen:\nbefore:\n%s\nafter:\n%s", before, after)
	}
	a.key(uv.KeyEscape, 0)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "resume a session") }, "the picker to close on esc")
	s := a.settled()
	if !clicksHasClosed(s, "code js") {
		t.Fatalf("a block toggled behind the picker:\n%s", s)
	}
	a.check("after the picker")
}

// The slash palette is an overlay too: a click on a row above it goes
// to the transcript (the palette has no click-outside-to-close), and
// the typed "/" draft is untouched either way.
func TestClicksBehindPaletteKeepDraft(t *testing.T) {
	t.Parallel()
	a := clicksApp(t)
	a.typeText("/")
	a.waitFor("/help")
	a.settled()
	a.click(2, 0)
	a.settled()
	if s := a.text(); !strings.Contains(s, "> /") {
		t.Fatalf("the palette draft did not survive a click behind it:\n%s", s)
	}
	a.check("clicked behind the palette")
	a.key(uv.KeyEscape, 0)
	a.settled()
}

// Two clicks in a row on the same header are two toggles: a
// double-click opens and closes, it does not open twice or select.
func TestClicksDoubleClickTogglesTwice(t *testing.T) {
	t.Parallel()
	a := clicksApp(t)
	before := a.settled()
	row := clicksRow(a, "▸", "code js")
	if row < 0 {
		t.Fatalf("no closed code header on screen:\n%s", before)
	}
	a.click(2, row)
	a.click(2, row)
	a.waitUntil(func(s string) bool { return clicksHasClosed(s, "code js") }, "the code block closed again after a double click")
	if s := a.settled(); s != before {
		t.Fatalf("a double click did not return the screen to where it was:\nbefore:\n%s\nafter:\n%s", before, s)
	}
	a.check("double click")
}

// The right button does nothing, over a header or anywhere else.
func TestClicksRightButtonIsNoop(t *testing.T) {
	t.Parallel()
	a := clicksApp(t)
	a.typeText("kept draft")
	a.waitFor("> kept draft")
	before := a.settled()
	rows := []int{0, 3, clicksRow(a, "▸", "code js"), clicksRow(a, "? keys", "? keys")}
	for _, y := range rows {
		if y < 0 {
			t.Fatalf("row to right-click not found on screen:\n%s", before)
		}
		a.term.SendMouse(uv.MouseClickEvent{X: 5, Y: y, Button: uv.MouseRight})
		a.term.SendMouse(uv.MouseReleaseEvent{X: 5, Y: y, Button: uv.MouseRight})
	}
	if after := a.settled(); after != before {
		t.Fatalf("a right click changed the screen:\nbefore:\n%s\nafter:\n%s", before, after)
	}
	a.check("right clicks")
}
