package vtreal

// Error paths, replayed through the real binary on a real PTY: a block
// whose recorded result was "error: ...", a provider that dropped the
// stream mid-turn, a turn that spends its whole step budget, and the
// stop block the replay plugin returns past the end of the tape. Each
// case asserts the ✗ error block, that the turn still finished, and
// that the composer and status bar survived (app.check).

import (
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// errorsTape is the absolute path of one fixture tape.
func errorsTape(t *testing.T, name string) string {
	t.Helper()
	p, err := filepath.Abs(filepath.Join("testdata", "replay", name))
	if err != nil {
		t.Fatal(err)
	}
	return p
}

// errorsSend types one line, waits for the nth turn to finish, and
// checks the frame invariants.
func errorsSend(a *app, input string, turn int, where string) {
	a.t.Helper()
	a.typeText(input)
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(turn, 60*time.Second) {
		a.t.Fatalf("%s: turn never finished:\n%s", where, a.text())
	}
	a.check(where)
}

// errorsWant fails with the screen when it does not hold every substring.
func errorsWant(a *app, where string, want ...string) {
	a.t.Helper()
	s := a.settled()
	for _, w := range want {
		if !strings.Contains(s, w) {
			a.t.Errorf("%s: screen is missing %q:\n%s", where, w, s)
		}
	}
}

// errorsHistoryText is everything bough recorded under this run's $HOME,
// for notes that have already scrolled off the viewport.
func errorsHistoryText(a *app) string {
	a.t.Helper()
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var sb strings.Builder
	for _, p := range paths {
		entries, err := history.Read(p)
		if err != nil {
			continue
		}
		for _, e := range entries {
			text, _ := e.Data["text"].(string)
			sb.WriteString(e.Kind + ": " + text + "\n")
		}
	}
	return sb.String()
}

// TestErrorsExitStatus: a result recorded as "error: exit status 3"
// draws an error block and the turn still reaches its final answer.
func TestErrorsExitStatus(t *testing.T) {
	t.Parallel()
	tape := errorsTape(t, "errors-exit.jsonl")
	a := startCfg(t, 100, 30, replayConfig(tape))
	a.check("boot")
	errorsSend(a, "run the failing command", 1, "failing turn")
	errorsWant(a, "failing turn", "✗", "exit status 3", "Verified: the command exited 3")
}

// TestErrorsStreamEnded: the provider-side failure a session records as
// "llm-openrouter: stream ended" comes back as a block error, not a crash.
func TestErrorsStreamEnded(t *testing.T) {
	t.Parallel()
	tape := errorsTape(t, "errors-stream.jsonl")
	a := startCfg(t, 100, 30, replayConfig(tape))
	a.check("boot")
	errorsSend(a, "ask the sub agent", 1, "stream-ended turn")
	errorsWant(a, "stream-ended turn", "✗", "stream ended", "the provider dropped the stream")
}

// TestErrorsStepBudget: eight code blocks in a row against max_steps: 8
// spend the budget; the loop notes it and asks for a final answer, which
// the tape gives as plain text.
func TestErrorsStepBudget(t *testing.T) {
	t.Parallel()
	tape := errorsTape(t, "errors-budget.jsonl")
	cfg := replayConfig(tape) + `
- id: loop
  plugin: loop
  config: {max_steps: 8}
`
	a := startCfg(t, 100, 30, cfg)
	a.check("boot")
	errorsSend(a, "count to eight", 1, "budget turn")
	// The final answer is the last thing on screen; the note itself may
	// have scrolled out of the viewport, so read it from history.
	errorsWant(a, "budget turn", "ran out of room")
	if h := errorsHistoryText(a); !strings.Contains(h, "step budget spent (8 steps)") {
		t.Errorf("budget turn: history has no step-budget note:\n%s\nscreen:\n%s", h, a.text())
	}
}

// TestErrorsEndOfTape: an input past the end of the recording gets the
// replay plugin's stop block, and it renders as an ordinary final answer.
func TestErrorsEndOfTape(t *testing.T) {
	t.Parallel()
	tape := errorsTape(t, "errors-endtape.jsonl")
	a := startCfg(t, 100, 30, replayConfig(tape))
	a.check("boot")
	errorsSend(a, "say hello", 1, "recorded turn")
	errorsWant(a, "recorded turn", "Hello there.")
	errorsSend(a, "one turn too many", 2, "past end of tape")
	errorsWant(a, "past end of tape", "end of tape")
}
