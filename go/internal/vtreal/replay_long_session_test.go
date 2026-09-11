package vtreal

// Long-running session: a generated tape of many identical turns
// (assistant code block -> recorded result -> stop) replayed through
// the real binary. What this guards is drift over a session's life:
// the frame must still settle quickly at turn 200, the composer must
// still be there, and the top of the transcript must still be
// reachable by scrolling once everything has landed.
//
// 20 turns by default; the full 200-turn run is env-gated:
//
//	BOUGH_LONG_SESSION=1 go test ./internal/vtreal -run TestLongSession -timeout 30m

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

// longSessionMarker is the text the Nth turn's answer ends with; it is
// what we look for when scrolling back to the top.
func longSessionMarker(n int) string { return fmt.Sprintf("TURN-%03d-OK", n) }

// longSessionTape writes a tape of n turns and returns its path.
func longSessionTape(t *testing.T, n int) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "long-session.jsonl")
	f, err := os.Create(path)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	enc := json.NewEncoder(f)
	at := time.Date(2026, 9, 10, 10, 0, 0, 0, time.UTC)
	seq := 0
	write := func(kind string, data map[string]any) {
		seq++
		at = at.Add(time.Second)
		if err := enc.Encode(map[string]any{
			"seq": seq, "at": at.Format(time.RFC3339), "kind": kind, "data": data,
		}); err != nil {
			t.Fatal(err)
		}
	}
	write("meta", map[string]any{"cwd": "/tmp/demo"})
	for i := 1; i <= n; i++ {
		code := fmt.Sprintf("console.log(%q)\n", fmt.Sprintf("step %d", i))
		write("input", map[string]any{"text": fmt.Sprintf("do step %d", i)})
		write("assistant", map[string]any{"text": "```js\n" + code + "```"})
		write("code", map[string]any{"text": code})
		write("result", map[string]any{"code": code, "text": fmt.Sprintf("step %d\n", i)})
		write("assistant", map[string]any{"text": fmt.Sprintf("```stop\nstep %d done: %s\n```", i, longSessionMarker(i))})
		write("done", map[string]any{"text": ""})
	}
	return path
}

// longSessionRun drives n turns, checking and timing each one.
func longSessionRun(t *testing.T, n int, budget time.Duration) {
	t.Helper()
	tape := longSessionTape(t, n)
	a := startCfg(t, 100, 30, replayConfig(tape))
	a.check("boot")

	var worst time.Duration
	worstTurn := 0
	for i := 1; i <= n; i++ {
		where := fmt.Sprintf("turn %d/%d", i, n)
		a.typeText(fmt.Sprintf("do step %d", i))
		a.key(uv.KeyEnter, 0)
		if !a.waitDone(i, 60*time.Second) {
			t.Fatalf("%s: turn never finished:\n%s", where, a.text())
		}
		start := time.Now()
		screen := a.settled()
		took := time.Since(start)
		if took > worst {
			worst, worstTurn = took, i
		}
		if i == 1 || i == n || i%10 == 0 {
			t.Logf("%s: settled in %s", where, took.Round(time.Millisecond))
		}
		if took > budget {
			t.Errorf("%s: took %s to settle (budget %s):\n%s", where, took, budget, screen)
		}
		if !strings.Contains(screen, longSessionMarker(i)) {
			t.Errorf("%s: answer %s not on screen:\n%s", where, longSessionMarker(i), screen)
		}
		if composerRow(strings.Split(screen, "\n")) < 0 {
			t.Errorf("%s: composer not on screen:\n%s", where, screen)
		}
		a.check(where)
		if t.Failed() {
			return
		}
	}
	t.Logf("worst settle: turn %d, %s", worstTurn, worst.Round(time.Millisecond))

	// The top of the transcript must still be reachable: scroll up
	// until the first turn's answer is on screen.
	found := false
	for range 200 {
		if strings.Contains(a.text(), longSessionMarker(1)) {
			found = true
			break
		}
		for range 5 {
			a.term.SendMouse(uv.MouseWheelEvent{X: 5, Y: 3, Button: uv.MouseWheelUp})
		}
		time.Sleep(20 * time.Millisecond)
	}
	if !found {
		t.Errorf("scrolling up never reached turn 1 (%s):\n%s", longSessionMarker(1), a.settled())
		return
	}
	a.check("scrolled to top")
}

func TestLongSessionTwentyTurns(t *testing.T) {
	t.Parallel()
	longSessionRun(t, 20, 5*time.Second)
}

func TestLongSessionTwoHundredTurns(t *testing.T) {
	if os.Getenv("BOUGH_LONG_SESSION") == "" {
		t.Skip("set BOUGH_LONG_SESSION=1 to replay the full 200-turn session")
	}
	longSessionRun(t, 200, 5*time.Second)
}
