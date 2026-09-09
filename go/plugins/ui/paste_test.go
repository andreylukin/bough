package ui

// Paste routing (paste.go) and the clipboard writer (clipboard.go).

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

func TestTallPasteCollapsesAndExpandsOnSubmit(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	body := strings.Repeat("line\n", 30) + "last"
	d.typeStr("look: ")
	d.feed(tea.PasteMsg{Content: body})
	if got := d.m.input.Value(); got != "look: [Pasted text #1 +31 lines]" {
		t.Fatalf("draft = %q", got)
	}
	if d.m.input.Height() != 1 {
		t.Errorf("composer height = %d, want 1 (the placeholder is one row)", d.m.input.Height())
	}
	if !strings.Contains(d.m.flash, "expands when sent") {
		t.Errorf("flash = %q", d.m.flash)
	}
	d.typeStr(" ok")
	d.press(keyEnter())
	if len(d.sent) != 1 || d.sent[0] != "look: "+body+" ok" {
		t.Fatalf("sent = %q", d.sent)
	}
}

func TestLongOneLinePasteCollapsesByChars(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	body := strings.Repeat("y", pasteCollapseChars+1)
	d.feed(tea.PasteMsg{Content: body})
	if got := d.m.input.Value(); got != "[Pasted text #1 801 chars]" {
		t.Fatalf("draft = %q", got)
	}
	d.press(keyEnter())
	if len(d.sent) != 1 || d.sent[0] != body {
		t.Fatalf("sent %d messages, first %d chars", len(d.sent), len(strings.Join(d.sent, "")))
	}
}

// Deleting the placeholder drops the paste; two pastes keep their
// own numbers and expand independently.
func TestDeletedPlaceholderIsDropped(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	a := strings.Repeat("a\n", 20)
	b := strings.Repeat("b\n", 20)
	d.feed(tea.PasteMsg{Content: a})
	d.feed(tea.PasteMsg{Content: b})
	if got := d.m.input.Value(); got != "[Pasted text #1 +20 lines][Pasted text #2 +20 lines]" {
		t.Fatalf("draft = %q", got)
	}
	d.m.setDraft("keep [Pasted text #2 +20 lines] only")
	d.press(keyEnter())
	if len(d.sent) != 1 || d.sent[0] != "keep "+b+" only" {
		t.Fatalf("sent = %q", d.sent)
	}
}

// A small paste is typed text: editable in place, CRLF normalised.
func TestSmallPasteStaysInlineWithUnixNewlines(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.feed(tea.PasteMsg{Content: "one\r\ntwo\rthree"})
	if got := d.m.input.Value(); got != "one\ntwo\nthree" {
		t.Fatalf("draft = %q", got)
	}
	if len(d.m.comp.pastes) != 0 {
		t.Errorf("a small paste must not be stashed")
	}
}

// A pasted image path (drag-drop from Finder: quoted, escaped, or a
// file:// URL) becomes the @path reference, like ctrl+v's image paste.
func TestPastedImagePathBecomesReference(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	img := filepath.Join(dir, "shot 1.png")
	if err := os.WriteFile(img, []byte("png"), 0o644); err != nil {
		t.Fatal(err)
	}
	for _, in := range []string{img, `"` + img + `"`, strings.ReplaceAll(img, " ", `\ `), "file://" + strings.ReplaceAll(img, " ", "%20"), img + "\n"} {
		d := defaultDrv(t)
		d.feed(tea.PasteMsg{Content: in})
		if got := d.m.input.Value(); got != "@"+img+" " {
			t.Errorf("paste %q: draft = %q", in, got)
		}
	}
	// Not an image, or not on disk: plain text.
	for _, in := range []string{filepath.Join(dir, "missing.png"), filepath.Join(dir, "notes.txt"), dir} {
		d := defaultDrv(t)
		d.feed(tea.PasteMsg{Content: in})
		if got := d.m.input.Value(); got != in {
			t.Errorf("paste %q: draft = %q", in, got)
		}
	}
}

// The copy action takes the focused block's raw text, else the last
// reply; the clipboard write goes by OSC 52 and the native tool, and
// the flash names both.
func TestCopyActionCopiesLastReplyRaw(t *testing.T) {
	writeClipboardNative = func(string) []string { return []string{"stub"} }
	t.Cleanup(func() { writeClipboardNative = clipboardNative })
	d := defaultDrv(t)
	d.event("assistant", "**bold** reply that is long enough to wrap around the eighty column screen a few times over, surely")
	d.event("assistant", "second")
	d.feed(tea.KeyPressMsg{Code: 'x', Mod: tea.ModCtrl})
	msgs := d.press(keyRune('y'))
	var copied bool
	for _, msg := range msgs {
		if c, ok := msg.(copiedMsg); ok {
			copied = true
			if c.via[0] != "stub" {
				t.Errorf("via = %v", c.via)
			}
		}
	}
	if !copied {
		t.Fatalf("copy produced no copiedMsg: %v", msgs)
	}
	if d.m.flash != "copied 6 chars · stub + OSC 52" {
		t.Errorf("flash = %q", d.m.flash)
	}
}

func TestCopyActionWithNothingToCopy(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.feed(tea.KeyPressMsg{Code: 'x', Mod: tea.ModCtrl})
	d.press(keyRune('y'))
	if d.m.flash != "nothing to copy yet" {
		t.Errorf("flash = %q", d.m.flash)
	}
}
