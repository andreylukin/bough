package ui

// Session-resume tests: transcript replay from seeded history, and the
// pre-chat session picker (list render, arrow navigation, enter
// resumes, esc falls through to a fresh session, absent seam = no
// picker).

import (
	"strings"
	"sync/atomic"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/exp/teatest/v2"

	"github.com/andreylukin/bough/plugins/history"
)

// seededHist is a resumed session's history: one full prior turn.
func seededHist() fakeHist {
	at := time.Date(2026, 8, 31, 10, 0, 0, 0, time.UTC)
	mk := func(seq int64, kind, text string) history.Entry {
		return history.Entry{Seq: seq, At: at, Kind: kind, Data: map[string]any{"text": text}}
	}
	return fakeHist{path: "/tmp/resumed.jsonl", entries: []history.Entry{
		mk(1, "input", "prior question"),
		mk(2, "assistant", "prior answer"),
		mk(3, "code", `tools.bash("ls")`),
		mk(4, "result", "a.txt\nb.txt"),
		mk(5, "done", ""),
	}}
}

func sessionRows() []history.SessionInfo {
	return []history.SessionInfo{
		{ID: "2026-09-01T10:00:00Z-42", ModTime: time.Now().Add(-time.Hour),
			Entries: 12, Title: "newest: fix the tests"},
		{ID: "2026-08-31T08:00:00Z-41", ModTime: time.Now().Add(-25 * time.Hour),
			Entries: 4, Title: "older: refactor loop"},
	}
}

// pickerCfg builds a picker-seam cfg whose choose records the chosen
// id and swaps the cfg pointer to a resumed-session cfg (what the
// launcher's remount does), so replay-after-choose is exercised.
func pickerCfg(t *testing.T, p *atomic.Pointer[uiCfg], chosen chan string) *uiCfg {
	t.Helper()
	cfg := cfgWith(t, nil, nil, nil)
	cfg.picker = true
	cfg.sessions = sessionRows()
	cfg.choose = func(id string) {
		chosen <- id
		if id != "" {
			p.Store(cfgWith(t, nil, nil, seededHist()))
		}
	}
	return cfg
}

// --- replay (teatest, full program) ---

func TestProgramReplaysResumedTranscript(t *testing.T) {
	t.Parallel()
	events := make(chan Event, 16)
	t.Cleanup(func() { close(events) })
	var p atomic.Pointer[uiCfg]
	p.Store(cfgWith(t, nil, nil, seededHist()))
	tm := teatest.NewTestModel(t, newModel(80, 24, func(string) {}, events, &p),
		teatest.WithInitialTermSize(80, 24))

	// The prior turn renders exactly as live: ❯ user line, assistant
	// text, and collapsed code/result headers (collapse: all default).
	waitForOutput(t, tm,
		"❯ prior question",
		"prior answer",
		"▸ Ran: ls (1 line)",
		"▸ result (2 lines)",
	)
	sendQuit(tm)
	tm.WaitFinished(t, teatest.WithFinalTimeout(4*time.Second))
	fm := tm.FinalModel(t).(model)
	if len(fm.blocks) != 6 {
		t.Errorf("replayed %d blocks, want 6 (user/assistant/code/result/done + resumed row)", len(fm.blocks))
	}
	if fm.running {
		t.Error("a replayed transcript must not be mid-turn")
	}
	if !fm.vp.AtBottom() {
		t.Error("viewport should start pinned at the bottom after replay")
	}
}

func TestProgramFreshSessionNoReplay(t *testing.T) {
	t.Parallel()
	events := make(chan Event, 16)
	t.Cleanup(func() { close(events) })
	var p atomic.Pointer[uiCfg]
	p.Store(cfgWith(t, nil, nil, fakeHist{path: "/tmp/fresh.jsonl"})) // 0 entries
	tm := teatest.NewTestModel(t, newModel(80, 24, func(string) {}, events, &p),
		teatest.WithInitialTermSize(80, 24))
	waitForOutput(t, tm, "bough") // first frame drawn
	sendQuit(tm)
	tm.WaitFinished(t, teatest.WithFinalTimeout(4*time.Second))
	if fm := tm.FinalModel(t).(model); len(fm.blocks) != 0 {
		t.Errorf("fresh session replayed %d blocks, want 0", len(fm.blocks))
	}
}

// --- picker (teatest, full program) ---

func TestProgramPickerResumeSelected(t *testing.T) {
	t.Parallel()
	events := make(chan Event, 16)
	t.Cleanup(func() { close(events) })
	chosen := make(chan string, 1)
	var p atomic.Pointer[uiCfg]
	p.Store(pickerCfg(t, &p, chosen))
	tm := teatest.NewTestModel(t, newModel(80, 24, func(string) {}, events, &p),
		teatest.WithInitialTermSize(80, 24))

	// The list renders before the chat view, newest first with counts
	// and titles.
	waitForOutput(t, tm,
		"resume a session",
		"12 entries  newest: fix the tests",
		"4 entries  older: refactor loop",
		"enter resume · esc new session",
	)
	// Arrow down + enter resumes the SECOND (older) session…
	tm.Send(tea.KeyPressMsg{Code: tea.KeyDown})
	tm.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
	select {
	case id := <-chosen:
		if id != "2026-08-31T08:00:00Z-41" {
			t.Errorf("chose %q, want the older session id", id)
		}
	case <-time.After(4 * time.Second):
		t.Fatal("session-choose was never invoked")
	}
	// …and the chat view replays the resumed transcript.
	waitForOutput(t, tm, "❯ prior question", "▸ result (2 lines)")
	sendQuit(tm)
	tm.WaitFinished(t, teatest.WithFinalTimeout(4*time.Second))
	if fm := tm.FinalModel(t).(model); fm.picking {
		t.Error("still picking after enter")
	}
}

