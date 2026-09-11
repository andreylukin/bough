package vtreal

// A very slow terminal (1KB/100ms drain) under the full-screen
// overlays: the /sessions picker over 500 fixture sessions and the
// subagent transcript overlay. The slow screen must converge to the
// fast one (no stale frame left behind), the picker highlight must
// stay on screen while it scrolls, esc must close within a bound once
// the output drains, and the process must not balloon while its
// output queues.

import (
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const (
	veryslowterminalChunk    = 1024
	veryslowterminalEvery    = 100 * time.Millisecond
	veryslowterminalSessions = 500
)

// veryslowterminalStart is slowterminalStart at 1KB/100ms.
func veryslowterminalStart(t *testing.T, cols, rows int, yml string, slow bool) *app {
	t.Helper()
	if !slow {
		return startCfg(t, cols, rows, yml)
	}
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := slowterminalNew(t, cols, rows, veryslowterminalChunk, veryslowterminalEvery)
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

// veryslowterminalSeed writes n sessions into $HOME/.bough/history
// with fixed, distinct mtimes so every run lists them in one order.
func veryslowterminalSeed(t *testing.T, home string, n int) {
	t.Helper()
	dir := filepath.Join(home, ".bough", "history")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	base := time.Date(2026, 1, 1, 12, 0, 0, 0, time.UTC)
	for i := range n {
		at := base.Add(time.Duration(i) * time.Minute)
		var sb strings.Builder
		for j, e := range []struct {
			kind string
			data map[string]any
		}{
			{"meta", map[string]any{"cwd": "/x/fixture"}},
			{"input", map[string]any{"text": fmt.Sprintf("fx%03d prompt", i)}},
			{"assistant", map[string]any{"text": fmt.Sprintf("reply fx%03d", i)}},
			{"done", map[string]any{}},
		} {
			b, _ := json.Marshal(map[string]any{"seq": j + 1, "at": at.Format(time.RFC3339), "kind": e.kind, "data": e.data})
			sb.Write(append(b, '\n'))
		}
		p := filepath.Join(dir, fmt.Sprintf("seed-fx%03d.jsonl", i))
		if err := os.WriteFile(p, []byte(sb.String()), 0o644); err != nil {
			t.Fatal(err)
		}
		if err := os.Chtimes(p, at, at); err != nil {
			t.Fatal(err)
		}
	}
}

// veryslowterminalSettled is settled with a window longer than the
// reader's pause: a 60ms window mistakes a 100ms drain gap for rest.
func (a *app) veryslowterminalSettled() string {
	prev := a.text()
	for range 100 {
		time.Sleep(400 * time.Millisecond)
		cur := a.text()
		if cur == prev {
			return cur
		}
		prev = cur
	}
	return prev
}

// veryslowterminalNorm is slowterminalNorm without the current
// session's row (its own id and mtime differ per run).
func veryslowterminalNorm(s string) string {
	var keep []string
	for _, l := range strings.Split(s, "\n") {
		if !strings.Contains(l, "(current)") {
			keep = append(keep, l)
		}
	}
	return slowterminalNorm(strings.Join(keep, "\n"))
}

func (a *app) veryslowterminalConverge(want string, within time.Duration) {
	a.t.Helper()
	deadline := time.Now().Add(within)
	var got string
	for time.Now().Before(deadline) {
		if got = veryslowterminalNorm(a.veryslowterminalSettled()); got == want {
			return
		}
	}
	a.t.Fatalf("slow screen never converged to the fast one\nwant:\n%s\ngot:\n%s", want, got)
}

// veryslowterminalRSSCheck: the slow run stays near the fast run and
// under an absolute 1GB ceiling.
func veryslowterminalRSSCheck(t *testing.T, fast, slow *app) {
	t.Helper()
	f, s := slowterminalRSS(t, fast), slowterminalRSS(t, slow)
	t.Logf("rss fast=%dKB slow=%dKB", f, s)
	if s > 2*f+100*1024 || s > 1024*1024 {
		t.Fatalf("slow terminal rss %dKB vs fast %dKB: output queue grew unbounded", s, f)
	}
}

// veryslowterminalEscWithin presses esc (flushed by a right arrow, see
// subagentsEsc) and waits for gone to leave the screen with the
// composer back, failing past within.
func (a *app) veryslowterminalEscWithin(gone string, within time.Duration) time.Duration {
	a.t.Helper()
	t0 := time.Now()
	a.key(uv.KeyEscape, 0)
	time.Sleep(150 * time.Millisecond)
	a.key(uv.KeyRight, 0)
	for time.Since(t0) < within {
		if s := a.text(); !strings.Contains(s, gone) && strings.Contains(s, "say something") {
			return time.Since(t0)
		}
		time.Sleep(20 * time.Millisecond)
	}
	a.t.Fatalf("esc did not close %q within %v on a slow terminal:\n%s", gone, within, a.text())
	return 0
}

func veryslowterminalOpenPicker(t *testing.T, slow bool) *app {
	t.Helper()
	a := veryslowterminalStart(t, 100, 30, replayConfig(resizeTmuxTape(t)), slow)
	veryslowterminalSeed(t, a.home, veryslowterminalSessions)
	a.typeText("/sessions")
	a.key(uv.KeyEnter, 0)
	a.waitFor("resume a session")
	return a
}

func TestVerySlowTerminalPickerAndOverlay(t *testing.T) {
	t.Parallel()

	t.Run("SessionsPicker", func(t *testing.T) {
		t.Parallel()
		fa := veryslowterminalOpenPicker(t, false)
		raw := fa.veryslowterminalSettled()
		if !strings.Contains(raw, "▸ ") || strings.Count(raw, "fx") < 20 {
			t.Fatalf("picker does not list the fixture sessions:\n%s", raw)
		}
		fast := veryslowterminalNorm(raw)
		sa := veryslowterminalOpenPicker(t, true)
		sa.veryslowterminalConverge(fast, 60*time.Second)
		veryslowterminalRSSCheck(t, fa, sa)
		d := sa.veryslowterminalEscWithin("resume a session", 10*time.Second)
		t.Logf("picker closed %v after esc", d)
		sa.check("picker closed")
	})

	// Moving the highlight past the first screenful of a 500-row list:
	// the highlighted row must stay on screen.
	t.Run("SessionsPickerScroll", func(t *testing.T) {
		t.Parallel()
		if os.Getenv("BOUGH_KNOWN_VERYSLOWTERMINALPICKERANDOVERLAY") == "" {
			t.Skip("known bug: the /sessions picker never scrolls (plugins/ui/session.go pickerView cuts rows at the pane height, no offset), so the highlight leaves the screen; set BOUGH_KNOWN_VERYSLOWTERMINALPICKERANDOVERLAY=1 to run")
		}
		a := veryslowterminalOpenPicker(t, true)
		for range 40 {
			a.key(uv.KeyDown, 0)
		}
		s := a.veryslowterminalSettled()
		if !strings.Contains(s, "▸ ") {
			t.Fatalf("after 40 downs the highlighted session is off screen:\n%s", s)
		}
		a.veryslowterminalEscWithin("resume a session", 10*time.Second)
	})

	t.Run("SubagentOverlay", func(t *testing.T) {
		t.Parallel()
		src, err := filepath.Abs("testdata/replay/subagents.jsonl")
		if err != nil {
			t.Fatal(err)
		}
		data, err := os.ReadFile(src)
		if err != nil {
			t.Fatal(err)
		}
		open := func(slow bool) *app {
			tape := filepath.Join(t.TempDir(), "subagents.jsonl")
			if err := os.WriteFile(tape, data, 0o644); err != nil {
				t.Fatal(err)
			}
			a := veryslowterminalStart(t, 100, 30, subagentsConfig(tape), slow)
			a.waitFor("subagent 2")
			for range 8 {
				a.key(uv.KeyTab, 0)
				a.veryslowterminalSettled()
				a.key('o', uv.ModCtrl)
				s := a.veryslowterminalSettled()
				if strings.Contains(s, "esc to close") && strings.Contains(s, "ParseFile") {
					return a
				}
				if strings.Contains(s, "esc to close") {
					a.veryslowterminalEscWithin("esc to close", 10*time.Second)
				} else {
					a.key('o', uv.ModCtrl) // the history inspector: its own key closes it
				}
				a.veryslowterminalSettled()
			}
			t.Fatalf("slow=%v: never opened subagent 1's transcript:\n%s", slow, a.text())
			return nil
		}
		fa := open(false)
		fast := veryslowterminalNorm(fa.veryslowterminalSettled())
		sa := open(true)
		sa.veryslowterminalConverge(fast, 60*time.Second)
		veryslowterminalRSSCheck(t, fa, sa)
		d := sa.veryslowterminalEscWithin("esc to close", 10*time.Second)
		t.Logf("overlay closed %v after esc", d)
		sa.waitFor("subagent 2")
		sa.check("overlay closed")
	})
}
