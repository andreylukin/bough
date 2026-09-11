package vtreal

// A second model service on the tape: `service: llm-small` gives the
// replay plugin a tape of its own, so the rows that call the cheap
// model — session-title and the composer's autocomplete —
// can be ENABLED in a replay run without eating the agent's replies.
// The two tapes have separate cursors, so each test asserts both
// halves: what the small tape produced, and that the main tape is
// still where it was (turn two must still answer MAIN-TWO).
//
// Only one small-model consumer is enabled per subtest: they would
// otherwise race for the same tape cursor and the run would not be
// deterministic.

import (
	"fmt"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// llmSmallConfig is replayConfig plus an llm-small row on its own
// tape; extra is the row list that turns one small-model consumer
// back on (they are all disabled here by default).
func llmSmallConfig(main, small, extra string) string {
	return fmt.Sprintf(`
- id: llm
  plugin: replay
  config: {file: %q}
- id: codemode
  plugin: replay
  config: {file: %q, provide: codemode}
- id: llm-small
  plugin: replay
  config: {file: %q, service: llm-small}
- id: commands
  plugin: commands
- id: history
  plugin: history
- id: loop
  plugin: loop
- id: ui
  plugin: ui
- id: session-title
  plugin: session-title
  disabled: true
- id: activity
  plugin: activity
  disabled: true
%s`, main, main, small, extra)
}

func llmSmallTapes(t *testing.T, small string) (string, string) {
	t.Helper()
	m, err := filepath.Abs("testdata/replay/llmsmall-main.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	s, err := filepath.Abs(filepath.Join("testdata/replay", small))
	if err != nil {
		t.Fatal(err)
	}
	return m, s
}

// llmSmallTurn types one line, submits it and waits for the turn to end.
func llmSmallTurn(a *app, in string, done int) {
	a.t.Helper()
	a.typeText(in)
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(done, 60*time.Second) {
		a.t.Fatalf("turn %q never finished:\n%s", in, a.text())
	}
}

// The title comes off the small tape: the status bar and the terminal
// title carry it, and the main tape still holds both of its turns.
func TestLlmSmallTitle(t *testing.T) {
	t.Parallel()
	main, small := llmSmallTapes(t, "llmsmall-title.jsonl")
	a := startCfg(t, 100, 30, llmSmallConfig(main, small, `
- id: session-title
  plugin: session-title
`))
	a.check("boot")

	llmSmallTurn(a, "list the files here", 1)
	if s := a.text(); !strings.Contains(s, "MAIN-ONE") {
		t.Fatalf("the first main-tape turn did not land:\n%s", s)
	}
	a.waitUntil(func(s string) bool { return strings.Contains(s, "Fix the flaky golden test") },
		"the session title from the small tape")
	a.check("named")

	if got := a.term.Snapshot().Title; !strings.Contains(got, "Fix the flaky golden test") {
		t.Fatalf("terminal title = %q, want the session title in it\nscreen:\n%s", got, a.text())
	}
	if s := a.text(); strings.Contains(s, "SMALL-SECOND") {
		t.Fatalf("the small tape was read more than once (one name per session):\n%s", s)
	}

	// The naming call must not have moved the main tape's cursor: turn
	// two is still turn two, not turn one's reply or the end of tape.
	llmSmallTurn(a, "and the tests", 2)
	s := a.settled()
	if !strings.Contains(s, "MAIN-TWO") {
		t.Fatalf("the main tape was consumed by the title call:\n%s", s)
	}
	if strings.Contains(s, "end of tape") {
		t.Fatalf("the main tape ran out early:\n%s", s)
	}
	a.check("second turn")
}

// The ↹ chip is the small model's guess at the rest of the draft; tab
// takes it, and the agent's own tape is untouched by the guessing.
func TestLlmSmallSuggestion(t *testing.T) {
	t.Parallel()
	main, small := llmSmallTapes(t, "llmsmall-predict.jsonl")
	a := startCfg(t, 100, 30, llmSmallConfig(main, small, ""))
	a.check("boot")

	a.typeText("fix the flaky")
	a.waitUntil(func(s string) bool { return strings.Contains(s, "↹ …golden test paths") },
		"the suggestion chip on the status line")
	a.check("suggested")

	a.key(uv.KeyTab, 0)
	a.waitUntil(func(s string) bool {
		for _, l := range strings.Split(s, "\n") {
			if strings.HasPrefix(l, "> ") && strings.Contains(l, "fix the flaky golden test paths") {
				return true
			}
		}
		return false
	}, "tab to append the suggestion to the draft")
	if s := a.settled(); strings.Contains(s, "↹ …") {
		t.Fatalf("the chip outlived being accepted:\n%s", s)
	}

	// Submitting now still gets the main tape's FIRST reply: the
	// guesses came off the small tape only.
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("the turn never finished:\n%s", a.text())
	}
	if s := a.settled(); !strings.Contains(s, "MAIN-ONE") {
		t.Fatalf("the main tape was consumed by the autocomplete:\n%s", s)
	}
	a.check("after the turn")
}
