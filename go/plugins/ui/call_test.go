package ui

import (
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/charmbracelet/x/exp/golden"
	"github.com/charmbracelet/x/exp/teatest/v2"
)

// Engine call rows (call.go): the provider call id is a string, which is
// what tells an engine call from a loop block's per-call detail.

func (d *drv) ev(kind, text string, data map[string]any) {
	d.feed(eventMsg{Kind: kind, Text: text, Data: data})
}

func startCall(d *drv, id, tool, detail string) {
	d.ev("call", detail, map[string]any{"id": id, "tool": tool, "phase": "start"})
}

// A running call shows a spinner row and the last lines of its output.
func TestGoldenEngineCallRunning(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("user", "run the tests")
	startCall(d, "toolu_1", "bash", "go test ./...")
	d.ev("call-delta", "=== RUN a\n=== RUN b\n", map[string]any{"id": "toolu_1"})
	d.ev("call-delta", "--- PASS: a\n--- PASS: b\nok  pkg 0.1s\n", map[string]any{"id": "toolu_1"})
	plain := d.plain()
	for _, want := range []string{"Running go test ./...", "--- PASS: a", "--- PASS: b", "ok  pkg 0.1s"} {
		if !strings.Contains(plain, want) {
			t.Fatalf("frame lacks %q:\n%s", want, plain)
		}
	}
	if strings.Contains(plain, "=== RUN b") {
		t.Fatalf("the tail keeps the last %d lines only:\n%s", callTail, plain)
	}
	golden.RequireEqual(t, []byte(d.view()))
}

// Each finished call is one line: what it did, how long, how it ended;
// red on a failure, (bg) when it outlived its turn as a job.
func TestGoldenEngineCallRows(t *testing.T) {
	t.Parallel()
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, nil))
	d.m.running = true // mid-turn: nothing folds yet
	d.event("user", "fix it")
	startCall(d, "c1", "bash", "go test ./...")
	startCall(d, "c2", "patch", "go/a.go")
	startCall(d, "c3", "bash", "make build")
	startCall(d, "c4", "bash", "sleep 100")
	d.ev("call", "go/a.go", map[string]any{"id": "c2", "tool": "patch", "ms": 40.0, "add": 3.0, "del": 1.0, "output": "patched go/a.go"})
	d.ev("call", "go test ./...", map[string]any{"id": "c1", "tool": "bash", "ms": 2300.0, "exit": 1.0, "output": "FAIL pkg\n"})
	d.ev("call", "make build", map[string]any{"id": "c3", "tool": "bash", "ms": 61000.0, "exit": 0.0, "adopted": true, "job": 2.0, "late": true})
	d.ev("call", "sleep 100", map[string]any{"id": "c4", "tool": "bash", "ms": 900.0, "error": "cancelled by the user", "canceled": true})
	plain := d.plain()
	for _, want := range []string{
		"▸ ✔ Patched go/a.go · <1s · +3 −1",
		"▸ ✗ Ran go test ./... · 2s · exit 1",
		"✔ Ran make build · 1m01s (bg)",
		"✔ Ran sleep 100 · <1s · cancelled",
	} {
		if !strings.Contains(plain, want) {
			t.Fatalf("frame lacks %q:\n%s", want, plain)
		}
	}
	n := 0
	for _, b := range d.m.blocks {
		if b.kind == "call" {
			n++
			if b.live {
				t.Errorf("call %s still live after its end", b.call.id)
			}
		}
	}
	if n != 4 {
		t.Fatalf("%d call rows, want 4 (one per call, replaced in place)", n)
	}
	golden.RequireEqual(t, []byte(d.view()))
}

// A loop block's per-call events (numeric ids) stay detail of the block.
func TestLoopCallEventsStayIgnored(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.ev("call", "ls", map[string]any{"id": 1, "tool": "bash", "phase": "start"})
	d.ev("call", "ls", map[string]any{"id": 1.0, "tool": "bash", "ms": 3.0, "exit": 0.0})
	d.ev("sub:call", "ls", map[string]any{"id": 1, "tool": "bash", "worker": 1})
	for _, b := range d.m.blocks {
		if b.kind == "call" || b.kind == "spawn" {
			t.Fatalf("a loop call made a %s block", b.kind)
		}
	}
}

// Opened, a finished call shows what it printed.
func TestEngineCallRowOpens(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	startCall(d, "c1", "view", "README.md")
	d.ev("call", "README.md", map[string]any{"id": "c1", "tool": "view", "ms": 2.0, "output": "# bough\nline two\n"})
	b := &d.m.blocks[len(d.m.blocks)-1]
	if !b.collapsible() || !b.collapsed {
		t.Fatalf("finished call with output: collapsible=%v collapsed=%v", b.collapsible(), b.collapsed)
	}
	b.collapsed = false
	d.m.refresh()
	if p := d.plain(); !strings.Contains(p, "▾ ✔ Read README.md") || !strings.Contains(p, "line two") {
		t.Fatalf("opened row:\n%s", p)
	}
}

// A delta-reset drops the partial reply and reasoning: the retried
// request streams from the start, and the text must not read twice.
func TestDeltaResetDropsStream(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("user", "hi")
	d.event("thinking-delta", "let me")
	d.event("assistant-delta", "Hel")
	d.ev("delta-reset", "", map[string]any{"seq": 1.0})
	for _, b := range d.m.blocks {
		if b.live {
			t.Fatalf("live %s block survived the reset: %q", b.kind, b.text)
		}
	}
	d.event("assistant-delta", "Hello")
	d.event("assistant", "Hello there")
	d.event("done", "")
	if p := d.plain(); strings.Contains(p, "HelHello") || strings.Count(p, "Hello there") != 1 {
		t.Fatalf("frame:\n%s", p)
	}
}

