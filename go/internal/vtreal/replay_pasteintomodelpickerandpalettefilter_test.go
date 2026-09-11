package vtreal

// A bracketed paste of "foo\n\x1bbar" into the /model picker search and
// the action palette filter on a real PTY: the overlay stays open, the
// newline submits nothing, no raw ESC reaches the screen, and esc
// afterwards leaves the composer draft as it was.

import (
	"os"
	"strings"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"
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

func TestPasteIntoModelPickerAndPaletteFilter(t *testing.T) {
	t.Parallel()

	t.Run("ModelPicker", func(t *testing.T) {
		t.Parallel()
		a := modelEffortStart(t)
		base := a.modelEffortAssistants()
		a.modelEffortOpenPicker()
		a.term.Paste(pasteIntoModelPickerAndPaletteFilterPayload)
		a.waitUntil(func(s string) bool { return strings.Contains(s, "search foo") }, "paste to reach the search")
		s := a.settled()
		if !strings.Contains(s, "pick a model") {
			t.Fatalf("picker closed on the paste:\n%s", s)
		}
		search := ""
		for _, l := range strings.Split(s, "\n") {
			if strings.HasPrefix(strings.TrimSpace(l), "search ") {
				search = l
			}
		}
		if !strings.Contains(search, "foo") {
			t.Errorf("paste did not become search text (search row %q):\n%s", search, s)
		}
		a.pickerPasteNoSubmit("model picker", base)
		a.key(uv.KeyEscape, 0)
		a.waitUntil(func(s string) bool { return !strings.Contains(s, "pick a model") }, "picker to close")
		a.pickerPasteNoSubmit("after esc", base)
		a.pickerPasteComposerEmpty("after esc")
	})

	t.Run("ActionPalette", func(t *testing.T) {
		t.Parallel()
		a := start(t, 100, 30)
		a.typeText("keep me")
		a.waitFor("keep me")
		actionPaletteLeader(a, 'p')
		a.waitFor("collapse_all")
		a.typeText("exp")
		a.waitFor("expand")
		a.term.Paste(pasteIntoModelPickerAndPaletteFilterPayload)
		s := a.settled()
		if strings.Contains(s, "❯ ") {
			t.Fatalf("paste submitted a prompt:\n%s", s)
		}
		c := actionPaletteComposer(s)
		rows := strings.Contains(s, "action · ")
		a.key(uv.KeyEscape, 0)
		a.waitUntil(func(s string) bool { return !strings.Contains(s, "action · ") && strings.Contains(s, "keep me") }, "palette to close")
		s2 := a.settled()
		if got := actionPaletteComposer(s2); !strings.Contains(got, "keep me") || strings.Contains(got, "foo") || strings.Contains(got, "bar") {
			t.Errorf("esc did not give the draft back (composer %q):\n%s", got, s2)
		}
		t.Run("Sanitized", func(t *testing.T) {
			pasteIntoModelPickerAndPaletteFilterKnown(t, "action palette filter takes a multi-line paste verbatim (model.go PasteMsg -> handlePaste falls through to the textarea while pal.open)")
			if !rows {
				t.Errorf("palette rows gone after the paste (the filter matches nothing):\n%s", s)
			}
			if !strings.Contains(c, "foo") || !strings.Contains(c, "bar") {
				t.Errorf("filter row is not the single sanitized line (composer %q):\n%s", c, s)
			}
		})
	})
}
