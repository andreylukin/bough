package ui

// Deep copy coverage: what a drag selection and the copy action put on
// the clipboard across folded headers, open boxes, wide runes, the
// live block, wheel scrolls, pane edges, and very large payloads.
// Product bugs found here are gated behind BOUGH_KNOWN_COPY=1.

import (
	"fmt"
	"os"
	"reflect"
	"strings"
	"testing"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"
	"pgregory.net/rapid"
)

func copyKnown(t *testing.T, bug string) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_COPY") != "1" {
		t.Skip("known bug (BOUGH_KNOWN_COPY=1 to run): " + bug)
	}
}

// copyDrag presses at (x0,y0), drags to (x1,y1), releases, and returns
// the selected text and the release command's messages.
func copyDrag(d *drv, x0, y0, x1, y1 int) (string, []tea.Msg) {
	d.feed(tea.MouseClickMsg{X: x0, Y: y0, Button: tea.MouseLeft})
	d.feed(tea.MouseMotionMsg{X: x1, Y: y1, Button: tea.MouseLeft})
	text := d.m.selectedText()
	next, cmd := d.m.Update(tea.MouseReleaseMsg{X: x1, Y: y1, Button: tea.MouseLeft})
	d.m = next.(model)
	return text, runCmd(cmd)
}

// copyPayload is the OSC 52 text the clipboard command asked for.
func copyPayload(msgs []tea.Msg) (string, bool) {
	for _, m := range msgs {
		v := reflect.ValueOf(m)
		if fmt.Sprintf("%T", m) == "tea.setClipboardMsg" && v.Kind() == reflect.String {
			return v.String(), true
		}
	}
	return "", false
}

// copyStub replaces the native clipboard writer (not parallel-safe:
// callers must not use t.Parallel).
func copyStub(t *testing.T, via []string) *[]string {
	var got []string
	writeClipboardNative = func(s string) []string { got = append(got, s); return via }
	t.Cleanup(func() { writeClipboardNative = clipboardNative })
	return &got
}

// A folded block's header is what a drag over it copies — never the
// hidden body.
func TestCopyDragOverFoldedHeaderTakesHeaderOnly(t *testing.T) {
	d := defaultDrv(t)
	d.event("assistant", "before")
	d.event("result", "HIDDEN-ONE\nHIDDEN-TWO\nHIDDEN-THREE")
	d.event("done", "")
	row := -1
	for i, l := range strings.Split(d.plain(), "\n") {
		if strings.Contains(l, "▸") || strings.Contains(l, "▾") {
			row = i
		}
	}
	if row < 0 {
		t.Fatalf("no header:\n%s", d.plain())
	}
	if strings.Contains(d.plain(), "HIDDEN-TWO") {
		d.click(row)
	}
	if strings.Contains(d.plain(), "HIDDEN-TWO") {
		t.Fatalf("could not fold:\n%s", d.plain())
	}
	row = frameRow(d, "▸")
	text, _ := copyDrag(d, 0, row-2, 79, row+1)
	if strings.Contains(text, "HIDDEN-TWO") || strings.Contains(text, "HIDDEN-THREE") {
		t.Errorf("folded body leaked into the copy: %q", text)
	}
	if !strings.Contains(text, "before") {
		t.Errorf("copy lost the prose above: %q", text)
	}
}

// Box borders are decoration: a drag over an open result must not copy
// the │ rails or ╭─╮ lines.
func TestCopyDragOverOpenBoxDropsBorders(t *testing.T) {
	d := defaultDrv(t)
	d.event("result", "line-one\nline-two\nline-three")
	d.event("done", "")
	if h := frameRow(d, "▸ result"); h >= 0 {
		d.click(h)
	}
	r := frameRow(d, "line-two")
	if r < 0 {
		t.Fatalf("box not open:\n%s", d.plain())
	}
	text, _ := copyDrag(d, 0, r, 79, r+1)
	for _, g := range []string{"│", "╭", "╰", "─"} {
		if strings.Contains(text, g) {
			t.Fatalf("border %q copied: %q", g, text)
		}
	}
}

