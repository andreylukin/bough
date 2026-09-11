package vtreal

// ask-steer-cancel: while tools.ask is pending, a typed line + Enter
// (what would otherwise steer the running turn) is routed as the ask's
// answer, and an Esc right behind it must not ALSO decline it. Exactly
// one of answer/decline is recorded, the turn ends, no spinner is left
// behind and the status bar stops saying the turn waits on the ask.

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// askSteerCancelAnswers returns the texts of every "ask/answer" entry
// in this run's session files.
func askSteerCancelAnswers(a *app) []string {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var out []string
	for _, p := range paths {
		b, err := os.ReadFile(p)
		if err != nil {
			continue
		}
		for _, l := range strings.Split(string(b), "\n") {
			var e struct {
				Kind string `json:"kind"`
				Data struct {
					Text string `json:"text"`
				} `json:"data"`
			}
			if json.Unmarshal([]byte(l), &e) == nil && e.Kind == "ask/answer" {
				out = append(out, e.Data.Text)
			}
		}
	}
	return out
}

// askSteerCancelSettle asserts the end state shared by every variant.
func askSteerCancelSettle(t *testing.T, a *app, answers ...string) {
	t.Helper()
	if !a.waitDone(1, 20*time.Second) {
		t.Fatalf("turn never recorded done/cancelled:\n%s", a.text())
	}
	a.waitUntil(func(s string) bool { return !cancelSpinner.MatchString(s) },
		"the spinner to stop")
	s := a.settled()
	if strings.Contains(s, "waiting for you") {
		t.Errorf("status bar still says waiting for you:\n%s", s)
	}
	if strings.Contains(s, askPendingPlaceholder) {
		t.Errorf("composer still shows the ask placeholder:\n%s", s)
	}
	got := askSteerCancelAnswers(a)
	if len(got) != 1 {
		t.Fatalf("want exactly one ask/answer entry, got %q\n%s", got, s)
	}
	ok := false
	for _, w := range answers {
		ok = ok || got[0] == w
	}
	if !ok {
		t.Fatalf("recorded answer %q, want one of %q", got[0], answers)
	}
	if strings.Contains(s, "(steer") {
		t.Errorf("the answer line was also shown as a steer:\n%s", s)
	}
	a.check("ask-steer-cancel end")
}

func TestAskSteerCancel(t *testing.T) {
	t.Parallel()
	// Enter then Esc back to back: the answer already went out, so Esc
	// may cancel the resumed turn but must not add a decline.
	t.Run("enter-then-esc-immediately", func(t *testing.T) {
		t.Parallel()
		a := askStart(t)
		a.settled()
		a.typeText("octarine")
		a.key(uv.KeyEnter, 0)
		a.key(uv.KeyEscape, 0)
		askSteerCancelSettle(t, a, "octarine")
	})
	// Esc after the answered one-liner shows: the turn finishes normally.
	t.Run("enter-then-esc-after-answer", func(t *testing.T) {
		t.Parallel()
		a := askStart(t)
		a.settled()
		a.typeText("octarine")
		a.key(uv.KeyEnter, 0)
		a.waitFor("❯? Pick a color → octarine")
		a.key(uv.KeyEscape, 0)
		askSteerCancelSettle(t, a, "octarine")
	})
	// A typed-but-unsent draft then Esc: that is the decline path.
	t.Run("draft-then-esc", func(t *testing.T) {
		t.Parallel()
		a := askStart(t)
		a.settled()
		a.typeText("octarine")
		a.key(uv.KeyEscape, 0)
		askSteerCancelSettle(t, a, "(declined)", "")
	})
}
