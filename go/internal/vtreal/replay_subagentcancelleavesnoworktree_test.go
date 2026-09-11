package vtreal

// Esc during tools.spawnAll: three real children (workers + codemode +
// tools-basic; only the model side comes from a tape) are cancelled
// mid-run. The tape is written per test with a unique token, the
// child sleep duration (the shell reads its command from stdin, so
// only argv can carry it), so pgrep sees only this run's processes.
// Children share the replay llm, so their three replies are identical
// and scheduling order does not matter. The parent starts a background job before spawning; a turn
// cancel must not touch it.

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

// subagentCancelLeavesNoWorktreeConfig is jobsConfig (real codemode and
// tools-basic) plus the workers row that registers tools.spawnAll.
func subagentCancelLeavesNoWorktreeConfig(tape string) string {
	cfg := jobsConfig(tape)
	out := strings.Replace(cfg, "- id: commands\n", "- id: workers\n  plugin: workers\n- id: commands\n", 1)
	if out == cfg {
		panic("subagentCancelLeavesNoWorktreeConfig: jobsConfig changed shape; workers row not added")
	}
	return out
}

// subagentCancelLeavesNoWorktreeTape writes the tape: the parent's block
// (a background job, then spawnAll of three), three identical child
// blocks that sleep, and a stop reply for the wake turn the finished
// job opens.
func subagentCancelLeavesNoWorktreeTape(t *testing.T, tok string) string {
	t.Helper()
	parent := fmt.Sprintf("```js\nconsole.log(tools.bash(\"sleep 12; echo BGJOB-%s\", 60))\nconsole.log(tools.spawnAll([\"task one\", \"task two\", \"task three\"]))\n```", tok)
	child := fmt.Sprintf("```js\nconsole.log(tools.bash(\"sleep %s; echo CHILD-DONE\"))\n```", tok)
	entries := []struct {
		kind, text string
	}{
		{"input", "fan out"},
		{"assistant", parent},
		{"assistant", child},
		{"assistant", child},
		{"assistant", child},
		{"assistant", "```stop\nWOKE-" + tok + "\n```"},
	}
	var sb strings.Builder
	for i, e := range entries {
		b, err := json.Marshal(map[string]any{
			"seq": i + 1, "at": time.Date(2026, 9, 10, 11, 0, i, 0, time.UTC).Format(time.RFC3339),
			"kind": e.kind, "data": map[string]any{"text": e.text},
		})
		if err != nil {
			t.Fatal(err)
		}
		sb.Write(b)
		sb.WriteByte('\n')
	}
	p := filepath.Join(t.TempDir(), "tape.jsonl")
	if err := os.WriteFile(p, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// subagentCancelLeavesNoWorktreeCount counts history entries of kind.
func subagentCancelLeavesNoWorktreeCount(a *app, kind string) int {
	n := 0
	for _, e := range jobsHistory(a) {
		if e.Kind == kind {
			n++
		}
	}
	return n
}

// subagentCancelLeavesNoWorktreeProcs is how many live processes carry
// pattern on their command line.
func subagentCancelLeavesNoWorktreeProcs(pattern string) int {
	out, _ := exec.Command("pgrep", "-f", pattern).Output()
	return len(strings.Fields(string(out)))
}

func TestSubagentCancelLeavesNoWorktree(t *testing.T) {
	t.Parallel()
	tok := fmt.Sprintf("40.%06d", time.Now().UnixNano()%1000000)
	a := startCfg(t, 110, 40, subagentCancelLeavesNoWorktreeConfig(subagentCancelLeavesNoWorktreeTape(t, tok)))
	jobsSay(a, "fan out")

	// Gate: all three children announced and replied to, and a child's
	// sleep is actually running.
	deadline := time.Now().Add(30 * time.Second)
	for subagentCancelLeavesNoWorktreeCount(a, "sub:assistant") < 3 || subagentCancelLeavesNoWorktreeProcs("sleep "+tok) == 0 {
		if time.Now().After(deadline) {
			t.Fatalf("children never started (sub:assistant=%d):\n%s", subagentCancelLeavesNoWorktreeCount(a, "sub:assistant"), a.text())
		}
		time.Sleep(50 * time.Millisecond)
	}
	if n := subagentCancelLeavesNoWorktreeCount(a, "sub:start"); n != 3 {
		t.Fatalf("want 3 sub:start, got %d", n)
	}

	a.key(uv.KeyEsc, 0)
	if !a.waitDone(1, 15*time.Second) {
		t.Fatalf("esc did not end the turn:\n%s", a.text())
	}

	// Runs first, while the job (12s) has not yet woken a turn.
	t.Run("history_quiet_and_no_processes", func(t *testing.T) {
		before := len(jobsHistory(a))
		time.Sleep(2 * time.Second)
		after := jobsHistory(a)
		if len(after) != before {
			t.Errorf("history kept growing after the cancel: %d -> %d; tail: %+v", before, len(after), after[before:])
		}
		if n := subagentCancelLeavesNoWorktreeProcs("sleep " + tok); n != 0 {
			t.Errorf("%d child shell process(es) still alive 2s after the cancel", n)
		}
	})

	t.Run("cards_cancelled", func(t *testing.T) {
		deadline := time.Now().Add(10 * time.Second)
		for {
			cancelled := map[float64]bool{}
			for _, e := range jobsHistory(a) {
				if e.Kind == "sub:done" && e.Data["status"] == "cancelled" {
					w, _ := e.Data["worker"].(float64)
					cancelled[w] = true
				}
			}
			if len(cancelled) == 3 {
				break
			}
			if time.Now().After(deadline) {
				t.Fatalf("want sub:done status=cancelled for 3 workers, got %v", cancelled)
			}
			time.Sleep(50 * time.Millisecond)
		}
		// The screen: every card head says cancelled. Polled here, not
		// with a.waitUntil, which would fail the parent test.
		var s string
		for deadline := time.Now().Add(10 * time.Second); time.Now().Before(deadline); time.Sleep(100 * time.Millisecond) {
			s = a.text()
			n := 0
			for _, l := range strings.Split(s, "\n") {
				if strings.Contains(l, "subagent") && strings.Contains(l, "cancelled") && !strings.Contains(l, "error") {
					n++
				}
			}
			if n == 3 {
				return
			}
		}
		t.Errorf("want three subagent cards reading cancelled (not error):\n%s", s)
	})

	t.Run("no_workspace_dirs", func(t *testing.T) {
		filepath.WalkDir(a.home, func(p string, d os.DirEntry, err error) error {
			if err == nil && d.IsDir() {
				b := strings.ToLower(d.Name())
				if strings.Contains(b, "worktree") || strings.Contains(b, "workspace") || strings.HasPrefix(b, "worker") {
					t.Errorf("subagent workspace left behind: %s", p)
				}
			}
			return nil
		})
	})

	t.Run("background_job_unaffected", func(t *testing.T) {
		// Not waitDone: the cancelled turn writes both "cancelled" and
		// "done", so the count reaches 2 before the job ever wakes one.
		found := false
		for deadline := time.Now().Add(30 * time.Second); !found && time.Now().Before(deadline); time.Sleep(100 * time.Millisecond) {
			for _, e := range jobsHistory(a) {
				if s := fmt.Sprint(e.Data["text"]); e.Kind == "input" && strings.Contains(s, "BGJOB-"+tok) && strings.Contains(s, "exited 0") {
					found = true
				}
			}
		}
		if !found {
			t.Fatalf("no wake input carrying the job's clean exit; the cancel touched the job:\n%s", a.text())
		}
		var s string
		for deadline := time.Now().Add(15 * time.Second); time.Now().Before(deadline); time.Sleep(100 * time.Millisecond) {
			if s = a.text(); strings.Contains(s, "WOKE-"+tok) {
				return
			}
		}
		t.Errorf("the wake turn's reply never rendered:\n%s", s)
	})
}
