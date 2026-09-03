package vtreal

// A tmux-backed session for the few checks x/vt gets wrong (resize).
// Each test gets its own tmux server (-L socket), so tests stay parallel.

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

type tmuxApp struct {
	t    *testing.T
	sock string
}

func startTmux(t *testing.T, cols, rows int) *tmuxApp {
	t.Helper()
	if _, err := exec.LookPath("tmux"); err != nil {
		t.Skip("tmux not installed")
	}
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(config), 0o644); err != nil {
		t.Fatal(err)
	}
	tm := &tmuxApp{t: t, sock: fmt.Sprintf("vtreal-%d-%d", os.Getpid(), time.Now().UnixNano())}
	shell := fmt.Sprintf("cd %s && HOME=%s TERM=xterm-256color %s -config %s", home, home, bin, cfg)
	tm.run("new-session", "-d", "-x", fmt.Sprint(cols), "-y", fmt.Sprint(rows), shell)
	t.Cleanup(func() { _ = exec.Command("tmux", "-L", tm.sock, "kill-server").Run() })
	tm.waitFor("say something")
	return tm
}

func (tm *tmuxApp) run(args ...string) string {
	tm.t.Helper()
	out, err := exec.Command("tmux", append([]string{"-L", tm.sock}, args...)...).CombinedOutput()
	if err != nil {
		tm.t.Fatalf("tmux %v: %v\n%s", args, err, out)
	}
	return string(out)
}

func (tm *tmuxApp) keys(keys ...string) { tm.run(append([]string{"send-keys", "-t", "0"}, keys...)...) }

func (tm *tmuxApp) resize(cols, rows int) {
	tm.run("resize-window", "-t", "0", "-x", fmt.Sprint(cols), "-y", fmt.Sprint(rows))
}

func (tm *tmuxApp) screen() string {
	out := tm.run("capture-pane", "-p", "-t", "0")
	ls := strings.Split(strings.TrimRight(out, "\n"), "\n")
	for i := range ls {
		ls[i] = strings.TrimRight(ls[i], " ")
	}
	return strings.Join(ls, "\n")
}

// settled waits until two captures 80ms apart match, then returns it.
func (tm *tmuxApp) settled() string {
	prev := tm.screen()
	for range 40 {
		time.Sleep(80 * time.Millisecond)
		cur := tm.screen()
		if cur == prev {
			return cur
		}
		prev = cur
	}
	return prev
}

func (tm *tmuxApp) waitFor(substr string) {
	tm.t.Helper()
	tm.waitUntil(func(s string) bool { return strings.Contains(s, substr) }, "screen to contain "+substr)
}

func (tm *tmuxApp) waitUntil(pred func(string) bool, what string) {
	tm.t.Helper()
	// Generous: under a full -race run a fresh tmux server plus a bough
	// boot can take well over the emulator suite's 8 s.
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		if pred(tm.screen()) {
			return
		}
		time.Sleep(50 * time.Millisecond)
	}
	tm.t.Fatalf("tmux: timed out waiting for %s\nscreen:\n%s", what, tm.screen())
}
