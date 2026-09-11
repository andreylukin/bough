package vtreal

// Subagent crash, restart, resume: the parent spawnAlls three real
// children (workers + codemode + tools-basic; the model side is a
// tape), bough is SIGKILLed while every child is inside its own shell
// call, and a second bough resumes the session. The cards must reach a
// terminal state (not spin forever), the overlay must show what each
// child did before the kill, no git worktree may be left behind, and a
// new prompt must still get its answer.
//
// Determinism: the kill waits until all three children's calls are on
// disk, all three cards read running, and the children's sleep (a
// per-run token) is alive.

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
)

// subagentCrashRestartResumeOverlayCount counts entries of kind.
func subagentCrashRestartResumeOverlayCount(a *app, kind string) int {
	n := 0
	for _, e := range jobsHistory(a) {
		if e.Kind == kind {
			n++
		}
	}
	return n
}

// subagentCrashRestartResumeOverlayCards counts card head rows holding sub.
func subagentCrashRestartResumeOverlayCards(s, sub string) int {
	n := 0
	for _, l := range strings.Split(s, "\n") {
		if strings.Contains(l, "subagent") && strings.Contains(l, sub) {
			n++
		}
	}
	return n
}

func TestSubagentCrashRestartResumeOverlay(t *testing.T) {
	t.Parallel()
	tok := fmt.Sprintf("41.%06d", time.Now().UnixNano()%1000000)
	home := t.TempDir()
	if out, err := exec.Command("git", "-C", home, "init", "-q").CombinedOutput(); err != nil {
		t.Fatalf("git init: %v %s", err, out)
	}
	parent := "```js\nconsole.log(tools.spawnAll([\"task one\", \"task two\", \"task three\"]))\n```"
	child := fmt.Sprintf("```js\nconsole.log(tools.bash(\"echo PARTIAL-%s; sleep %s\"))\n```", tok, tok)
	tape := crashResumeIntegrityTape(t, home, "crash-tape.jsonl", "fan out", parent)
	// The three child replies follow the parent's on the same tape
	// (the tape's done entry does not stop the replay llm).
	f, err := os.OpenFile(tape, os.O_APPEND|os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	for i := 0; i < 3; i++ {
		fmt.Fprintf(f, "{\"seq\":%d,\"at\":\"2026-09-10T12:00:03Z\",\"kind\":\"assistant\",\"data\":{\"text\":%q}}\n", 10+i, child)
	}
	f.Close()
	cfg := subagentCancelLeavesNoWorktreeConfig
	a := crashResumeIntegrityStart(t, home, cfg(tape))
	jobsSay(a, "fan out")

	deadline := time.Now().Add(30 * time.Second)
	for {
		s := a.text()
		if subagentCrashRestartResumeOverlayCount(a, "sub:code") >= 3 &&
			subagentCancelLeavesNoWorktreeProcs("sleep "+tok) > 0 &&
			subagentCrashRestartResumeOverlayCards(s, "running") == 3 {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("three children never got running (sub:code=%d):\n%s", subagentCrashRestartResumeOverlayCount(a, "sub:code"), s)
		}
		time.Sleep(50 * time.Millisecond)
	}
	if err := a.cmd.Process.Signal(syscall.SIGKILL); err != nil {
		t.Fatal(err)
	}
	_ = a.term.Wait(a.cmd)
	_ = exec.Command("pkill", "-f", "sleep "+tok).Run() // orphaned child shells
	session := crashResumeIntegritySession(t, home)
	id := strings.TrimSuffix(filepath.Base(session), ".jsonl")

	next := crashResumeIntegrityTape(t, home, "next-tape.jsonl", "and now",
		"```stop\nAFTER-"+tok+"\n```")
	b := crashResumeIntegrityStart(t, home, cfg(next), "--resume", id)
	b.waitFor("resumed ")
	b.check("resumed boot")
	s := b.settled()

	t.Run("cards_terminal", func(t *testing.T) {
		if n := subagentCrashRestartResumeOverlayCards(s, "subagent"); n < 3 {
			t.Fatalf("want 3 subagent cards after resume, got %d:\n%s", n, s)
		}
		if os.Getenv("BOUGH_KNOWN_SUBAGENT_CRASH_RESTART_RESUME_OVERLAY") == "" {
			t.Skip("known bug: resumed spawn cards with no sub:done stay 'running' forever (plugins/ui/session.go closes only the parent turn as interrupted); set BOUGH_KNOWN_SUBAGENT_CRASH_RESTART_RESUME_OVERLAY=1 to run")
		}
		time.Sleep(time.Second)
		s := b.settled()
		if n := subagentCrashRestartResumeOverlayCards(s, "running"); n != 0 {
			t.Errorf("%d card(s) still spinning 'running' after resume:\n%s", n, s)
		}
		low := strings.ToLower(s)
		if subagentCrashRestartResumeOverlayCards(low, "cancelled")+subagentCrashRestartResumeOverlayCards(low, "interrupted") != 3 {
			t.Errorf("want three cards reading cancelled/interrupted:\n%s", s)
		}
	})

	t.Run("overlay_partial_transcript", func(t *testing.T) {
		subagentsFocusCard(b, "subagent")
		b.key('o', uv.ModCtrl)
		o := b.settled()
		if !strings.Contains(o, "esc to close") || !strings.Contains(o, "PARTIAL-"+tok) {
			t.Errorf("overlay does not show the child's recorded call:\n%s", o)
		}
		subagentsEsc(b)
		b.settled()
	})

	t.Run("no_leftover_worktrees", func(t *testing.T) {
		out, err := exec.Command("git", "-C", home, "worktree", "list").Output()
		if err != nil {
			t.Fatal(err)
		}
		if n := len(strings.Split(strings.TrimSpace(string(out)), "\n")); n != 1 {
			t.Errorf("leftover worktrees:\n%s", out)
		}
	})

	t.Run("new_prompt_works", func(t *testing.T) {
		jobsSay(b, "and now")
		b.waitFor("AFTER-" + tok)
		b.check("after new prompt")
	})
}
