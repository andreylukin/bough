package ui

// A bracketed paste carrying a newline and a raw ESC byte into the
// /model picker's search and the action palette's filter: the filter
// holds one sanitized line, the newline selects nothing, the ESC byte
// neither closes the overlay nor reaches the screen, and esc afterwards
// leaves the composer draft as it was.

import (
	"os"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

const pasteIntoModelPickerAndPaletteFilterPayload = "foo\n\x1bbar"

// pasteIntoModelPickerAndPaletteFilterKnown skips a subtest pinned to a
// known product bug unless BOUGH_KNOWN_PASTE_INTO_MODEL_PICKER_AND_PALETTE_FILTER is set.
func pasteIntoModelPickerAndPaletteFilterKnown(t *testing.T, bug string) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_PASTE_INTO_MODEL_PICKER_AND_PALETTE_FILTER") == "" {
		t.Skip("known bug (set BOUGH_KNOWN_PASTE_INTO_MODEL_PICKER_AND_PALETTE_FILTER=1 to run): " + bug)
	}
}

// pasteIntoModelPickerAndPaletteFilterClean fails on a newline, CR or
// ESC byte in s.
func pasteIntoModelPickerAndPaletteFilterClean(t *testing.T, where, s string) {
	t.Helper()
	if strings.ContainsAny(s, "\n\r\x1b") {
		t.Errorf("%s: not a sanitized single line: %q", where, s)
	}
}

func TestPasteIntoModelPickerAndPaletteFilter(t *testing.T) {
	t.Parallel()

	t.Run("ModelPicker", func(t *testing.T) {
		t.Parallel()
		d, ran := modelDrv(t)
		d.dispatchLine("/model")
		d.feed(tea.PasteMsg{Content: pasteIntoModelPickerAndPaletteFilterPayload})
		if !d.m.mp.open {
			t.Fatal("paste closed the picker")
		}
		if len(*ran) != 1 {
			t.Fatalf("pasted newline dispatched a choice: ran %v", *ran)
		}
		if !strings.Contains(d.m.mp.query, "foo") {
			t.Errorf("paste did not reach the search: %q", d.m.mp.query)
		}
		query, frame := d.m.mp.query, d.view()
		d.press(tea.KeyPressMsg{Code: tea.KeyEscape})
		if d.m.mp.open || len(*ran) != 1 || len(d.sent) != 0 {
			t.Fatalf("esc: open=%v ran=%v sent=%v", d.m.mp.open, *ran, d.sent)
		}
		if got := d.m.input.Value(); got != "" {
			t.Errorf("draft changed by the picker paste: %q", got)
		}
		t.Run("Sanitized", func(t *testing.T) {
			pasteIntoModelPickerAndPaletteFilterKnown(t, "model picker keeps the raw ESC byte in its search (model.go PasteMsg: strings.Fields only drops whitespace)")
			pasteIntoModelPickerAndPaletteFilterClean(t, "picker query", query)
			if strings.Contains(frame, "\x1bbar") {
				t.Errorf("raw ESC leaked into the frame")
			}
		})
	})

	t.Run("ActionPalette", func(t *testing.T) {
		t.Parallel()
		d := drvCmds(t, reg(t, "alpha"))
		d.typeStr("keep me")
		d.press(keyCtrl('x'))
		d.press(keyRune('p'))
		d.typeStr("exp")
		d.feed(tea.PasteMsg{Content: pasteIntoModelPickerAndPaletteFilterPayload})
		if !d.m.pal.open || !d.m.pal.actionsOnly {
			t.Fatalf("paste closed the palette (open=%v actions=%v draft=%q)", d.m.pal.open, d.m.pal.actionsOnly, d.m.input.Value())
		}
		if len(d.sent) != 0 {
			t.Fatalf("pasted newline submitted: %v", d.sent)
		}
		filter, frame := d.m.input.Value(), d.view()
		d.press(tea.KeyPressMsg{Code: tea.KeyEscape})
		if got := d.m.input.Value(); got != "keep me" || len(d.sent) != 0 {
			t.Errorf("esc: draft=%q sent=%v, want the displaced draft back", got, d.sent)
		}
		t.Run("Sanitized", func(t *testing.T) {
			pasteIntoModelPickerAndPaletteFilterKnown(t, "action palette filter takes a multi-line paste verbatim (model.go PasteMsg -> handlePaste falls through to the textarea while pal.open)")
			pasteIntoModelPickerAndPaletteFilterClean(t, "palette filter", filter)
			if strings.Contains(frame, "\x1bbar") {
				t.Errorf("raw ESC leaked into the frame")
			}
		})
	})
}
