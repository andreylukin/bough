package vtreal

// Detach and reattach under tmux while a turn streams. A real tmux
// client is attached on its own PTY, detached while the reply is still
// arriving (delay_ms), the turn finishes with nobody watching, and a new
// client attaches at a different size. What must hold on reattach:
//
//   - a full redraw at the new size: bar and composer on the last rows,
//     no row wider than the pane, no running spinner left from before
//   - the done notice sent exactly once: bough has no OSC 9, its done
//     notice is the "✓ " tab title (plugins/ui/tabtitle.go), counted in
//     the raw pane output via pipe-pane
//   - mouse reporting still on in the pane (1002/1003 + 1006)

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/creack/pty"
)

// tmuxDetachReattachMidStreamClient is one attached tmux client on a PTY.
type tmuxDetachReattachMidStreamClient struct {
	cmd *exec.Cmd
	f   *os.File
	mu  sync.Mutex
	out []byte
}

func tmuxDetachReattachMidStreamAttach(t *testing.T, tm *tmuxApp, cols, rows int) *tmuxDetachReattachMidStreamClient {
	t.Helper()
	cmd := exec.Command("tmux", "-L", tm.sock, "attach", "-t", "0")
	cmd.Env = append(os.Environ(), "TERM=xterm-256color", "TMUX=") // never nest inside the runner's tmux
	f, err := pty.StartWithSize(cmd, &pty.Winsize{Cols: uint16(cols), Rows: uint16(rows)})
	if err != nil {
		t.Fatal(err)
	}
	c := &tmuxDetachReattachMidStreamClient{cmd: cmd, f: f}
	done := make(chan struct{})
	go func() {
		defer close(done)
		buf := make([]byte, 4096)
		for {
			n, err := f.Read(buf)
			c.mu.Lock()
			c.out = append(c.out, buf[:n]...)
			c.mu.Unlock()
			if err != nil {
				return
			}
		}
	}()
	t.Cleanup(func() {
		_ = cmd.Process.Kill()
		_, _ = cmd.Process.Wait()
		_ = f.Close()
		<-done
	})
	tm.waitUntil(func(string) bool {
		return strings.TrimSpace(tm.run("list-clients", "-t", "0")) != ""
	}, "a client to attach")
	return c
}

// tmuxDetachReattachMidStreamPaneSize is the pane's "cols rows".
func tmuxDetachReattachMidStreamPaneSize(tm *tmuxApp) string {
	return strings.TrimSpace(tm.run("display", "-p", "-t", "0", "#{pane_width} #{pane_height}"))
}

func tmuxDetachReattachMidStreamTitle(tm *tmuxApp) string {
	return strings.TrimSpace(tm.run("display", "-p", "-t", "0", "#{pane_title}"))
}

func TestTmuxDetachReattachMidStream(t *testing.T) {
	t.Parallel()
	tape := resizeTmuxTape(t)
	yml := strings.Replace(replayConfig(tape),
		fmt.Sprintf("config: {file: %q}", tape),
		fmt.Sprintf("config: {file: %q, delay_ms: 400}", tape), 1)
	tm, home := resizeTmuxStart(t, 100, 30, yml)
	// window-size latest: the window follows the newest client, as a
	// person reattaching from another terminal would expect.
	tm.run("set-option", "-g", "window-size", "latest")
	tm.run("set-option", "-g", "status", "off") // the pane gets every client row
	raw := filepath.Join(home, "pane.raw")
	tm.run("pipe-pane", "-O", "-t", "0", "cat >> "+raw)

	tmuxDetachReattachMidStreamAttach(t, tm, 100, 30)
	tm.waitUntil(func(string) bool { return tmuxDetachReattachMidStreamPaneSize(tm) == "100 30" }, "pane at 100x30")

	resizeTmuxSend(tm, "run the tests and show me a very long separator line")
	// The turn is running: the "● " title (the welcome banner has a ●
	// of its own, so the screen cannot tell).
	tm.waitUntil(func(string) bool { return strings.HasPrefix(tmuxDetachReattachMidStreamTitle(tm), "● ") }, "the running title")
	tm.run("detach-client", "-s", "0")
	tm.waitUntil(func(string) bool {
		out, _ := exec.Command("tmux", "-L", tm.sock, "list-clients").Output()
		return strings.TrimSpace(string(out)) == ""
	}, "the client to detach")
	if resizeTmuxDone(home) > 0 {
		t.Fatalf("the turn finished before the detach, so it was not mid-stream; raise delay_ms:\n%s", resizeTmuxScreen(tm))
	}
	resizeTmuxWaitDone(t, tm, home, 1)
	time.Sleep(300 * time.Millisecond) // let the done frame land while detached

	tmuxDetachReattachMidStreamAttach(t, tm, 70, 20)
	tm.waitUntil(func(string) bool { return tmuxDetachReattachMidStreamPaneSize(tm) == "70 20" }, "pane at 70x20")

	t.Run("redraw", func(t *testing.T) {
		resizeTmuxCheck(t, tm, "reattached @ 70x20", 70)
		s := resizeTmuxScreen(tm)
		if !strings.Contains(s, "Two Go files") && !strings.Contains(s, "separator") {
			t.Errorf("the finished turn is not on the redrawn screen:\n%s", s)
		}
		if n := len(strings.Split(s, "\n")); n > 20 {
			t.Errorf("capture has %d rows in a 20-row pane:\n%s", n, s)
		}
	})

	t.Run("done notice once", func(t *testing.T) {
		if got := tmuxDetachReattachMidStreamTitle(tm); !strings.HasPrefix(got, "✓ ") {
			t.Errorf("pane title %q, want the ✓ done title", got)
		}
		time.Sleep(300 * time.Millisecond)
		b, err := os.ReadFile(raw)
		if err != nil {
			t.Fatal(err)
		}
		s := string(b)
		if n := strings.Count(s, "\x1b]2;✓ ") + strings.Count(s, "\x1b]0;✓ "); n != 1 {
			t.Errorf("done title sent %d times, want 1", n)
		}
		if n := strings.Count(s, "\x1b]9;"); n > 1 {
			t.Errorf("OSC 9 sent %d times, want at most 1", n)
		}
	})

	t.Run("mouse on", func(t *testing.T) {
		got := strings.Fields(tm.run("display", "-p", "-t", "0", "#{mouse_any_flag} #{mouse_button_flag} #{mouse_sgr_flag}"))
		if len(got) != 3 || (got[0] != "1" && got[1] != "1") || got[2] != "1" {
			t.Errorf("pane mouse flags any/button/sgr = %v, want motion + SGR on", got)
		}
	})
}