// A finished engine turn folds its call rows like a loop turn's steps.
func TestEngineCallsFold(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("user", "look")
	for i, c := range [][2]string{{"view", "a.go"}, {"bash", "ls"}, {"view", "b.go"}} {
		id := string(rune('a' + i))
		startCall(d, id, c[0], c[1])
		d.ev("call", c[1], map[string]any{"id": id, "tool": c[0], "ms": 5.0})
	}
	d.event("assistant", "Looked.")
	d.event("done", "")
	if p := d.plain(); !strings.Contains(p, "▸ 3 steps · read 2 files, ran 1 command") {
		t.Fatalf("no fold row:\n%s", p)
	}
}

// A child's native calls are its steps on the card: counted at their
// end, named while they run.
func TestEngineSubCallOnCard(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.ev("sub:start", "count lines", map[string]any{"worker": 1})
	d.ev("sub:call", "wc -l *.go", map[string]any{"id": "c1", "tool": "bash", "phase": "start", "worker": 1})
	card := d.m.spawnCard(1)
	if card.sub.calls != 0 || card.sub.last != "Running wc -l *.go" {
		t.Fatalf("running: calls=%d last=%q", card.sub.calls, card.sub.last)
	}
	d.ev("sub:call", "wc -l *.go", map[string]any{"id": "c1", "tool": "bash", "ms": 10.0, "exit": 0.0, "output": "42 total\n", "worker": 1})
	card = d.m.spawnCard(1)
	if card.sub.calls != 1 || card.sub.last != "Ran wc -l *.go" || card.sub.lastOut != "42 total" {
		t.Fatalf("done: calls=%d last=%q out=%q", card.sub.calls, card.sub.last, card.sub.lastOut)
	}
	if len(card.sub.log) != 1 || card.sub.log[0].Kind != "call" {
		t.Fatalf("log = %+v, want the one recorded call", card.sub.log)
	}
	if tr := stripANSI(d.m.subTranscript(card, d.m.cfg.Load())); !strings.Contains(tr, "✔ Ran wc -l *.go") {
		t.Fatalf("overlay:\n%s", tr)
	}
}

// Resuming an engine session: the engine record is bookkeeping, call
// rows render from their entries, and a wake turn is a dim line, not a
// prompt nobody typed.
func TestReplayEngineSession(t *testing.T) {
	t.Parallel()
	m := testModel(t)
	cfg := m.cfg.Load()
	at := time.Date(2026, 9, 22, 12, 0, 0, 0, time.UTC)
	h := fakeHist{path: "/tmp/e.jsonl"}
	for i, e := range []history.Entry{
		{Kind: "engine", Data: map[string]any{"engine": "unreal", "model": "m"}},
		{Kind: "input", Data: map[string]any{"text": "build it", "input_id": "in-1"}},
		{Kind: "call", Data: map[string]any{"text": "make", "id": "c1", "tool": "bash", "ms": 20.0, "exit": 0.0, "adopted": true, "job": 1.0}},
		{Kind: "done", Data: map[string]any{"running": 1.0}},
		{Kind: "input", Data: map[string]any{"text": "call c1 finished", "wake": true, "reason": "call", "calls": []any{"c1"}}},
		{Kind: "assistant", Data: map[string]any{"text": "Built.", "model": "m"}},
		{Kind: "done", Data: map[string]any{"wake": true}},
	} {
		e.Seq, e.At = int64(i+1), at
		h.entries = append(h.entries, e)
	}
	cfg.hist = h
	m.cfg.Store(cfg)
	m.replay()
	var kinds []string
	for _, b := range m.blocks {
		kinds = append(kinds, b.kind)
		if b.kind == "engine" {
			t.Fatal("the engine entry rendered as a block")
		}
		if b.kind == "user" && b.text == "call c1 finished" {
			t.Fatal("a wake turn rendered as a typed prompt")
		}
	}
	p := stripANSI(m.View().Content)
	for _, want := range []string{"❯ build it", "✔ Ran make · <1s (bg)", "↻ background call finished"} {
		if !strings.Contains(p, want) {
			t.Fatalf("replay lacks %q (kinds %v):\n%s", want, kinds, p)
		}
	}
}

// The real program: a call row opens running and closes in place.
func TestProgramEngineCallRow(t *testing.T) {
	t.Parallel()
	events := make(chan Event, 16)
	tm := startProgram(t, events, func(string) {})
	events <- Event{Kind: "call", Text: "go vet ./...", Data: map[string]any{"id": "c1", "tool": "bash", "phase": "start"}}
	events <- Event{Kind: "call-delta", Text: "checking pkg\n", Data: map[string]any{"id": "c1"}}
	waitForOutput(t, tm, "Running go vet ./...", "checking pkg")
	events <- Event{Kind: "call", Text: "go vet ./...", Data: map[string]any{"id": "c1", "tool": "bash", "ms": 1500.0, "exit": 0.0}}
	events <- Event{Kind: "done"}
	waitForOutput(t, tm, "✔ Ran go vet ./... · 2s")
	tm.Send(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	tm.Send(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	tm.WaitFinished(t, teatest.WithFinalTimeout(waitBudget))
}
