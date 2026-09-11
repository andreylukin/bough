package vtreal

// The tools.ask flow on a real PTY: the model side comes off a tape
// (testdata/replay/ask.jsonl), the runtime side is the REAL codemode,
// so tools.ask actually blocks the turn on the user the way it does
// live. What is asserted: the pending ask block and its numbered
// options, the "waiting for you" status, the "? " tab title, the
// answer placeholder in the composer, and that every way of answering
// (number, freeform, click, esc) resumes the turn and consumes the
// tape's next reply.
//
// Note the tape keeps its recorded "ask"/"ask/answer"/"result" entries
// for shape, but they drive nothing: replay only feeds the model.

import (
	"fmt"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// askConfig is replayConfig's model side with the real codemode (and
// so the real ask plugin) left in place. The timeout is short so a
// broken answer path fails the test instead of hanging for 10 minutes.
func askConfig(tape string) string {
	return fmt.Sprintf(`
- id: llm
  plugin: replay
  config: {file: %q}
- id: ask
  plugin: ask
  config: {timeout_minutes: 1}
# Rows that call the model on their own would eat tape replies.
- id: session-title
  plugin: session-title
  disabled: true
- id: activity
  plugin: activity
  disabled: true
`, tape)
}

// askStart boots bough on the ask tape and submits the tape's prompt,
// leaving the turn blocked on the pending ask.
func askStart(t *testing.T) *app {
	t.Helper()
	tape, err := filepath.Abs("testdata/replay/ask.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	a := startCfg(t, 100, 30, askConfig(tape))
	a.typeText("pick a color")
	a.key(uv.KeyEnter, 0)
	a.waitFor("? Pick a color")
	return a
}

// askWaitTitle waits for the terminal title, which arrives out of band
// from the cell grid.
func askWaitTitle(a *app, want string) {
	a.t.Helper()
	deadline := time.Now().Add(15 * time.Second)
	for time.Now().Before(deadline) {
		if a.term.Snapshot().Title == want {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	a.t.Fatalf("title is %q, want %q\nscreen:\n%s", a.term.Snapshot().Title, want, a.text())
}

// askOptionRow is the screen row of the numbered option, -1 if absent.
func askOptionRow(a *app, n int, option string) int {
	for i, l := range a.lines() {
		if strings.Contains(l, fmt.Sprintf("%d.", n)) && strings.Contains(l, option) {
			return i
		}
	}
	return -1
}

// askAnswered asserts the ask collapsed to its answered one-liner and
// the next tape reply landed, i.e. the turn resumed.
func askAnswered(a *app, answer string) {
	a.t.Helper()
	want := "❯? Pick a color → " + answer
	a.waitUntil(func(s string) bool {
		return strings.Contains(s, want) && strings.Contains(s, "Color locked in.")
	}, "the answered ask one-liner "+want+" and the next tape reply")
	s := a.settled()
	if strings.Contains(s, "waiting for you") {
		a.t.Errorf("status bar still says waiting for you after answering:\n%s", s)
	}
	if strings.Contains(s, askPendingPlaceholder) {
		a.t.Errorf("composer still shows the answer placeholder after answering:\n%s", s)
	}
}

// askPendingPlaceholder is the composer placeholder while an ask is
// pending (plugins/ui/ask.go).
const askPendingPlaceholder = "type a number or your answer"

// While the ask is pending the block shows the question and its
// numbered options, the status bar says so, the title carries "? ",
// and the composer tells you how to answer.
func TestAskPendingState(t *testing.T) {
	t.Parallel()
	a := askStart(t)
	s := a.settled()
	for _, want := range []string{"? Pick a color", "1.", "chartreuse", "2.", "vermilion",
		"waiting for you", askPendingPlaceholder} {
		if !strings.Contains(s, want) {
			t.Errorf("pending ask screen missing %q:\n%s", want, s)
		}
	}
	askWaitTitle(a, "? pick a color")
}

// A bare number picks that option, the turn resumes and the tape's
// next reply is consumed; the title goes back to the finished state.
func TestAskNumberPicksOption(t *testing.T) {
	t.Parallel()
	a := askStart(t)
	a.settled()
	a.typeText("2")
	a.key(uv.KeyEnter, 0)
	askAnswered(a, "vermilion")
	askWaitTitle(a, "✓ pick a color")
}

// Anything that is not an option number is the literal answer.
func TestAskFreeformAnswer(t *testing.T) {
	t.Parallel()
	a := askStart(t)
	a.settled()
	a.typeText("octarine")
	a.key(uv.KeyEnter, 0)
	askAnswered(a, "octarine")
}

// Clicking an option row answers with that option.
func TestAskClickAnswersOption(t *testing.T) {
	t.Parallel()
	a := askStart(t)
	a.settled()
	row := askOptionRow(a, 1, "chartreuse")
	if row < 0 {
		t.Fatalf("no option row for chartreuse:\n%s", a.text())
	}
	a.click(5, row)
	askAnswered(a, "chartreuse")
}

// Esc declines the ask: the tool returns "(declined)" and the turn
// carries on rather than leaving the composer captured.
func TestAskEscDeclines(t *testing.T) {
	t.Parallel()
	a := askStart(t)
	a.settled()
	a.key(uv.KeyEscape, 0)
	askAnswered(a, "(declined)")
}
