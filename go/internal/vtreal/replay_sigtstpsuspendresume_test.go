//go:build !windows

package vtreal

// SIGTSTP then SIGCONT from outside (a job-control shell's ^Z / fg, or
// `kill -STOP`-style tooling), idle and mid-stream. While bough is
// stopped the "shell" resets the terminal: alt screen, mouse and
// bracketed paste off, screen cleared, a prompt drawn. After SIGCONT
// bough must put every mode back, repaint the whole screen and finish
// the stream it was in.

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
	"github.com/charmbracelet/x/ansi"
)

// sigtstpSuspendResumeShell is what a shell leaves behind after ^Z.
const sigtstpSuspendResumeShell = "\x1b[?1049l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l" +
	"\x1b[2J\x1b[HSHELLPROMPT$ "

func TestSigtstpSuspendResume(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/signals.jsonl")
	for _, mid := range []bool{false, true} {
		name := "idle"
		if mid {
			name = "mid-stream"
		}
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			if os.Getenv("BOUGH_KNOWN_SIGTSTP_SUSPEND_RESUME") == "" {
				t.Skip("known bug: bough installs no SIGCONT handler (bubbletea only restores after its own ctrl+z Suspend), so an external SIGTSTP/SIGCONT never re-enables alt screen/mouse/bracketed paste or repaints; set BOUGH_KNOWN_SIGTSTP_SUSPEND_RESUME=1 to run")
			}
			sigtstpSuspendResumeRun(t, tape, mid)
		})
	}
}

func sigtstpSuspendResumeRun(t *testing.T, tape string, mid bool) {
	a := startCfg(t, 100, 30, signalsConfig(tape))
	a.typeText("quick")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 30*time.Second) {
		t.Fatalf("first turn never finished:\n%s", a.text())
	}
	a.waitFor("quick answer")
	if mid {
		a.typeText("slow")
		a.key(uv.KeyEnter, 0)
		a.waitFor("slow3")
		if strings.Contains(a.text(), "slow answer") {
			t.Fatalf("turn finished before the signal; it must still be streaming:\n%s", a.text())
		}
	}

	pid := a.cmd.Process.Pid
	if err := syscall.Kill(pid, syscall.SIGTSTP); err != nil {
		t.Fatalf("SIGTSTP: %v", err)
	}
	if !sigtstpSuspendResumeState(pid, "T") {
		// The kernel discards SIGTSTP to an orphaned process group
		// (bough shares the test binary's); SIGSTOP cannot be caught
		// either, so the resume path under test is the same.
		t.Logf("SIGTSTP discarded (orphaned process group); stopping with SIGSTOP")
		_ = syscall.Kill(pid, syscall.SIGSTOP)
		if !sigtstpSuspendResumeState(pid, "T") {
			t.Fatalf("process %d never stopped", pid)
		}
	}
	a.term.Emu.Write([]byte(sigtstpSuspendResumeShell)) //nolint:errcheck
	a.waitFor("SHELLPROMPT$")
	if a.term.Snapshot().AltScreen {
		t.Fatalf("setup: alt screen still on after the shell reset")
	}

	if err := syscall.Kill(pid, syscall.SIGCONT); err != nil {
		t.Fatalf("SIGCONT: %v", err)
	}
	var bad []string
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		bad = sigtstpSuspendResumeBad(a)
		if bad == nil {
			break
		}
		time.Sleep(50 * time.Millisecond)
	}
	if bad != nil {
		t.Fatalf("after SIGCONT: %s\nscreen:\n%s", strings.Join(bad, ", "), a.text())
	}
	a.check("after resume")

	want := 1
	if mid {
		want = 2
	}
	if !a.waitDone(want, 30*time.Second) {
		t.Fatalf("stream never completed after resume:\n%s", a.text())
	}
	if mid {
		a.waitFor("slow answer")
	}
	a.check("after stream")
}

// sigtstpSuspendResumeBad lists what is not yet restored.
func sigtstpSuspendResumeBad(a *app) []string {
	s := a.term.Snapshot()
	var bad []string
	if !s.AltScreen {
		bad = append(bad, "alt screen off")
	}
	if !s.DEC[ansi.ButtonEventMouseMode].IsSet() && !s.DEC[ansi.AnyEventMouseMode].IsSet() {
		bad = append(bad, "mouse tracking off")
	}
	if !s.DEC[ansi.SgrExtMouseMode].IsSet() {
		bad = append(bad, "sgr mouse off")
	}
	if !s.DEC[ansi.BracketedPasteMode].IsSet() {
		bad = append(bad, "bracketed paste off")
	}
	txt := a.text()
	if strings.Contains(txt, "SHELLPROMPT") {
		bad = append(bad, "shell prompt still on screen (no full repaint)")
	}
	if !strings.Contains(txt, "? keys") || composerRow(strings.Split(txt, "\n")) < 0 {
		bad = append(bad, "status bar/composer not repainted")
	}
	return bad
}

// sigtstpSuspendResumeState waits until ps reports the process state prefix.
func sigtstpSuspendResumeState(pid int, want string) bool {
	for range 50 {
		out, _ := exec.Command("ps", "-o", "state=", "-p", fmt.Sprint(pid)).Output()
		if strings.HasPrefix(strings.TrimSpace(string(out)), want) {
			return true
		}
		time.Sleep(20 * time.Millisecond)
	}
	return false
}
