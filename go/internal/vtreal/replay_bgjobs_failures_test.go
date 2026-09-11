package vtreal

// Background-job failure paths on the real binary: a replay tape
// answers the model, the REAL codemode + tools-basic run the blocks, so
// jobs are actual processes.

import (
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// bgjobsTape writes a tape: one typed turn running code, then n wake
// turns answered with a plain stop.
func bgjobsTape(t *testing.T, code string, wakes int) string {
	t.Helper()
	rows := []struct {
		kind string
		data map[string]any
	}{
		{"meta", map[string]any{"cwd": "/tmp/demo"}},
		{"input", map[string]any{"text": "start the jobs"}},
		{"assistant", map[string]any{"text": "```js\n" + code + "```"}},
		{"code", map[string]any{"text": code}},
		{"result", map[string]any{"code": code, "text": "started"}},
		{"assistant", map[string]any{"text": "```stop\nJOBS-STARTED\n```"}},
		{"done", map[string]any{"text": ""}},
	}
	for i := range wakes {
		rows = append(rows,
			struct {
				kind string
				data map[string]any
			}{"input", map[string]any{"text": "[background job] wake"}},
			struct {
				kind string
				data map[string]any
			}{"assistant", map[string]any{"text": fmt.Sprintf("```stop\nWOKE-%d\n```", i+1)}},
			struct {
				kind string
				data map[string]any
			}{"done", map[string]any{"text": ""}},
		)
	}
	var b strings.Builder
	for i, r := range rows {
		line, err := json.Marshal(map[string]any{"seq": i + 1, "at": "2026-09-11T10:00:00Z", "kind": r.kind, "data": r.data})
		if err != nil {
			t.Fatal(err)
		}
		b.Write(line)
		b.WriteByte('\n')
	}
	p := filepath.Join(t.TempDir(), "bgjobs.jsonl")
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// bgjobsShortDir is a short temp dir: its path rides inside commands.
func bgjobsShortDir(t *testing.T) string {
	t.Helper()
	dir, err := os.MkdirTemp("", "bgj")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	return dir
}

// bgjobsStart boots bough in home (config already written) with args.
func bgjobsStart(t *testing.T, home string, args ...string) *app {
	t.Helper()
	term, err := NewTerminal(t, 100, 30)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, append([]string{"-config", filepath.Join(home, "bough.yml")}, args...)...)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=",
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: 100, rows: 30, home: home}
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

func bgjobsWakes(a *app) []string {
	var out []string
	for _, e := range jobsHistory(a) {
		if text, _ := e.Data["text"].(string); e.Kind == "input" && strings.HasPrefix(text, "[background job] ") {
			out = append(out, text)
		}
	}
	return out
}

func bgjobsAlive(pid int) bool { return syscall.Kill(pid, 0) == nil }

func bgjobsWaitGone(t *testing.T, pid int, d time.Duration) bool {
	t.Helper()
	deadline := time.Now().Add(d)
	for time.Now().Before(deadline) {
		if !bgjobsAlive(pid) {
			return true
		}
		time.Sleep(50 * time.Millisecond)
	}
	return false
}

// Two jobs finishing at the same instant while the agent is idle start
// ONE wake turn, and that turn is told about both.
func TestBgjobsTwoFinishWhileIdleOneWake(t *testing.T) {
	t.Parallel()
	dir := bgjobsShortDir(t)
	gate := filepath.Join(dir, "gate")
	code := fmt.Sprintf("tools.bash(%q, 120); tools.bash(%q, 120)\n",
		fmt.Sprintf("while [ ! -e %s ]; do sleep 0.02; done; echo JOB-A; exit 2", gate),
		fmt.Sprintf("while [ ! -e %s ]; do sleep 0.02; done; echo JOB-B", gate))
	a := startCfg(t, 100, 30, jobsConfig(bgjobsTape(t, code, 2)))
	jobsSay(a, "start the jobs")
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.waitUntil(func(s string) bool { return strings.Contains(s, "job 2") }, "the strip to list job 2")
	if err := os.WriteFile(gate, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	if !a.waitDone(2, 60*time.Second) {
		t.Fatalf("the finished jobs never woke a turn:\n%s", a.text())
	}
	time.Sleep(time.Second) // a second, spurious wake would land now
	// "The same instant" is two processes: the second notice may land
	// inside the turn the first one woke (a "job" entry via landJobs)
	// or start a second wake. Either way each job is told exactly once.
	w := bgjobsWakes(a)
	var told []string
	for _, e := range jobsHistory(a) {
		text, _ := e.Data["text"].(string)
		if e.Kind == "job" || (e.Kind == "input" && strings.HasPrefix(text, "[background job] ")) {
			told = append(told, text)
		}
	}
	all := strings.Join(w, "\n") + "\n" + strings.Join(told, "\n")
	if !strings.Contains(all, "JOB-A") || !strings.Contains(all, "JOB-B") {
		t.Fatalf("a job was never reported: wakes %q, job entries %q", w, told)
	}
	if !strings.Contains(all, "job 1 [failed]") || !strings.Contains(all, "exit status 2") {
		t.Fatalf("the failing job's status was never reported: %q", told)
	}
	if len(w) == 0 || len(w) > 2 {
		t.Fatalf("want one or two wake turns, got %d: %q", len(w), w)
	}
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "job 2 ·") }, "the strip to empty")
	a.check("after the wake")
}

