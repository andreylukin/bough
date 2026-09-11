package vtreal

// Headless run whose stdout reader goes away after the first line
// (`bough --headless | head -1`). The process must not hang, must not
// panic, must leave the session's history entry finalized with a
// "done", and must exit non-zero in the documented way: Go's runtime
// turns a write to a closed stdout pipe into SIGPIPE, so the exit is
// "killed by SIGPIPE" (the shell's 141). When every write already sat
// in the pipe buffer before the close, nothing sees EPIPE and the run
// exits 0 as usual; both outcomes are accepted, nothing else is.

import (
	"bufio"
	"bytes"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

type headlessSigpipeStdoutClosedResult struct {
	first   string
	stderr  string
	state   *os.ProcessState
	elapsed time.Duration
	hist    string
}

// headlessSigpipeStdoutClosedRun starts `bough --headless` on the tape
// with stdout on a pipe, reads one line, closes the read end, and
// waits (bounded) for the process to exit.
func headlessSigpipeStdoutClosedRun(t *testing.T, tapeName string, prompts ...string) headlessSigpipeStdoutClosedResult {
	t.Helper()
	tape, err := filepath.Abs(filepath.Join("testdata", "replay", tapeName))
	if err != nil {
		t.Fatal(err)
	}
	home := t.TempDir()
	hist := filepath.Join(home, "session.jsonl")
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(resumeConfig(tape, hist)), 0o644); err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg, "--headless")
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "NO_COLOR=", "BOUGH_VERBOSE=", "BOUGH_HEADLESS_IDLE=30",
	)
	cmd.Stdin = strings.NewReader(strings.Join(prompts, "\n") + "\n")
	var errb bytes.Buffer
	cmd.Stderr = &errb
	rd, wr, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	cmd.Stdout = wr
	start := time.Now()
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	wr.Close()
	first, _ := bufio.NewReader(rd).ReadString('\n')
	rd.Close() // the reader goes away: every later write is EPIPE

	done := make(chan struct{})
	go func() { cmd.Wait(); close(done) }()
	select {
	case <-done:
	case <-time.After(30 * time.Second):
		cmd.Process.Kill()
		<-done
		t.Fatalf("bough --headless hung after its stdout reader closed (first line %q)\n--- stderr ---\n%s", first, errb.String())
	}
	return headlessSigpipeStdoutClosedResult{first: first, stderr: errb.String(), state: cmd.ProcessState, elapsed: time.Since(start), hist: hist}
}

func TestHeadlessSigpipeStdoutClosed(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name    string
		prompts []string
	}{
		{"OneTurn", []string{"how many go files are here"}},
		// A second prompt keeps the process writing after the close.
		{"TwoTurns", []string{"how many go files are here", "/help"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			r := headlessSigpipeStdoutClosedRun(t, "headless_answer.jsonl", tc.prompts...)
			screen := "first line " + strings.TrimSpace(r.first) + "\nstate " + r.state.String() + "\n--- stderr ---\n" + r.stderr

			t.Logf("exit: %s in %s", r.state, r.elapsed)
			if !strings.HasPrefix(r.first, "[") {
				t.Errorf("first stdout line must be an event:\n%s", screen)
			}
			ws := r.state.Sys().(syscall.WaitStatus)
			switch {
			case ws.Signaled() && ws.Signal() == syscall.SIGPIPE:
			case ws.Exited() && ws.ExitStatus() == 0:
			default:
				t.Errorf("exit %s, want killed by SIGPIPE (or 0 when no write saw EPIPE):\n%s", r.state, screen)
			}
			for _, bad := range []string{"panic:", "goroutine ", "fatal error:", "DATA RACE"} {
				if strings.Contains(r.stderr, bad) {
					t.Errorf("stderr carries %q:\n%s", bad, screen)
				}
			}
			entries, err := history.Read(r.hist)
			if err != nil {
				t.Fatalf("reading history %s: %v\n%s", r.hist, err, screen)
			}
			dones := 0
			for _, e := range entries {
				if e.Kind == "done" || e.Kind == "cancelled" {
					dones++
				}
			}
			if dones < 1 {
				t.Errorf("history has no done entry after stdout closed (%d entries):\n%s", len(entries), screen)
			}
		})
	}
}