// A long unbroken line hard-wrapped at the pane width must come back as
// the one line the model wrote.
func TestCopyDragJoinsWrappedLine(t *testing.T) {
	d := defaultDrv(t)
	long := strings.Repeat("abcdefghij", 20) // 200 cells, wraps at 80
	d.event("assistant", long)
	d.event("done", "")
	r := frameRow(d, "abcdefghij")
	text, _ := copyDrag(d, 0, r, 79, r+2) // the three wrapped rows
	if strings.TrimSpace(text) != long {
		t.Errorf("wrapped line not rejoined:\n got %q\nwant %q", text, long)
	}
}

const copyWideSrc = "漢字かな🙂テスト 中文字符 👍 ok\n第二行 🎉🎉🎉 終わり"

// Wide runes: any selection edge (including mid-rune cells) copies
// whole runes only, never U+FFFD or broken UTF-8.
func TestCopyWideRunesNeverHalved(t *testing.T) {
	rapid.Check(t, func(rt *rapid.T) {
		d := defaultDrv(t)
		d.event("assistant", copyWideSrc)
		d.event("done", "")
		r := frameRow(d, "漢字")
		if r < 0 {
			rt.Fatalf("no row")
		}
		x0 := rapid.IntRange(0, 79).Draw(rt, "x0")
		x1 := rapid.IntRange(0, 79).Draw(rt, "x1")
		dy := rapid.IntRange(0, 1).Draw(rt, "dy")
		if dy == 0 && x0 == x1 {
			x1 = (x1 + 1) % 80
		}
		text, _ := copyDrag(d, x0, r, x1, r+dy)
		if !utf8.ValidString(text) || strings.ContainsRune(text, utf8.RuneError) {
			rt.Fatalf("broken rune in %q", text)
		}
		for _, ln := range strings.Split(text, "\n") {
			if s := strings.TrimSpace(ln); s != "" && !strings.Contains(copyWideSrc, s) {
				rt.Fatalf("copied %q is not a substring of the reply", s)
			}
		}
	})
}

// The live block growing below a selection does not change what the
// selection copies.
func TestCopySelectionStableWhileLiveBlockGrows(t *testing.T) {
	d := defaultDrv(t)
	d.event("assistant", "stable reply one")
	d.event("done", "")
	d.event("assistant-delta", "streaming ")
	r := frameRow(d, "stable reply one")
	d.feed(tea.MouseClickMsg{X: 0, Y: r, Button: tea.MouseLeft})
	d.feed(tea.MouseMotionMsg{X: 30, Y: r, Button: tea.MouseLeft})
	before := d.m.selectedText()
	if !strings.Contains(before, "stable reply one") {
		t.Fatalf("selection = %q", before)
	}
	for i := range 40 {
		d.event("assistant-delta", fmt.Sprintf("word%d ", i))
	}
	d.event("assistant-delta", "\nmore\nlines\n")
	if after := d.m.selectedText(); after != before {
		t.Errorf("selection drifted as the live block grew: %q -> %q", before, after)
	}
}

// A wheel scroll keeps the selection anchored to content, not to the
// screen: the copied text is the same after scrolling.
func TestCopySelectionSurvivesWheel(t *testing.T) {
	d := defaultDrv(t)
	for i := range 40 {
		d.event("assistant", fmt.Sprintf("reply line %02d", i))
	}
	d.event("done", "")
	r := frameRow(d, "reply line 3")
	if r < 0 {
		t.Fatalf("row not on screen:\n%s", d.plain())
	}
	d.feed(tea.MouseClickMsg{X: 0, Y: r, Button: tea.MouseLeft})
	d.feed(tea.MouseMotionMsg{X: 20, Y: r + 2, Button: tea.MouseLeft})
	before := d.m.selectedText()
	for range 3 {
		d.feed(tea.MouseWheelMsg{X: 5, Y: 3, Button: tea.MouseWheelUp})
	}
	if after := d.m.selectedText(); after != before || !d.m.sel.active {
		t.Errorf("wheel changed the selection: %q -> %q (active %v)", before, after, d.m.sel.active)
	}
}

