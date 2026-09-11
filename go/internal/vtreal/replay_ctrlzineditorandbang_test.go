package vtreal

// External editor that exits nonzero after the pane was resized while
// it ran (the SIGTSTP suspend path has its own suite). Under tmux: the
// $EDITOR script prints a marker and blocks on a fifo, the test resizes
// the window, releases the fifo, and the script exits 3. The TUI must
// come back on the alt screen, laid out for the new size, draft kept.

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"
)

// ctrlzInEditorAndBangStart is startTmux with $EDITOR pointed at a
// fifo-blocking script in the run's $HOME; returns the app and $HOME.
func ctrlzInEditorAndBangStart(t *testing.T, cols, rows int) (*tmuxApp, string) {
	t.Helper()
	if _, err := exec.LookPath("tmux"); err != nil {
		t.Skip("tmux not installed")
	}
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(config), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := syscall.Mkfifo(filepath.Join(home, "fifo"), 0o600); err != nil {
		t.Fatal(err)
	}
	ed := filepath.Join(home, "editor.sh")
	script := "#!/bin/sh\nprintf 'EDITOR-RUNNING'\nread x < \"$HOME/fifo\"\nprintf 'clobbered' > \"$1\"\nexit 3\n"
	if err := os.WriteFile(ed, []byte(script), 0o755); err != nil {
		t.Fatal(err)
	}
	tm := &tmuxApp{t: t, sock: fmt.Sprintf("vtedbang-%d-%d", os.Getpid(), time.Now().UnixNano())}
	shell := fmt.Sprintf("cd %s && HOME=%s TERM=xterm-256color VISUAL= EDITOR=%s %s -config %s", home, home, ed, bin, cfg)
	tm.run("new-session", "-d", "-x", fmt.Sprint(cols), "-y", fmt.Sprint(rows), shell)
	t.Cleanup(func() {
		_ = exec.Command("tmux", "-L", tm.sock, "kill-server").Run()
		dir := os.Getenv("TMUX_TMPDIR")
		if dir == "" {
			dir = "/tmp"
		}
		_ = os.Remove(filepath.Join(dir, fmt.Sprintf("tmux-%d", os.Getuid()), tm.sock))
	})
	tm.waitFor("say something")
	return tm, home
}

// ctrlzInEditorAndBangAlt reports whether the pane is on the alt screen.
func ctrlzInEditorAndBangAlt(tm *tmuxApp) bool {
	return strings.TrimSpace(tm.run("display-message", "-p", "-t", "0", "#{alternate_on}")) == "1"
}

func TestCtrlzInEditorAndBangEditorFailsAfterResize(t *testing.T) {
	t.Parallel()
	tm, home := ctrlzInEditorAndBangStart(t, 100, 30)
	tm.keys("-l", "keep this draft")
	tm.waitFor("keep this draft")
	tm.keys("C-g")
	tm.waitFor("EDITOR-RUNNING")
	if ctrlzInEditorAndBangAlt(tm) {
		t.Fatalf("editor running but pane still on the alt screen\nscreen:\n%s", tm.screen())
	}

	tm.resize(60, 16)
	// Opening the fifo for write blocks until the script reads it.
	f, err := os.OpenFile(filepath.Join(home, "fifo"), os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	_, _ = f.WriteString("go\n")
	_ = f.Close()

	tm.waitFor("draft kept")
	scr := tm.settled()
	if !ctrlzInEditorAndBangAlt(tm) {
		t.Fatalf("alt screen not restored after the failed editor\nscreen:\n%s", scr)
	}
	if !strings.Contains(scr, "exit status 3") {
		t.Fatalf("notice does not name exit status 3\nscreen:\n%s", scr)
	}
	if !strings.Contains(scr, "keep this draft") || strings.Contains(scr, "clobbered") {
		t.Fatalf("draft not kept intact\nscreen:\n%s", scr)
	}
	if strings.Contains(scr, "EDITOR-RUNNING") {
		t.Fatalf("editor output leaked onto the restored screen\nscreen:\n%s", scr)
	}
	ls := strings.Split(scr, "\n")
	if len(ls) != 16 {
		t.Fatalf("captured %d rows, want 16\nscreen:\n%s", len(ls), scr)
	}
	for i, l := range ls {
		if w := len([]rune(l)); w > 60 {
			t.Fatalf("row %d is %d cells wide in a 60-col pane\nscreen:\n%s", i, w, scr)
		}
	}
	if strings.TrimSpace(ls[len(ls)-1]) == "" {
		t.Fatalf("last row blank: TUI not redrawn at the new size\nscreen:\n%s", scr)
	}
	// The TUI still takes input at the new size.
	tm.keys("-l", " more")
	tm.waitFor("keep this draft more")
}
