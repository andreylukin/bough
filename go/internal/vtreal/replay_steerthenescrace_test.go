package vtreal

// Enter-steer and esc in ONE pty write while a reply streams: the
// enter lands the steer, the esc cancels the turn before the loop
// reaches its next boundary. The turn must end cancelled, and the
// steer text must not vanish — it is either back in the composer or
// recorded in history (a steer/queued input) — and the next turn must
// run on the rest of the tape.

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

const steerThenEscRaceText = "use the staging bucket instead"

// steerThenEscRaceTape: a long first reply, then two short ones so the
// next turn has a reply whether or not the steer consumed one.
func steerThenEscRaceTape(t *testing.T) string {
	t.Helper()
	long := "ALPHASTART" + strings.Repeat(" filler", 200) + " ALPHAEND"
	entries := []map[string]any{
		{"kind": "input", "data": map[string]any{"text": "start the long one"}},
		{"kind": "assistant", "data": map[string]any{"text": long}},
		{"kind": "done", "data": map[string]any{"text": ""}},
		{"kind": "input", "data": map[string]any{"text": "second"}},
		{"kind": "assistant", "data": map[string]any{"text": "```stop\nBETAREPLY\n```"}},
		{"kind": "done", "data": map[string]any{"text": ""}},
		{"kind": "input", "data": map[string]any{"text": "third"}},
		{"kind": "assistant", "data": map[string]any{"text": "```stop\nGAMMAREPLY\n```"}},
		{"kind": "done", "data": map[string]any{"text": ""}},
	}
	var sb strings.Builder
	for i, e := range entries {
		e["seq"] = i + 1
		e["at"] = "2026-09-11T10:00:00Z"
		b, err := json.Marshal(e)
		if err != nil {
			t.Fatal(err)
		}
		sb.Write(b)
		sb.WriteByte('\n')
	}
	p := filepath.Join(t.TempDir(), "race.jsonl")
	if err := os.WriteFile(p, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

func TestSteerThenEscRace(t *testing.T) {
	t.Parallel()
	a := startCfg(t, 100, 30, cancelConfig(steerThenEscRaceTape(t), 120))

	a.typeText("start the long one\r")
	a.waitFor("ALPHASTART")
	// One write: the steer, its enter and the esc arrive together.
	a.typeText(steerThenEscRaceText + "\r\x1b")

	a.waitFor("■ cancelled")
	a.waitUntil(func(s string) bool { return !cancelSpinner.MatchString(s) },
		"the spinner to stop after the cancel")
	screen := a.settled()
	a.check("after steer+esc")

	t.Run("TestSteerThenEscRaceCancelled", func(t *testing.T) {
		if strings.Contains(screen, "ALPHAEND") {
			t.Errorf("the reply kept streaming after esc:\n%s", screen)
		}
		// A cancel records "cancelled" then the usual "done" (loop/cancel.go).
		var kinds []string
		for _, e := range a.steerEntries() {
			switch e.Kind {
			case "assistant", "cancelled", "done":
				kinds = append(kinds, e.Kind)
			}
		}
		if got := strings.Join(kinds, " "); got != "cancelled done" {
			t.Errorf("history tail = %q, want \"cancelled done\":\n%s", got, screen)
		}
	})

	t.Run("TestSteerThenEscRaceSteerNotLost", func(t *testing.T) {
		ls := strings.Split(screen, "\n")
		inComposer := false
		if r := composerRow(ls); r >= 0 && strings.Contains(ls[r], steerThenEscRaceText) {
			inComposer = true
		}
		inHistory := false
		for _, e := range a.steerEntries() {
			if txt, _ := e.Data["text"].(string); e.Kind == "input" && txt == steerThenEscRaceText {
				inHistory = true
			}
		}
		if !inComposer && !inHistory {
			t.Errorf("steer text neither restored to the composer nor recorded in history:\n%s", screen)
		}
		if inHistory && !strings.Contains(screen, steerThenEscRaceText) {
			t.Errorf("steer recorded in history but not shown on screen:\n%s", screen)
		}
	})

	t.Run("TestSteerThenEscRaceNextTurn", func(t *testing.T) {
		// Clear whatever the composer holds (a restored steer) first.
		ls := strings.Split(screen, "\n")
		if r := composerRow(ls); r >= 0 && !cancelComposerEmpty(ls[r]) {
			a.typeText("\x15") // ctrl+u
			a.waitUntil(func(s string) bool {
				l := strings.Split(s, "\n")
				r := composerRow(l)
				return r >= 0 && cancelComposerEmpty(l[r])
			}, "the composer to clear")
		}
		a.typeText("second\r")
		if !a.waitDone(3, 60*time.Second) { // cancelled + done, then this turn's done
			t.Fatalf("next turn never finished:\n%s", a.text())
		}
		s := a.settled()
		if !strings.Contains(s, "BETAREPLY") && !strings.Contains(s, "GAMMAREPLY") {
			t.Errorf("next turn did not render a tape reply:\n%s", s)
		}
		for _, bad := range []string{"ALPHAEND", "end of tape"} {
			if strings.Contains(s, bad) {
				t.Errorf("%q on screen after the next turn:\n%s", bad, s)
			}
		}
		a.check("after the next turn")
	})
}