// Dragging past every pane edge clamps, never panics, and never copies
// the composer or status bar.
func TestCopyDragBeyondPaneEdges(t *testing.T) {
	d := defaultDrv(t)
	d.event("assistant", "edge alpha\nedge beta")
	d.event("done", "")
	r := frameRow(d, "edge alpha")
	for _, to := range [][2]int{{500, r}, {-5, r}, {5, -20}, {5, 500}, {-9, -9}, {999, 999}} {
		text, _ := copyDrag(d, 3, r, to[0], to[1])
		if strings.Contains(text, "say something") {
			t.Errorf("drag to %v copied the composer: %q", to, text)
		}
		if !utf8.ValidString(text) {
			t.Errorf("drag to %v: invalid utf8", to)
		}
		_ = d.view()
	}
}

// Copy with a code block focused copies exactly the code the model
// wrote: no line numbers, no ANSI, no header.
func TestCopyFocusedCodeBlockIsExactSource(t *testing.T) {
	got := copyStub(t, []string{"stub"})
	d := defaultDrv(t)
	code := "const a = 1;\n\tif (a) {\n  console.log(\"x\\ty\");\n}"
	d.event("code", code)
	d.press(keyTab())
	d.feed(tea.KeyPressMsg{Code: 'x', Mod: tea.ModCtrl})
	msgs := d.press(keyRune('y'))
	osc, ok := copyPayload(msgs)
	if !ok {
		t.Fatalf("no OSC 52 write: %v", msgs)
	}
	if osc != code {
		t.Errorf("OSC 52 text = %q, want %q", osc, code)
	}
	if len(*got) != 1 || (*got)[0] != code {
		t.Errorf("native text = %q", *got)
	}
}

// The drag copy's OSC 52 and native payloads are the exact selected
// text, also for a >100 KB selection; the flash counts its lines.
func TestCopyLargeSelectionPayloadExact(t *testing.T) {
	got := copyStub(t, []string{"stub"})
	d := newDrv(t, 120, 30, cfgWith(t, nil, nil, nil))
	var b strings.Builder
	for i := range 1500 {
		fmt.Fprintf(&b, "row %04d %s\n", i, strings.Repeat("x", 90))
	}
	d.event("assistant", b.String())
	d.event("done", "")
	d.m.sel = selection{pressed: true, active: true, cy: len(d.m.lines) - 1, cx: 200}
	want := d.m.selectedText()
	if len(want) < 100_000 {
		t.Fatalf("selection only %d bytes", len(want))
	}
	next, cmd := d.m.Update(tea.MouseReleaseMsg{X: 0, Y: 0, Button: tea.MouseLeft})
	d.m = next.(model)
	msgs := runCmd(cmd)
	osc, ok := copyPayload(msgs)
	if !ok || osc != want {
		t.Fatalf("OSC 52 payload differs (%d vs %d bytes, ok %v)", len(osc), len(want), ok)
	}
	if len(*got) != 1 || (*got)[0] != want {
		t.Fatal("native payload differs")
	}
	for _, m := range msgs {
		d.feed(m)
	}
	if n := strings.Count(want, "\n") + 1; !strings.HasPrefix(d.m.flash, fmt.Sprintf("copied %d lines", n)) {
		t.Errorf("flash = %q", d.m.flash)
	}
}

// Native and tmux paths both failing leaves OSC 52 as the only
// (unconfirmed) path; with OSC 52 absent too, the flash says failed.
func TestCopyDragFlashWhenNativeFails(t *testing.T) {
	copyStub(t, nil)
	for _, tc := range []struct{ term, want string }{
		{"xterm-256color", "copied 8 chars · OSC 52"}, {"dumb", "copy failed"},
	} {
		t.Setenv("TERM", tc.term)
		d := defaultDrv(t)
		d.event("assistant", "flash me please")
		d.event("done", "")
		r := frameRow(d, "flash me")
		x := strings.Index(strings.Split(d.plain(), "\n")[r], "flash")
		_, msgs := copyDrag(d, x, r, x+8, r)
		for _, m := range msgs {
			d.feed(m)
		}
		if !strings.HasPrefix(d.m.flash, tc.want) {
			t.Errorf("TERM=%s flash = %q, want %q", tc.term, d.m.flash, tc.want)
		}
	}
}
