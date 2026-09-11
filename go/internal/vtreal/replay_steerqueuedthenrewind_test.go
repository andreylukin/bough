package vtreal

// steer-queued-then-rewind: while a reply streams, one line is entered
// as a steer (Enter) and a second queued (alt+enter); then esc esc —
// the first esc cancels the turn (stop.go escPress), the second arms
// the idle double-esc. Neither pending line may vanish silently: each
// is back in the composer or still shown in the transcript. History
// must hold no user entry the screen does not account for, and the
// next submit must send exactly one copy of what the composer holds.
//
// The replay plugin has no release gate, so the first reply is held
// open by a long body at a slow delay_ms: it cannot finish (≈60 s)
// before the test's esc, which is the only "release" it gets.

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const (
	steerQueuedThenRewindSteer = "SQRSTEER use staging"
	steerQueuedThenRewindQueue = "SQRQUEUE then deploy"
)

func steerQueuedThenRewindTape(t *testing.T) string {
	t.Helper()
	long := "HELDSTART" + strings.Repeat(" held", 300) + " HELDEND"
	entries := []map[string]any{
		{"kind": "input", "data": map[string]any{"text": "start held"}},
		{"kind": "assistant", "data": map[string]any{"text": long}},
		{"kind": "done", "data": map[string]any{"text": ""}},
		{"kind": "input", "data": map[string]any{"text": "next"}},
		{"kind": "assistant", "data": map[string]any{"text": "```stop\nNEXTREPLY\n```"}},
		{"kind": "done", "data": map[string]any{"text": ""}},
		{"kind": "input", "data": map[string]any{"text": "extra"}},
		{"kind": "assistant", "data": map[string]any{"text": "```stop\nEXTRAREPLY\n```"}},
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
	p := filepath.Join(t.TempDir(), "sqr.jsonl")
	if err := os.WriteFile(p, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// steerQueuedThenRewindInputs is the text of every "input" entry.
func steerQueuedThenRewindInputs(a *app) []string {
	var out []string
	for _, e := range a.steerEntries() {
		if e.Kind == "input" {
			s, _ := e.Data["text"].(string)
			out = append(out, s)
		}
	}
	return out
}

func steerQueuedThenRewindCount(xs []string, sub string) int {
	n := 0
	for _, x := range xs {
		if strings.Contains(x, sub) {
			n++
		}
	}
	return n
}

func TestSteerQueuedThenRewind(t *testing.T) {
	t.Parallel()
	a := startCfg(t, 100, 30, cancelConfig(steerQueuedThenRewindTape(t), 200))

	a.typeText("start held")
	a.key(uv.KeyEnter, 0)
	a.waitFor("HELDSTART")
	a.typeText(steerQueuedThenRewindSteer)
	a.key(uv.KeyEnter, 0)
	a.waitFor(steerQueuedThenRewindSteer + " (steer · pending)")
	a.typeText(steerQueuedThenRewindQueue)
	a.key(uv.KeyEnter, uv.ModAlt)
	a.waitFor(steerQueuedThenRewindQueue + " (queued)")

	// Double esc: cancel, then the idle first press.
	a.key(uv.KeyEscape, 0)
	a.waitFor("■ cancelled")
	a.key(uv.KeyEscape, 0)
	a.waitUntil(func(s string) bool { return !cancelSpinner.MatchString(s) },
		"the spinner to stop after the cancel")
	time.Sleep(1500 * time.Millisecond) // a stray turn from a queued line would show by now
	screen := a.settled()
	a.check("after steer+queue then esc esc")
	comp := followUpComposer(a)
	inputs := steerQueuedThenRewindInputs(a)

	t.Run("TestSteerQueuedThenRewindNotLost", func(t *testing.T) {
		if strings.Contains(screen, "HELDEND") {
			t.Errorf("the held reply finished; the gate leaked:\n%s", screen)
		}
		for _, txt := range []string{steerQueuedThenRewindSteer, steerQueuedThenRewindQueue} {
			inComposer := strings.Contains(comp, txt)
			n := strings.Count(screen, txt)
			shown := (n > 0 && !inComposer) || n > 1
			if !inComposer && !shown {
				t.Errorf("%q neither restored to the composer nor shown:\n%s", txt, screen)
			}
			if strings.Contains(screen, txt+" (steer · pending)") {
				t.Errorf("%q still shows pending after the turn ended:\n%s", txt, screen)
			}
		}
	})

	t.Run("TestSteerQueuedThenRewindNoOrphanInputs", func(t *testing.T) {
		// A line back in the composer (unsent) must not also be
		// recorded as sent, and nothing is recorded twice.
		for _, txt := range []string{steerQueuedThenRewindSteer, steerQueuedThenRewindQueue} {
			n := steerQueuedThenRewindCount(inputs, txt)
			if n > 1 {
				t.Errorf("%q recorded %d times in history: %q", txt, n, inputs)
			}
			if n == 1 && strings.Contains(comp, txt) {
				t.Errorf("%q is both in history (sent) and back in the composer: inputs=%q\n%s", txt, inputs, screen)
			}
		}
	})

	t.Run("TestSteerQueuedThenRewindNextSubmitOnce", func(t *testing.T) {
		before := steerQueuedThenRewindInputs(a)
		if cancelComposerEmpty(comp) {
			a.typeText("next")
		}
		a.key(uv.KeyEnter, 0)
		if !a.waitDone(3, 60*time.Second) { // cancelled + done, then this turn's done
			t.Fatalf("next turn never finished:\n%s", a.text())
		}
		time.Sleep(1500 * time.Millisecond) // a duplicate turn would show by now
		after := steerQueuedThenRewindInputs(a)
		if added := after[len(before):]; len(added) != 1 {
			t.Errorf("next submit recorded %d inputs, want 1: %q", len(added), added)
		}
		for _, txt := range []string{steerQueuedThenRewindSteer, steerQueuedThenRewindQueue} {
			if n := steerQueuedThenRewindCount(after, txt); n > 1 {
				t.Errorf("%q in history %d times after the next submit: %q", txt, n, after)
			}
		}
		if s := a.settled(); strings.Contains(s, "end of tape") {
			t.Errorf("an extra model call ran off the tape:\n%s", s)
		}
		a.check("after the next submit")
	})
}
