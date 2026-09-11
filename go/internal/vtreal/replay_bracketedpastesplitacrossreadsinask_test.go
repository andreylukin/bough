package vtreal

// A bracketed paste whose ESC [200~ / ESC [201~ markers are split
// across PTY writes 50ms apart (so bough's reader sees them in separate
// reads), into the ask freeform answer and the /sessions picker. The
// content must land once, no marker remnant may reach the screen, and
// the newlines inside the paste must not submit.
//
// The sessions picker has no filter field (plugins/ui/session.go
// handlePickerKey: up/down/enter/esc only), so there the paste lands in
// the composer behind it: asserted as "picker unmoved, composer holds
// the paste once after esc".

import (
	"os"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// bracketedPasteSplitWrite writes chunks straight to the PTY with a
// 50ms gap between them.
func bracketedPasteSplitWrite(a *app, chunks ...string) {
	a.t.Helper()
	for i, c := range chunks {
		if i > 0 {
			time.Sleep(50 * time.Millisecond)
		}
		if _, err := a.term.pty.Write([]byte(c)); err != nil {
			a.t.Fatal(err)
		}
	}
}

// bracketedPasteSplitNoRemnants fails on any piece of a paste marker.
func bracketedPasteSplitNoRemnants(a *app, s string) {
	a.t.Helper()
	for _, bad := range []string{"200~", "201~", "00~", "01~", "[200", "[201", "\x1b"} {
		if strings.Contains(s, bad) {
			a.t.Errorf("paste marker remnant %q on screen:\n%s", bad, s)
		}
	}
}

// bracketedPasteSplitKnown skips unless
// BOUGH_KNOWN_BRACKETED_PASTE_SPLIT_ACROSS_READS_IN_ASK is set: the input
// reader (ultraviolet EscTimeout, 50ms) flushes a partial ESC [200~ at a
// read gap instead of waiting for the rest, so the marker leaks as text
// and the paste body arrives as keystrokes (its newline submits).
func bracketedPasteSplitKnown(t *testing.T) {
	if os.Getenv("BOUGH_KNOWN_BRACKETED_PASTE_SPLIT_ACROSS_READS_IN_ASK") == "" {
		t.Skip("known bug: split bracketed-paste markers leak and the paste body submits; set BOUGH_KNOWN_BRACKETED_PASTE_SPLIT_ACROSS_READS_IN_ASK to run")
	}
}

func TestBracketedPasteSplitAcrossReadsInAsk(t *testing.T) {
	t.Parallel()

	t.Run("FreeformSingleLine", func(t *testing.T) {
		t.Parallel()
		bracketedPasteSplitKnown(t)
		a := askStart(t)
		a.settled()
		bracketedPasteSplitWrite(a, "\x1b[2", "00~octa", "rine\x1b[20", "1~")
		a.waitFor("octarine")
		s := a.settled()
		bracketedPasteSplitNoRemnants(a, s)
		if n := strings.Count(s, "octarine"); n != 1 {
			t.Errorf("pasted text appears %d times, want 1:\n%s", n, s)
		}
		a.key(uv.KeyEnter, 0)
		askAnswered(a, "octarine")
	})

	t.Run("FreeformMultiLineNotSubmitted", func(t *testing.T) {
		t.Parallel()
		bracketedPasteSplitKnown(t)
		a := askStart(t)
		a.settled()
		bracketedPasteSplitWrite(a, "\x1b", "[200~alpha", "\rbeta\n", "gamma\x1b[", "201~")
		a.waitFor("gamma")
		s := a.settled()
		bracketedPasteSplitNoRemnants(a, s)
		for _, w := range []string{"alpha", "beta", "gamma"} {
			if n := strings.Count(s, w); n != 1 {
				t.Errorf("%q appears %d times, want 1:\n%s", w, n, s)
			}
		}
		if strings.Contains(s, "Color locked in.") || strings.Contains(s, "Pick a color →") {
			t.Fatalf("a newline inside the paste answered the ask:\n%s", s)
		}
		if !strings.Contains(s, "waiting for you") {
			t.Errorf("ask no longer pending after paste:\n%s", s)
		}
	})

	t.Run("SessionPicker", func(t *testing.T) {
		t.Parallel()
		bracketedPasteSplitKnown(t)
		a := start(t, 120, 30)
		newSessionSeed(t, a, "seed-alpha", "/elsewhere/alpha", "alpha prompt")
		newSessionOpenPicker(a)
		before := a.settled()
		bracketedPasteSplitWrite(a, "\x1b[200", "~zeta\r", "eta\x1b", "[201~")
		time.Sleep(300 * time.Millisecond)
		s := a.settled()
		bracketedPasteSplitNoRemnants(a, s)
		if !strings.Contains(s, "resume a session") || s != before {
			t.Errorf("paste changed or closed the picker:\nbefore:\n%s\nafter:\n%s", before, s)
		}
		if strings.Contains(s, "resumed seed-alpha") {
			t.Fatalf("a newline in the paste resumed a session:\n%s", s)
		}
		a.key(uv.KeyEscape, 0)
		a.waitUntil(func(s string) bool { return !strings.Contains(s, "resume a session") }, "picker to close")
		s = a.settled()
		bracketedPasteSplitNoRemnants(a, s)
		if strings.Count(s, "zeta") != 1 || strings.Count(s, "eta") != 2 { // "eta" is inside "zeta"
			t.Errorf("composer should hold the paste once:\n%s", s)
		}
		if strings.Contains(s, "echo:") || strings.Contains(s, "resumed seed-alpha") {
			t.Errorf("paste was submitted:\n%s", s)
		}
	})
}
