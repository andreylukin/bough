package vtreal

// "?" (the keymap help) while a tools.ask is pending, on the ask tape
// (testdata/replay/ask.jsonl). Help must not eat the ask: the first esc
// closes help only, the ask stays pending with its options, and a
// number then answers it. The model's view of the answer is the tool
// result the runtime records ("result" entry, fed to the next tape
// request); the replay plugin keeps no request log of its own.

import (
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// helpOverlayDuringAskKeysLine is a line only the keymap help shows.
const helpOverlayDuringAskKeysLine = "on an empty composer: this list (/keys)"

// helpOverlayDuringAskResult waits for the run's "result" entry and
// returns its text: what the model sees in its next request.
func helpOverlayDuringAskResult(a *app) string {
	a.t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		for _, e := range pasteEntries(a) {
			if e.Kind == "result" {
				text, _ := e.Data["text"].(string)
				return text
			}
		}
		time.Sleep(50 * time.Millisecond)
	}
	a.t.Fatalf("no result entry was recorded:\n%s", a.text())
	return ""
}

func TestHelpOverlayDuringAsk(t *testing.T) {
	t.Parallel()
	t.Run("help_opens_over_pending_ask", func(t *testing.T) {
		t.Parallel()
		a := askStart(t)
		a.settled()
		a.typeText("?")
		a.waitFor(helpOverlayDuringAskKeysLine)
		s := a.settled()
		// The help block is taller than the screen, so pending shows in
		// the status bar and the composer placeholder, not the options.
		if !strings.Contains(s, "waiting for you") || !strings.Contains(s, askPendingPlaceholder) ||
			strings.Contains(s, "Color locked in.") {
			t.Fatalf("opening help disturbed the pending ask:\n%s", s)
		}
	})
	t.Run("esc_closes_help_only_then_number_answers", func(t *testing.T) {
		t.Parallel()
		a := askStart(t)
		a.settled()
		a.typeText("?")
		a.waitFor(helpOverlayDuringAskKeysLine)
		a.settled()
		a.key(uv.KeyEscape, 0)
		// Wait for the repaint: settled() alone can return the pre-esc
		// screen when the redraw lags under a parallel run.
		a.waitUntil(func(s string) bool { return !strings.Contains(s, helpOverlayDuringAskKeysLine) },
			"first esc to close help")
		s := a.settled()
		if strings.Contains(s, "(declined)") || strings.Contains(s, "Color locked in.") ||
			askOptionRow(a, 1, "chartreuse") < 0 || askOptionRow(a, 2, "vermilion") < 0 {
			t.Fatalf("first esc resolved the ask instead of only closing help:\n%s", s)
		}
		a.typeText("1")
		a.key(uv.KeyEnter, 0)
		askAnswered(a, "chartreuse")
		if got := helpOverlayDuringAskResult(a); got != "you picked chartreuse\n" {
			t.Fatalf("model got tool result %q, want %q", got, "you picked chartreuse\n")
		}
	})
}
