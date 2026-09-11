package vtreal

// A bracketed paste of "foo\nbar" into the /model and /sessions
// pickers: it may become search text (newline stripped) or be dropped,
// but the embedded newline must not submit or dispatch anything, and
// the composer draft must be untouched once esc closes the picker.

import (
	"os"
	"strings"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"
)

// pickerPasteKnown skips a subtest pinned to a known product bug
// unless BOUGH_KNOWN_PICKER_PASTE is set.
func pickerPasteKnown(t *testing.T, bug string) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_PICKER_PASTE") == "" {
		t.Skip("known bug (set BOUGH_KNOWN_PICKER_PASTE=1 to run): " + bug)
	}
}

// pickerPasteNoSubmit fails when the paste reached the transcript as a
// sent prompt or consumed a tape reply beyond the base assistant count.
func (a *app) pickerPasteNoSubmit(where string, base int) {
	a.t.Helper()
	s := a.settled()
	if strings.Contains(s, "❯ foo") || strings.Contains(s, "❯ bar") {
		a.t.Fatalf("%s: paste was submitted as a prompt:\n%s", where, s)
	}
	if n := a.modelEffortAssistants() - base; n != 0 {
		a.t.Fatalf("%s: paste consumed %d tape replies:\n%s", where, n, s)
	}
}

// pickerPasteComposerEmpty checks the composer is back holding only its
// placeholder: no foo, no bar, no paste tag.
func (a *app) pickerPasteComposerEmpty(where string) {
	a.t.Helper()
	s := a.settled()
	ls := strings.Split(s, "\n")
	row := composerRow(ls)
	if row < 0 {
		a.t.Fatalf("%s: no composer on screen:\n%s", where, s)
	}
	for _, l := range ls[row:] {
		if strings.Contains(l, "foo") || strings.Contains(l, "bar") || strings.Contains(l, "[Pasted text") {
			a.t.Fatalf("%s: composer draft changed by the picker paste (row %q):\n%s", where, l, s)
		}
	}
	if !strings.Contains(s, "say something") {
		a.t.Fatalf("%s: composer placeholder gone, draft not empty:\n%s", where, s)
	}
}

func TestPickerPaste(t *testing.T) {
	t.Parallel()

	t.Run("Model", func(t *testing.T) {
		t.Parallel()
		a := modelEffortStart(t)
		base := a.modelEffortAssistants()
		a.modelEffortOpenPicker()
		a.term.Paste("foo\nbar")
		s := a.settled()
		if !strings.Contains(s, "pick a model") {
			t.Fatalf("picker closed on the pasted newline:\n%s", s)
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

	t.Run("Sessions", func(t *testing.T) {
		t.Parallel()
		pickerPasteKnown(t, "tea.PasteMsg bypasses the /sessions picker (plugins/ui/model.go Update PasteMsg -> handlePaste -> m.input): foo/bar land in the hidden composer draft")
		a := modelEffortStart(t)
		newSessionSeed(t, a, "seed-alpha", "/elsewhere/alpha", "alpha prompt")
		base := a.modelEffortAssistants() // the seed has its own reply
		newSessionOpenPicker(a)
		a.term.Paste("foo\nbar")
		s := a.settled()
		if !strings.Contains(s, "resume a session") {
			t.Fatalf("picker closed or dispatched on the pasted newline:\n%s", s)
		}
		a.pickerPasteNoSubmit("sessions picker", base)
		a.key(uv.KeyEscape, 0)
		a.waitUntil(func(s string) bool { return !strings.Contains(s, "resume a session") }, "picker to close")
		a.pickerPasteNoSubmit("after esc", base)
		a.pickerPasteComposerEmpty("after esc")
	})
}