func TestProgramPickerEscStartsFresh(t *testing.T) {
	t.Parallel()
	events := make(chan Event, 16)
	t.Cleanup(func() { close(events) })
	chosen := make(chan string, 1)
	var p atomic.Pointer[uiCfg]
	p.Store(pickerCfg(t, &p, chosen))
	tm := teatest.NewTestModel(t, newModel(80, 24, func(string) {}, events, &p),
		teatest.WithInitialTermSize(80, 24))

	waitForOutput(t, tm, "resume a session")
	tm.Send(tea.KeyPressMsg{Code: tea.KeyEscape})
	select {
	case id := <-chosen:
		if id != "" {
			t.Errorf("esc chose %q, want \"\" (fresh session)", id)
		}
	case <-time.After(4 * time.Second):
		t.Fatal("session-choose was never invoked on esc")
	}
	sendQuit(tm)
	tm.WaitFinished(t, teatest.WithFinalTimeout(4*time.Second))
	fm := tm.FinalModel(t).(model)
	if fm.picking {
		t.Error("still picking after esc")
	}
	if len(fm.blocks) != 0 {
		t.Errorf("fresh session after esc replayed %d blocks, want 0", len(fm.blocks))
	}
}

// --- picker details (direct driver: deterministic full frames) ---

// markerOn reports whether the picker row containing title carries the
// selection marker.
func markerOn(plain, title string) bool {
	for _, l := range strings.Split(plain, "\n") {
		if strings.Contains(l, title) {
			return strings.Contains(l, "▸")
		}
	}
	return false
}

func TestPickerArrowNavigationMovesMarker(t *testing.T) {
	t.Parallel()
	cfg := cfgWith(t, nil, nil, nil)
	cfg.picker = true
	cfg.sessions = sessionRows()
	cfg.choose = func(string) {}
	d := newDrv(t, 80, 24, cfg)
	if !d.m.picking {
		t.Fatal("picker seam present but model not picking")
	}
	if p := d.plain(); !markerOn(p, "newest: fix the tests") || markerOn(p, "older: refactor loop") {
		t.Fatalf("marker should start on the newest session:\n%s", p)
	}
	d.press(keyDown())
	if p := d.plain(); !markerOn(p, "older: refactor loop") {
		t.Fatalf("down should move the marker to the older session:\n%s", p)
	}
	d.press(keyDown()) // clamped at the last row
	if p := d.plain(); !markerOn(p, "older: refactor loop") {
		t.Fatalf("down past the end should stay on the last session:\n%s", p)
	}
	d.press(keyUp())
	if p := d.plain(); !markerOn(p, "newest: fix the tests") {
		t.Fatalf("up should move the marker back:\n%s", p)
	}
}

func TestPickerAbsentWithoutSeam(t *testing.T) {
	t.Parallel()
	cfg := cfgWith(t, nil, nil, nil)
	cfg.sessions = sessionRows() // sessions listed but no picker marker
	cfg.choose = func(string) {}
	d := newDrv(t, 80, 24, cfg)
	if d.m.picking {
		t.Fatal("no session-picker marker: model must start in the chat view")
	}
	if p := d.plain(); strings.Contains(p, "resume a session") {
		t.Fatalf("picker rendered without the seam:\n%s", p)
	}
}

func TestPickerQuitBindingWorks(t *testing.T) {
	t.Parallel()
	cfg := cfgWith(t, nil, nil, nil)
	cfg.picker = true
	cfg.sessions = sessionRows()
	cfg.choose = func(string) {}
	d := newDrv(t, 80, 24, cfg)
	if !hasQuit(d.press(keyCtrl('c'))) {
		t.Error("quit binding should work inside the picker")
	}
}

// --- replay details (direct driver) ---

func TestReplayIsIdempotent(t *testing.T) {
	t.Parallel()
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, seededHist()))
	n := len(d.m.blocks)
	if n != 6 { // 5 entries + the resumed row
		t.Fatalf("replayed %d blocks, want 6", n)
	}
	d.m.replay() // a second replay must not double-render
	if got := len(d.m.blocks); got != n {
		t.Errorf("second replay grew blocks %d -> %d", n, got)
	}
}

func TestReplayThenLiveTurnAppends(t *testing.T) {
	t.Parallel()
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, seededHist()))
	d.typeStr("follow-up")
	d.press(keyEnter())
	d.event("assistant", "resumed reply")
	d.event("done", "")
	p := d.plain()
	for _, want := range []string{"❯ prior question", "❯ follow-up", "resumed reply"} {
		if !strings.Contains(p, want) {
			t.Errorf("frame missing %q after live turn on resumed session:\n%s", want, p)
		}
	}
	if d.sent[0] != "follow-up" {
		t.Errorf("live send on resumed session = %q, want follow-up", d.sent[0])
	}
}
