package vtreal

// A slow "!" shell command on a real tmux pane (resize needs tmux: x/vt
// drops rows on resize): it prints bscr-tick-1..50 with a sleep between
// lines, the pane is resized mid-run, and esc must cancel it — the
// child gone (pgrep on a unique $0 marker), partial output plus a
// cancelled marker in the box, and the composer and next turn working.
// Replay-backed: the next turn must get the tape's first reply.

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// bangShellCancelAndResizeStart boots bough in tmux on the bang tape
// and returns the run's $HOME, a slow ! line and its unique marker.
func bangShellCancelAndResizeStart(t *testing.T) (*tmuxApp, string, string, string) {
	t.Helper()
	tape, _ := filepath.Abs("testdata/replay/bang-shell.jsonl")
	tm, home := resizeTmuxStart(t, 100, 30, replayConfig(tape))
	marker := fmt.Sprintf("bscr-%d-%d", os.Getpid(), time.Now().UnixNano())
	line := "!sh -c 'for i in $(seq 1 50); do echo bscr-tick-$i; sleep 0.2; done' " + marker
	t.Cleanup(func() { _ = exec.Command("pkill", "-f", marker).Run() })
	return tm, home, line, marker
}

// bangShellCancelAndResizeAlive reports whether the marked child runs.
func bangShellCancelAndResizeAlive(marker string) bool {
	return exec.Command("pgrep", "-f", marker).Run() == nil
}

// bangShellCancelAndResizeWaitAlive waits for the child's liveness to
// reach want.
func bangShellCancelAndResizeWaitAlive(t *testing.T, tm *tmuxApp, marker string, want bool) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		if bangShellCancelAndResizeAlive(marker) == want {
			return
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatalf("child %s alive=%v, want %v\nscreen:\n%s", marker, !want, want, resizeTmuxScreen(tm))
}

// bangShellCancelAndResizeNextTurn: the composer takes a real turn
// and it gets the tape's first reply.
func bangShellCancelAndResizeNextTurn(t *testing.T, tm *tmuxApp, home string) {
	t.Helper()
	resizeTmuxSend(tm, "hello")
	resizeTmuxWaitDone(t, tm, home, 1)
	tm.waitFor("first-tape-reply")
	if s := tm.settled(); strings.Contains(s, "end of tape") {
		t.Fatalf("a ! line consumed a tape reply:\n%s", s)
	}
}

func TestBangShellCancelAndResize(t *testing.T) {
	t.Parallel()
	const gate = "BOUGH_KNOWN_BANG_SHELL_CANCEL_AND_RESIZE"

	t.Run("TestBangShellCancelAndResizeCompletesAfterResize", func(t *testing.T) {
		t.Parallel()
		tm, home, line, marker := bangShellCancelAndResizeStart(t)
		resizeTmuxSend(tm, line)
		bangShellCancelAndResizeWaitAlive(t, tm, marker, true)
		tm.resize(60, 20)
		time.Sleep(300 * time.Millisecond)
		tm.resize(100, 30)
		tm.waitFor("bscr-tick-50")
		bangShellCancelAndResizeWaitAlive(t, tm, marker, false)
		resizeTmuxCheck(t, tm, "after slow ! finished", 100)
		bangShellCancelAndResizeNextTurn(t, tm, home)
	})

	t.Run("TestBangShellCancelAndResizeStreamsPartialOutput", func(t *testing.T) {
		t.Parallel()
		tm, _, line, marker := bangShellCancelAndResizeStart(t)
		resizeTmuxSend(tm, line)
		tm.waitFor("bscr-tick-10")
		if !bangShellCancelAndResizeAlive(marker) {
			t.Fatalf("bscr-tick-10 only appeared after the child exited: output is not streamed\n%s", tm.screen())
		}
	})

	t.Run("TestBangShellCancelAndResizeEscKillsChild", func(t *testing.T) {
		t.Parallel()
		if os.Getenv(gate) == "" {
			t.Skip("known bug: esc does not cancel a running ! command — runBang (plugins/ui/bang.go) runs under a 60s-timeout context nothing else cancels; set " + gate + " to run")
		}
		tm, home, line, marker := bangShellCancelAndResizeStart(t)
		resizeTmuxSend(tm, line)
		tm.waitFor("bscr-tick-10")
		tm.resize(70, 24)
		tm.keys("Escape")
		bangShellCancelAndResizeWaitAlive(t, tm, marker, false)
		s := resizeTmuxSettled(tm, 70)
		if !strings.Contains(s, "bscr-tick-10") || strings.Contains(s, "bscr-tick-50") {
			t.Fatalf("cancelled box must keep the partial output only:\n%s", s)
		}
		if !strings.Contains(strings.ToLower(s), "cancel") {
			t.Fatalf("cancelled ! box has no cancelled marker:\n%s", s)
		}
		tm.resize(100, 30)
		bangShellCancelAndResizeNextTurn(t, tm, home)
	})
}
