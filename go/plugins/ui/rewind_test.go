package ui

// The rewind menu: a list you walk with the arrows and pick from, the
// way Claude Code's Esc+Esc opens one. It used to print the turns into
// the transcript, which you could read but not use.

import (
	"strings"
	"testing"

	"github.com/andreylukin/bough/plugins/history"
)

// It opens on "(current)": the present, walking back from there.
func TestRewindOpensOnCurrent(t *testing.T) {
	t.Parallel()
	d := rewindDrv(t, "one", "two", "three")
	d.press(keyEsc())
	d.press(keyEsc())
	if got, want := d.m.rw.pick, len(d.m.rw.rows); got != want {
		t.Fatalf("cursor at %d, want %d ((current))", got, want)
	}
	if !strings.Contains(d.plain(), "❯ (current)") {
		t.Errorf("(current) should be the selected row:\n%s", d.plain())
	}
}

func TestRewindArrowsWalkTheTurns(t *testing.T) {
	t.Parallel()
	d := rewindDrv(t, "one", "two", "three")
	d.press(keyEsc())
	d.press(keyEsc())

	d.press(keyUp())
	if !strings.Contains(d.plain(), "❯ three") {
		t.Errorf("up from (current) selects the newest turn:\n%s", d.plain())
	}
	d.press(keyUp())
	d.press(keyUp())
	if !strings.Contains(d.plain(), "❯ one") {
		t.Errorf("three ups reach the oldest turn:\n%s", d.plain())
	}
	d.press(keyUp()) // at the top: stays
	if !strings.Contains(d.plain(), "❯ one") {
		t.Errorf("up at the top should not wrap:\n%s", d.plain())
	}
	d.press(keyDown())
	if !strings.Contains(d.plain(), "❯ two") {
		t.Errorf("down walks forward again:\n%s", d.plain())
	}
}

// Picking a row goes back to BEFORE that prompt, so undoing "two"
// means the session ends at "one". Fork keeps the turn it is given, so
// that is a fork at "one" (seq 1), not at "two".
func TestRewindEnterGoesBackToBeforeThePrompt(t *testing.T) {
	t.Parallel()
	d := rewindDrv(t, "one", "two")
	d.press(keyEsc())
	d.press(keyEsc())
	d.press(keyUp()) // "two", whose own input entry is seq 3
	d.press(keyEnter())
	if d.m.rw.open {
		t.Error("enter should close the menu")
	}
	p := d.plain()
	if !strings.Contains(p, "/tree 1") {
		t.Errorf("before \"two\" is a fork at \"one\" (seq 1):\n%s", p)
	}
	if strings.Contains(p, "/tree 3") {
		t.Errorf("forking AT the picked turn would keep it:\n%s", p)
	}
}

// Before the first prompt there is no earlier turn: that point is a
// session with nothing in it.
func TestRewindToBeforeTheFirstPromptStartsFresh(t *testing.T) {
	t.Parallel()
	d := rewindDrv(t, "one", "two")
	d.press(keyEsc())
	d.press(keyEsc())
	d.press(keyUp())
	d.press(keyUp()) // "one", the oldest
	d.press(keyEnter())
	if p := d.plain(); !strings.Contains(p, "/new") {
		t.Errorf("before the first prompt is a fresh session:\n%s", p)
	}
}

// Enter on "(current)" changes nothing: it is the way out that is not
// a decision.
func TestRewindEnterOnCurrentDoesNothing(t *testing.T) {
	t.Parallel()
	d := rewindDrv(t, "one")
	d.press(keyEsc())
	d.press(keyEsc())
	d.press(keyEnter())
	if d.m.rw.open {
		t.Error("the menu should close")
	}
	if p := d.plain(); strings.Contains(p, "/tree") {
		t.Errorf("(current) should not fork anything:\n%s", p)
	}
}

func TestRewindEscCancels(t *testing.T) {
	t.Parallel()
	d := rewindDrv(t, "one")
	d.press(keyEsc())
	d.press(keyEsc())
	d.press(keyEsc())
	if d.m.rw.open {
		t.Error("esc should close the menu")
	}
	if p := d.plain(); strings.Contains(p, "/tree") {
		t.Errorf("cancelling must not fork:\n%s", p)
	}
}

// Each row says what its turn wrote, so you can see which points have
// code behind them — bough forks the CONVERSATION, and putting files
// back is /undo.
func TestRewindRowsShowWhatEachTurnWrote(t *testing.T) {
	t.Parallel()
	h := fakeHist{path: "/tmp/s.jsonl", entries: []history.Entry{
		{Seq: 1, Kind: "input", Data: map[string]any{"text": "wrote nothing"}},
		{Seq: 2, Kind: "done", Data: map[string]any{"files": []string{}}},
		{Seq: 3, Kind: "input", Data: map[string]any{"text": "wrote one"}},
		{Seq: 4, Kind: "done", Data: map[string]any{"files": []string{"only.go"}}},
		{Seq: 5, Kind: "input", Data: map[string]any{"text": "wrote several"}},
		{Seq: 6, Kind: "done", Data: map[string]any{"files": []string{"a.go", "b.go", "c.go"}}},
	}}
	cfg := cfgWith(t, nil, nil, h)
	cfg.cmds = reg(t, "tree", "new")
	d := newDrv(t, 100, 30, cfg)
	d.press(keyEsc())
	d.press(keyEsc())
	p := d.plain()
	for _, want := range []string{"no files written", "wrote only.go", "wrote 3 files"} {
		if !strings.Contains(p, want) {
			t.Errorf("the menu should say %q:\n%s", want, p)
		}
	}
}

// A background job's wake-up is recorded as an input, but nobody typed
// it, so it is not a point you rewind to.
func TestRewindSkipsBackgroundWakeups(t *testing.T) {
	t.Parallel()
	rows := rewindTurns([]history.Entry{
		{Seq: 1, Kind: "input", Data: map[string]any{"text": "a real prompt"}},
		{Seq: 2, Kind: "input", Data: map[string]any{"text": "[background job] A command you started has finished"}},
	})
	if len(rows) != 1 || rows[0].text != "a real prompt" {
		t.Fatalf("only typed prompts are rewind points, got %+v", rows)
	}
}

// The prompt shown is what was TYPED: a skill's body is appended to
// the message that was sent, and it is not a menu row.
func TestRewindShowsTheTypedPrompt(t *testing.T) {
	t.Parallel()
	rows := rewindTurns([]history.Entry{
		{Seq: 1, Kind: "input", Data: map[string]any{
			"text":  "please frobnicate\n\n[skill: frobnicate]\nlots of body",
			"typed": "please frobnicate",
		}},
	})
	if len(rows) != 1 || rows[0].text != "please frobnicate" {
		t.Fatalf("row should be the typed line, got %+v", rows)
	}
}
