package vtreal

// The session's working directory is deleted between turns (a branch
// checkout, a `rm -rf` in another pane). The next turn's tools.bash
// must come back with a visible error instead of hanging, the "@"
// picker must show nothing rather than crash, and quitting must leave
// the alt screen with every turn in history ($HOME is elsewhere).
//
// Determinism: the tape's commands are `echo` and `pwd`; every wait is
// a waitDone with a timeout.

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// cwdDeletedUnderSessionStart boots bough with $HOME at home and the
// process cwd at dir, so removing dir leaves config and history alone.
func cwdDeletedUnderSessionStart(t *testing.T, home, dir, yml string) *app {
	t.Helper()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, 100, 30)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = dir
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", "PWD="+dir,
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

// cwdDeletedUnderSessionError: the failed block shows the shell's own
// complaint about the missing directory.
func cwdDeletedUnderSessionError(s string) bool {
	return strings.Contains(s, "✗ error") && strings.Contains(s, "getcwd")
}

func TestCwdDeletedUnderSession(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	dir := filepath.Join(t.TempDir(), "project")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "cwdgone-marker.txt"), []byte("x\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	block := func(cmd string) string {
		return fmt.Sprintf("```js\nconsole.log(tools.bash(%q))\n```", cmd)
	}
	tape := sighupTerminalCloseHistoryTape(t, home, [][2]any{
		{"meta", map[string]any{"cwd": dir}},
		{"input", map[string]any{"text": "first turn"}},
		{"assistant", map[string]any{"text": block("echo cwdgone-first")}},
		{"assistant", map[string]any{"text": "```stop\nfirst ok\n```"}},
		{"done", map[string]any{"text": ""}},
		{"input", map[string]any{"text": "where am i"}},
		{"assistant", map[string]any{"text": block("pwd")}},
		{"assistant", map[string]any{"text": "```stop\nsecond ok\n```"}},
		{"done", map[string]any{"text": ""}},
	})
	a := cwdDeletedUnderSessionStart(t, home, dir, jobsConfig(tape))

	a.typeText("first turn")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 30*time.Second) {
		t.Fatalf("first turn never finished:\n%s", a.text())
	}
	a.waitFor("cwdgone-first")
	a.check("first turn")

	if err := os.RemoveAll(dir); err != nil {
		t.Fatal(err)
	}

	t.Run("bash_reports_error", func(t *testing.T) {
		a.t = t
		a.typeText("where am i")
		a.key(uv.KeyEnter, 0)
		if !a.waitDone(2, 30*time.Second) {
			t.Fatalf("turn with a deleted cwd hung:\n%s", a.text())
		}
		s := a.settled()
		t.Logf("screen after pwd in a deleted cwd:\n%s", s)
		if !cwdDeletedUnderSessionError(s) {
			t.Errorf("tools.bash in a deleted cwd shows no error:\n%s", s)
		}
		a.check("deleted-cwd turn")
	})

	t.Run("at_picker_empty", func(t *testing.T) {
		a.t = t
		a.typeText("@")
		a.atPickerDraft("@ typed", "> @")
		s := a.settled()
		if names, _ := atPickerRows(s); len(names) != 0 {
			t.Errorf("picker lists files of a deleted cwd %q:\n%s", names, s)
		}
		a.check("@ in a deleted cwd")
		a.key(uv.KeyEscape, 0)
		for range 2 {
			a.key(uv.KeyBackspace, 0)
		}
	})

	a.t = t
	a.key('c', uv.ModCtrl)
	a.waitFor("ctrl+c")
	a.key('c', uv.ModCtrl)
	done := make(chan error, 1)
	go func() { done <- a.term.Wait(a.cmd) }()
	select {
	case err := <-done:
		if err != nil {
			t.Errorf("exit: %v", err)
		}
	case <-time.After(8 * time.Second):
		t.Fatalf("no exit after two ctrl+c:\n%s", a.text())
	}
	left := false
	for i := 0; i < 100 && !left; i++ {
		if left = !a.term.Snapshot().AltScreen; !left {
			time.Sleep(50 * time.Millisecond)
		}
	}
	if !left {
		t.Errorf("still in the alt screen after exit")
	}
	if n := a.doneCount(); n != 2 {
		t.Errorf("history has %d finished turns, want 2", n)
	}
}
