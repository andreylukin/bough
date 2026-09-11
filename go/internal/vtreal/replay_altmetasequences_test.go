package vtreal

// ESC-prefixed meta keys as a legacy terminal sends them (no kitty
// protocol): alt+b/f/backspace move and delete by word in the draft,
// alt+. is unbound and inserts nothing, and a lone ESC followed a few
// milliseconds later by 'x' is neither a double-esc clear nor a cancel.

import (
	"strings"
	"testing"
	"time"
)

// altmetaDraft returns the composer row's text after the "> " prompt.
func altmetaDraft(a *app) string {
	a.t.Helper()
	ls := a.lines()
	i := composerRow(ls)
	if i < 0 {
		a.t.Fatalf("no composer on screen:\n%s", a.text())
	}
	return strings.TrimPrefix(ls[i], "> ")
}

// altmetaWant waits for the draft to read exactly want.
func altmetaWant(a *app, want string) {
	a.t.Helper()
	a.waitUntil(func(string) bool {
		ls := a.lines()
		i := composerRow(ls)
		return i >= 0 && strings.TrimPrefix(ls[i], "> ") == want
	}, "draft "+want)
	a.settled()
	if got := altmetaDraft(a); got != want {
		a.t.Fatalf("draft = %q, want %q\nscreen:\n%s", got, want, a.text())
	}
}

func TestAltMetaSequences(t *testing.T) {
	t.Run("word_motions", func(t *testing.T) {
		a := start(t, 100, 30)
		a.typeText("one two three")
		altmetaWant(a, "one two three")
		a.typeText("\x1bb") // alt+b: to the start of "three"
		a.typeText("X")
		altmetaWant(a, "one two Xthree")
		a.typeText("\x1bb\x1bb") // back to the start of "two"
		a.typeText("\x1bf")      // alt+f: to the end of "two"
		a.typeText("Y")
		altmetaWant(a, "one twoY Xthree")
	})
	t.Run("alt_backspace", func(t *testing.T) {
		a := start(t, 100, 30)
		a.typeText("one two three")
		altmetaWant(a, "one two three")
		a.typeText("\x1b\x7f") // alt+backspace
		a.typeText("Z")
		altmetaWant(a, "one two Z")
	})
	t.Run("alt_dot_inserts_nothing", func(t *testing.T) {
		a := start(t, 100, 30)
		a.typeText("abc")
		altmetaWant(a, "abc")
		a.typeText("\x1b.")
		a.typeText("!")
		altmetaWant(a, "abc!")
	})
	t.Run("esc_then_x_is_not_double_esc", func(t *testing.T) {
		a := start(t, 100, 30)
		a.typeText("keep me")
		altmetaWant(a, "keep me")
		before := a.settled()
		a.typeText("\x1b")
		time.Sleep(3 * time.Millisecond)
		a.typeText("x")
		screen := a.settled()
		d := altmetaDraft(a)
		// Read either as alt+x (inserts nothing) or esc then x: the
		// draft must survive, with at most the x appended.
		if d != "keep me" && d != "keep mex" {
			t.Fatalf("draft = %q after ESC x\nscreen:\n%s", d, screen)
		}
		// A second lone esc must only ARM the clear: if the first esc
		// stayed armed through the x, this one would wipe the draft.
		a.typeText("\x1b")
		a.waitFor("press esc again to clear the draft")
		if got := altmetaDraft(a); got != d {
			t.Fatalf("second esc changed draft %q -> %q\nscreen:\n%s", d, got, a.text())
		}
		// No turn was running: nothing was cancelled or sent.
		after := a.text()
		for _, bad := range []string{"cancel", "interrupted"} {
			if strings.Contains(after, bad) && !strings.Contains(before, bad) {
				t.Fatalf("idle ESC x printed %q\nscreen:\n%s", bad, after)
			}
		}
	})
}
