package ui

// Enter vs alt+enter while a turn runs: with the loop's "steer"
// service mounted, enter steers the running turn and alt+enter queues
// a follow-up; without it enter queues, as it always did.

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/andreylukin/bough/plugins/history"
)

func keyAltEnter() tea.KeyPressMsg { return tea.KeyPressMsg{Code: tea.KeyEnter, Mod: tea.ModAlt} }

// steerDrv is a running-turn driver whose steer service records what
// it was handed and answers ok.
func steerDrv(t *testing.T, ok bool) (*drv, *[]string) {
	t.Helper()
	var steered []string
	cfg := cfgWith(t, nil, nil, nil)
	cfg.steer = func(text string) bool {
		steered = append(steered, text)
		return ok
	}
	d := newDrv(t, 80, 24, cfg)
	d.typeStr("first")
	d.press(keyEnter())
	if !d.m.running || len(d.sent) != 1 {
		t.Fatalf("running=%v sent=%v after the first prompt", d.m.running, d.sent)
	}
	return d, &steered
}

func TestEnterSteersRunningTurn(t *testing.T) {
	t.Parallel()
	d, steered := steerDrv(t, true)
	d.typeStr("use B instead")
	d.press(keyEnter())
	if got := *steered; len(got) != 1 || got[0] != "use B instead" {
		t.Fatalf("steered = %v, want [use B instead]", got)
	}
	if len(d.sent) != 1 {
		t.Fatalf("a steer must not also go to inputs: sent = %v", d.sent)
	}
	if d.m.input.Value() != "" {
		t.Fatalf("composer not cleared: %q", d.m.input.Value())
	}
	if f := d.plain(); !strings.Contains(f, "❯ use B instead (steer · pending)") {
		t.Fatalf("pending steer row missing:\n%s", f)
	}
	// The loop lands it at its next boundary and says so.
	d.event("steer", "use B instead")
	f := d.plain()
	if !strings.Contains(f, "❯ use B instead (steer)") || strings.Contains(f, "pending") {
		t.Fatalf("landed steer should drop the pending marker:\n%s", f)
	}
	if n := strings.Count(f, "use B instead"); n != 1 {
		t.Fatalf("steer rendered %d times, want once:\n%s", n, f)
	}
	if !d.m.running {
		t.Fatal("a steer does not end the turn")
	}
}

func TestAltEnterQueuesFollowUpWhileRunning(t *testing.T) {
	t.Parallel()
	d, steered := steerDrv(t, true)
	d.typeStr("and then this")
	d.press(keyAltEnter())
	if len(*steered) != 0 {
		t.Fatalf("alt+enter must not steer: %v", *steered)
	}
	if len(d.sent) != 2 || d.sent[1] != "and then this" {
		t.Fatalf("sent = %v, want the follow-up queued to inputs", d.sent)
	}
	if f := d.plain(); !strings.Contains(f, "❯ and then this (queued)") {
		t.Fatalf("queued row missing:\n%s", f)
	}
	if d.m.input.Value() != "" {
		t.Fatalf("composer not cleared: %q", d.m.input.Value())
	}
}

// Idle, alt+enter is just a send.
func TestAltEnterSendsWhenIdle(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("hello")
	d.press(keyAltEnter())
	if len(d.sent) != 1 || d.sent[0] != "hello" || !d.m.running {
		t.Fatalf("sent = %v running = %v", d.sent, d.m.running)
	}
}

func TestEnterQueuesWithoutSteerService(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("first")
	d.press(keyEnter())
	d.typeStr("second")
	d.press(keyEnter())
	if len(d.sent) != 2 || d.sent[1] != "second" {
		t.Fatalf("sent = %v, want both lines to inputs", d.sent)
	}
	if f := d.plain(); !strings.Contains(f, "❯ second (queued)") {
		t.Fatalf("queued row missing:\n%s", f)
	}
}

