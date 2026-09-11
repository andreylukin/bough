package vtreal

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"
)

// editorStart boots the echo config like startCfg, with $EDITOR set to
// a shell script written into the fresh $HOME and TMPDIR pointed inside
// it, so the draft temp file is observable. VISUAL is cleared so the
// host's value cannot win.
func editorStart(t *testing.T, script string) *app {
	t.Helper()
	home := t.TempDir()
	ed := filepath.Join(home, "editor.sh")
	if err := os.WriteFile(ed, []byte("#!/bin/sh\n"+script), 0o755); err != nil {
		t.Fatal(err)
	}
	tmp := filepath.Join(home, "tmp")
	if err := os.Mkdir(tmp, 0o755); err != nil {
		t.Fatal(err)
	}
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(config), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, 100, 30)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", "VISUAL=", "EDITOR="+ed, "TMPDIR="+tmp,
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

// editorComposer returns the composer row, failing with the screen.
func editorComposer(a *app) string {
	a.t.Helper()
	ls := a.lines()
	i := composerRow(ls)
	if i < 0 {
		a.t.Fatalf("no composer on screen:\n%s", a.text())
	}
	return ls[i]
}

// editorNoTempLeft asserts the draft temp file was removed.
func editorNoTempLeft(a *app) {
	a.t.Helper()
	left, _ := filepath.Glob(filepath.Join(a.home, "tmp", "bough-draft-*"))
	if len(left) > 0 {
		a.t.Fatalf("draft temp files left behind: %v\nscreen:\n%s", left, a.text())
	}
}

// The editor prints a marker and blocks on a go-file, so the test can
// see the TUI suspended (main screen, marker visible) before releasing
// it; then it appends to the draft and exits 0.
func TestEditorAppendsToDraft(t *testing.T) {
	t.Parallel()
	a := editorStart(t, `printf 'EDITOR-RUNNING'
while [ ! -f "$HOME/go" ]; do sleep 0.05; done
printf ' plus edits' >> "$1"
`)
	a.typeText("hello draft")
	a.waitFor("hello draft")
	a.key('g', uv.ModCtrl)

	a.waitFor("EDITOR-RUNNING")
	if a.term.Snapshot().AltScreen {
		t.Fatalf("editor running but TUI still on the alt screen\nscreen:\n%s", a.text())
	}
	if err := os.WriteFile(filepath.Join(a.home, "go"), nil, 0o644); err != nil {
		t.Fatal(err)
	}

	a.waitFor("hello draft plus edits")
	if !a.term.Snapshot().AltScreen {
		t.Fatalf("alt screen not restored after the editor exited\nscreen:\n%s", a.text())
	}
	a.settled()
	if row := editorComposer(a); !strings.Contains(row, "hello draft plus edits") {
		t.Fatalf("composer row %q lacks the edited draft\nscreen:\n%s", row, a.text())
	}
	if strings.Contains(a.text(), "EDITOR-RUNNING") {
		t.Fatalf("editor output leaked onto the restored screen\nscreen:\n%s", a.text())
	}
	editorNoTempLeft(a)
}

func TestEditorFailureKeepsDraft(t *testing.T) {
	t.Parallel()
	a := editorStart(t, `printf 'clobbered' > "$1"
exit 1
`)
	a.typeText("keep me")
	a.waitFor("keep me")
	a.key('g', uv.ModCtrl)

	a.waitFor("draft kept")
	if !a.term.Snapshot().AltScreen {
		t.Fatalf("alt screen not restored after the failed editor\nscreen:\n%s", a.text())
	}
	if !strings.Contains(a.text(), "exit status 1") {
		t.Fatalf("notice does not name the exit status\nscreen:\n%s", a.text())
	}
	if row := editorComposer(a); !strings.Contains(row, "keep me") || strings.Contains(row, "clobbered") {
		t.Fatalf("composer row %q: draft not kept intact\nscreen:\n%s", row, a.text())
	}
	editorNoTempLeft(a)
}