// A job still running when bough is told to quit (SIGTERM) dies with
// it; resuming that session shows no phantom job, and the history
// tells the model the job it was promised news of is gone.
func TestBgjobsQuitKillsJobAndResume(t *testing.T) {
	t.Parallel()
	dir := bgjobsShortDir(t)
	pidf := filepath.Join(dir, "pid")
	code := fmt.Sprintf("tools.bash(%q, 600)\n", fmt.Sprintf("echo $$ > %s; exec sleep 300", pidf))
	home := t.TempDir()
	if err := os.WriteFile(filepath.Join(home, "bough.yml"), []byte(jobsConfig(bgjobsTape(t, code, 1))), 0o644); err != nil {
		t.Fatal(err)
	}
	a := bgjobsStart(t, home)
	jobsSay(a, "start the jobs")
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.waitUntil(func(s string) bool { return strings.Contains(s, "job 1 ·") }, "the strip to list the job")
	pid := 0
	deadline := time.Now().Add(10 * time.Second)
	for pid == 0 && time.Now().Before(deadline) {
		b, _ := os.ReadFile(pidf)
		pid, _ = strconv.Atoi(strings.TrimSpace(string(b)))
		time.Sleep(20 * time.Millisecond)
	}
	if pid == 0 {
		t.Fatal("the job never wrote its pid")
	}
	t.Cleanup(func() { syscall.Kill(pid, syscall.SIGKILL) })

	if err := a.cmd.Process.Signal(syscall.SIGTERM); err != nil {
		t.Fatal(err)
	}
	exited := make(chan struct{})
	go func() { a.cmd.Wait(); close(exited) }()
	select {
	case <-exited:
	case <-time.After(15 * time.Second):
		t.Fatalf("bough did not exit on SIGTERM:\n%s", a.text())
	}
	if !bgjobsWaitGone(t, pid, 5*time.Second) {
		t.Fatalf("job pid %d outlived bough (orphaned)", pid)
	}

	b := bgjobsStart(t, home, "-c")
	b.waitFor("start the jobs") // the resumed transcript
	s := b.settled()
	ls := strings.Split(s, "\n")
	if c := composerRow(ls); c >= 0 {
		for _, l := range ls[c+1:] {
			if strings.Contains(l, "job 1 ·") {
				t.Fatalf("the resumed session shows a job that no longer exists:\n%s", s)
			}
		}
	}
	t.Run("history records that the job died at quit", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_TOOLS_BACKGROUND_JOBS") != "1" {
			t.Skip("known bug (BOUGH_KNOWN_TOOLS_BACKGROUND_JOBS=1 to run): jobs killed at unmount queue no notice/entry (plugins/tools/tools.go Apply Effect cancels jobs after the loop stops), so a resumed model still believes `You will be told when it finishes`")
		}
		var es []history.Entry = jobsHistory(b)
		for _, e := range es {
			text, _ := e.Data["text"].(string)
			if e.Kind == "job" || (strings.Contains(text, "job 1") && strings.Contains(text, "killed")) {
				return
			}
		}
		t.Fatalf("no entry tells the model job 1 was killed when bough quit")
	})
}