// The loop says no turn is running (it ended before the key landed):
// the line is sent as ordinary input, never dropped.
func TestSteerRefusedFallsBackToSend(t *testing.T) {
	t.Parallel()
	d, steered := steerDrv(t, false)
	d.typeStr("late")
	d.press(keyEnter())
	if len(*steered) != 1 {
		t.Fatalf("steer not attempted: %v", *steered)
	}
	if len(d.sent) != 2 || d.sent[1] != "late" {
		t.Fatalf("sent = %v, want the refused steer sent as input", d.sent)
	}
	if f := d.plain(); strings.Contains(f, "steer") {
		t.Fatalf("a refused steer renders as a plain queued line:\n%s", f)
	}
}

// A steer the loop had no boundary left for runs as the next turn:
// done with a pending steer keeps the spinner on until that turn's
// own done.
func TestSteerDuringFinalReplyLandsBeforeDone(t *testing.T) {
	t.Parallel()
	d, _ := steerDrv(t, true)
	d.typeStr("more")
	d.press(keyEnter())
	d.event("assistant", "final words")
	d.event("steer", "more") // the loop lands it before its done and asks again
	if !d.m.running {
		t.Fatal("a landed steer does not end the turn")
	}
	d.event("assistant", "did more")
	d.event("done", "")
	if d.m.running {
		t.Fatal("the one done ends the turn")
	}
	f := d.plain()
	if !strings.Contains(f, "❯ more (steer)") || strings.Contains(f, "pending") {
		t.Fatalf("landed steer row:\n%s", f)
	}
	iS, iR := strings.Index(f, "❯ more"), strings.Index(f, "did more")
	if iS < 0 || iR < iS {
		t.Fatalf("the second reply should follow the steer row:\n%s", f)
	}
}

// Esc with a steer pending: the loop records the steer, then
// cancels — the row stops pending and the spinner stops with the
// done; nothing runs on its own.
func TestCancelWithPendingSteerStops(t *testing.T) {
	t.Parallel()
	d, _ := steerDrv(t, true)
	d.typeStr("stop doing X")
	d.press(keyEnter())
	d.event("steer", "stop doing X")
	d.event("cancelled", "")
	d.event("done", "")
	if d.m.running {
		t.Fatal("still running after the cancelled turn's done")
	}
	if f := d.plain(); !strings.Contains(f, "❯ stop doing X (steer)") || strings.Contains(f, "pending") {
		t.Fatalf("recorded steer row:\n%s", f)
	}
}

// alt+enter with the slash palette open accepts the selection like
// enter — it never submits a half-typed "/mo" past the list.
func TestAltEnterAcceptsPaletteSelection(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha"))
	d.typeStr("/al")
	if !d.m.pal.open {
		t.Fatal("palette did not open on /al")
	}
	d.press(keyAltEnter())
	if len(d.sent) != 0 {
		t.Fatalf("alt+enter on the palette must dispatch, not send: sent=%v", d.sent)
	}
	if f := d.plain(); !strings.Contains(f, "alpha ran") || d.m.pal.open {
		t.Fatalf("expected /alpha to run and the palette to close:\n%s", f)
	}
}

// A resumed session shows which inputs were steers.
func TestReplayMarksSteerInputs(t *testing.T) {
	t.Parallel()
	h := histWith("/tmp/s.jsonl", "asked", "steered")
	for i := range h.entries {
		h.entries[i].Kind = "input"
	}
	h.entries[1].Data["steer"] = true
	h.entries = append(h.entries, history.Entry{Seq: 3, Kind: "done", Data: map[string]any{"text": ""}})
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, h))
	f := d.plain()
	if !strings.Contains(f, "❯ asked") || strings.Contains(f, "❯ asked (") || !strings.Contains(f, "❯ steered (steer)") {
		t.Fatalf("replay rows:\n%s", f)
	}
}

func TestKeysHelpNamesFollowUp(t *testing.T) {
	t.Parallel()
	txt := keysText(cfgWith(t, nil, nil, nil))
	if !strings.Contains(txt, "alt+enter") || !strings.Contains(txt, "steer") {
		t.Fatalf("keys help should name alt+enter and steering:\n%s", txt)
	}
}
