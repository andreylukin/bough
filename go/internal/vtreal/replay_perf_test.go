package vtreal

// Latency probes on a replayed tape: how long the TUI takes to paint
// the first streamed word, to close the turn after the last delta, to
// settle after a 400-line reply, and whether it redraws at all once
// idle. The bounds are deliberately generous — these catch a
// pathological regression (a lost wakeup, a redraw loop), not a few
// milliseconds of drift — and every probe logs its measurement so a
// run reports the real numbers.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const (
	perfFirstWord = "PERFALPHA"
	perfLastWord  = "PERFOMEGA"
	perfBudget    = 2 * time.Second
)

// perfConfig is replayConfig plus a streaming delay, so words arrive
// one at a time the way they do from a real provider.
func perfConfig(tape string, delayMS int) string {
	return strings.Replace(
		replayConfig(tape),
		fmt.Sprintf("config: {file: %q}", tape),
		fmt.Sprintf("config: {file: %q, delay_ms: %d}", tape, delayMS),
		1)
}

// perfTape writes a tape whose turns answer with the given replies.
func perfTape(t *testing.T, replies ...string) string {
	t.Helper()
	var b strings.Builder
	seq := 0
	write := func(kind, text string) {
		seq++
		line, err := json.Marshal(map[string]any{
			"seq": seq, "at": "2026-09-10T10:00:00Z", "kind": kind,
			"data": map[string]any{"text": text},
		})
		if err != nil {
			t.Fatal(err)
		}
		b.Write(line)
		b.WriteByte('\n')
	}
	for i, r := range replies {
		write("input", fmt.Sprintf("turn %d", i+1))
		write("assistant", r)
		write("done", "")
	}
	path := filepath.Join(t.TempDir(), "perf.jsonl")
	if err := os.WriteFile(path, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return path
}

// perfWaitDone is waitDone with a fine sampling interval, so the
// measurement is not quantised by the poll.
func perfWaitDone(a *app, n int, timeout time.Duration) bool {
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if a.doneCount() >= n {
			return true
		}
		time.Sleep(5 * time.Millisecond)
	}
	return false
}

// perfWaitText polls the screen tightly for substr and returns how
// long it took.
func perfWaitText(a *app, substr string, timeout time.Duration) (time.Duration, bool) {
	start := time.Now()
	for time.Since(start) < timeout {
		if strings.Contains(a.text(), substr) {
			return time.Since(start), true
		}
		time.Sleep(2 * time.Millisecond)
	}
	return time.Since(start), false
}

// perfSend types a line and presses enter, returning the instant enter
// was pressed.
func perfSend(a *app, s string) time.Time {
	a.typeText(s)
	a.key(uv.KeyEnter, 0)
	return time.Now()
}

// perfReply streams prose and then stops: the first word is the first
// paint, the last word is the last delta before the turn closes.
func perfReply() string {
	return perfFirstWord + " " + strings.Repeat("filler ", 20) + perfLastWord +
		"\n\n```stop\ndone\n```"
}

// Enter to the first streamed word on screen.
func TestPerfFirstWordLatency(t *testing.T) {
	t.Parallel()
	tape := perfTape(t, perfReply())
	a := startCfg(t, 100, 30, perfConfig(tape, 5))
	perfSend(a, "go")
	d, ok := perfWaitText(a, perfFirstWord, 20*time.Second)
	if !ok {
		t.Fatalf("first streamed word never appeared (waited %v):\n%s", d, a.text())
	}
	t.Logf("enter -> first word: %v", d)
	if d > perfBudget {
		t.Errorf("first word took %v after enter (> %v):\n%s", d, perfBudget, a.text())
	}
}

// Last streamed delta on screen to the turn's "done" entry.
func TestPerfDeltaToDoneLatency(t *testing.T) {
	t.Parallel()
	tape := perfTape(t, perfReply())
	a := startCfg(t, 100, 30, perfConfig(tape, 5))
	perfSend(a, "go")
	if _, ok := perfWaitText(a, perfLastWord, 20*time.Second); !ok {
		t.Fatalf("last streamed word never appeared:\n%s", a.text())
	}
	last := time.Now()
	if !perfWaitDone(a, 1, 20*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	d := time.Since(last)
	t.Logf("last delta -> done: %v", d)
	if d > perfBudget {
		t.Errorf("turn took %v to close after the last delta (> %v):\n%s", d, perfBudget, a.text())
	}
}

// A 400-line reply must land and stop moving quickly, with the screen
// still holding every invariant.
func TestPerfLongReplySettle(t *testing.T) {
	t.Parallel()
	var sb strings.Builder
	sb.WriteString(perfFirstWord + "\n")
	for i := range 400 {
		fmt.Fprintf(&sb, "line %03d of the long reply\n", i)
	}
	sb.WriteString(perfLastWord + "\n\n```stop\ndone\n```")
	tape := perfTape(t, sb.String())
	a := startCfg(t, 100, 30, perfConfig(tape, 0))
	start := perfSend(a, "go")
	if !perfWaitDone(a, 1, 30*time.Second) {
		t.Fatalf("400-line turn never finished:\n%s", a.text())
	}
	// Settled: two samples 60ms apart that match.
	prev := a.text()
	var settle time.Duration
	for {
		if time.Since(start) > 30*time.Second {
			t.Fatalf("screen never settled after a 400-line reply:\n%s", a.text())
		}
		time.Sleep(60 * time.Millisecond)
		cur := a.text()
		if cur == prev {
			settle = time.Since(start)
			break
		}
		prev = cur
	}
	t.Logf("enter -> settled after a 400-line reply: %v", settle)
	if settle > 5*time.Second {
		t.Errorf("400-line reply took %v to settle (> 5s):\n%s", settle, a.text())
	}
	a.check("400-line reply")
}

// Idle costs nothing: once the turn has settled the screen must not
// change again for two seconds. A redraw loop (a ticking chip, a
// re-render every frame) shows up here as a differing sample.
func TestPerfIdleNoRedrawChurn(t *testing.T) {
	t.Parallel()
	tape := perfTape(t, perfReply())
	a := startCfg(t, 100, 30, perfConfig(tape, 5))
	perfSend(a, "go")
	if !perfWaitDone(a, 1, 20*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	base := a.settled()
	for i := range 20 {
		time.Sleep(100 * time.Millisecond)
		if cur := a.text(); cur != base {
			t.Fatalf("screen redrew while idle (sample %d, %v after settle):\nbefore:\n%s\nafter:\n%s",
				i+1, time.Duration(i+1)*100*time.Millisecond, base, cur)
		}
	}
	t.Logf("idle: 20 samples over 2s identical")
}
