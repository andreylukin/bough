package vtreal

// Backpressure on a very slow terminal (2KB/s) while the tape streams
// a ~200KB reply: esc pressed midway must reach the loop within 2 s
// (input is not starved behind queued output), the screen must end
// on the cancelled marker, and history must hold the partial reply
// exactly once.

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// 205 bytes every 100ms ≈ 2KB/s, a deterministic throttle.
const (
	slowterminalbackpressurelongreplyChunk = 205
	slowterminalbackpressurelongreplyEvery = 100 * time.Millisecond
	// ~33k words of "wNNNNN " ≈ 200KB.
	slowterminalbackpressurelongreplyWords = 33000
)

// slowterminalbackpressurelongreplyStart is slowterminalStart at 2KB/s.
func slowterminalbackpressurelongreplyStart(t *testing.T, cols, rows int, yml string) *app {
	t.Helper()
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := slowterminalNew(t, cols, rows, slowterminalbackpressurelongreplyChunk, slowterminalbackpressurelongreplyEvery)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=",
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: cols, rows: rows, home: home}
	t.Cleanup(func() {
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	a.waitFor("say something")
	return a
}

// slowterminalbackpressurelongreplyEntries reads this run's one history file.
func slowterminalbackpressurelongreplyEntries(t *testing.T, a *app) []history.Entry {
	t.Helper()
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	if len(paths) != 1 {
		t.Fatalf("want one history file, got %v", paths)
	}
	es, err := history.Read(paths[0])
	if err != nil {
		t.Fatal(err)
	}
	return es
}

func TestSlowTerminalBackpressureLongReply(t *testing.T) {
	t.Parallel()
	tape := slowterminalLongTape(t, slowterminalbackpressurelongreplyWords)
	if st, _ := os.Stat(tape); st.Size() < 190_000 {
		t.Fatalf("tape reply too small: %d bytes", st.Size())
	}
	// 1 ms per word: the whole reply takes well over 30 s to stream.
	a := slowterminalbackpressurelongreplyStart(t, 100, 30, cancelConfig(tape, 1))
	a.typeText("long please")
	a.key(uv.KeyEnter, 0)
	a.waitFor("LONGSTART")
	time.Sleep(2 * time.Second) // midway: output is backed up now
	if n := a.doneCount(); n != 0 {
		t.Fatalf("turn already ended before esc (%d)", n)
	}

	t0 := time.Now()
	a.key(uv.KeyEsc, 0)
	if !a.waitDone(1, 2*time.Second) {
		t.Fatalf("esc not honoured within 2s on a 2KB/s terminal (input starved):\n%s", a.text())
	}
	t.Logf("cancel recorded after %v", time.Since(t0))

	a.waitFor("■ cancelled")
	if s := a.settled(); strings.Contains(s, "LONGEND") {
		t.Fatalf("the reply kept streaming after esc:\n%s", s)
	}

	// A done after the cancelled entry is by design (ui/model.go keeps
	// lastEnd "cancelled" through it).
	var partial, cancelled int
	for _, e := range slowterminalbackpressurelongreplyEntries(t, a) {
		if e.Kind == "cancelled" {
			cancelled++
		}
		if txt, _ := e.Data["text"].(string); strings.Contains(txt, "LONGSTART") {
			partial++
			if strings.Contains(txt, "LONGEND") {
				t.Errorf("history holds the whole reply (%s entry), want a partial one", e.Kind)
			}
		}
	}
	if cancelled != 1 {
		t.Fatalf("history: %d cancelled entries, want 1", cancelled)
	}
	if partial > 1 {
		t.Fatalf("history: the partial reply is recorded %d times, want once", partial)
	}
	t.Run("HistoryPartial", func(t *testing.T) {
		if partial != 1 {
			t.Fatalf("history: %d entries hold the partial reply, want 1", partial)
		}
	})
}
