package vtreal

// /undo on a real PTY. A replayed tape runs nothing, so the live
// done entry lists no files; instead the test builds the workspace
// itself: a git repo in $HOME, a real checkpoint tree, the files the
// "turn" wrote, and a session file whose input/done entries name them.
// bough resumes that session (history.file) and /undo must put exactly
// those files back and say so.

import (
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// undoStart is startCfg with a HOME the test has already populated.
func undoStart(t *testing.T, home string, cols, rows int, yml string) *app {
	t.Helper()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, cols, rows)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=",
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: cols, rows: rows, home: home}
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

// undoWorkspace makes $HOME a git repo holding a.txt="before",
// checkpoints it, then plays the turn: a.txt rewritten, new.txt
// created, keep.txt edited but NOT listed. It writes the session file
// and returns home and the session path.
func undoWorkspace(t *testing.T) (home, session string) {
	t.Helper()
	home, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	if out, err := exec.Command("git", "-C", home, "init", "-q").CombinedOutput(); err != nil {
		t.Fatalf("git init: %v %s", err, out)
	}
	write := func(name, s string) {
		if err := os.WriteFile(filepath.Join(home, name), []byte(s), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	// Keep bough's own state out of the checkpoint.
	write(".gitignore", ".bough/\nbough.yml\n")
	write("a.txt", "before\n")
	write("keep.txt", "before\n")
	// history.Snapshot fails on a repo with no index yet (its empty
	// temp index is "smaller than expected"), so seed one.
	if out, err := exec.Command("git", "-C", home, "add", ".gitignore").CombinedOutput(); err != nil {
		t.Fatalf("git add: %v %s", err, out)
	}
	tree, err := history.Snapshot(home)
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	write("a.txt", "after\n")
	write("new.txt", "created by the turn\n")
	write("keep.txt", "edited by hand\n")

	dir := filepath.Join(home, ".bough", "history")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	at := time.Date(2026, 9, 10, 10, 0, 0, 0, time.UTC)
	entries := []history.Entry{
		{Seq: 1, At: at, Kind: "meta", Data: map[string]any{"cwd": home}},
		{Seq: 2, At: at, Kind: "input", Data: map[string]any{"text": "rewrite a.txt and add new.txt", "checkpoint": tree}},
		{Seq: 3, At: at, Kind: "assistant", Data: map[string]any{"text": "```stop\nDone.\n```"}},
		{Seq: 4, At: at, Kind: "done", Data: map[string]any{"files": []string{"a.txt", "new.txt"}}},
	}
	session = filepath.Join(dir, "undo-session.jsonl")
	var b strings.Builder
	for _, e := range entries {
		line, err := json.Marshal(e)
		if err != nil {
			t.Fatal(err)
		}
		b.Write(line)
		b.WriteByte('\n')
	}
	if err := os.WriteFile(session, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return home, session
}

// undoConfig is replayConfig with the history row pinned to session.
func undoConfig(t *testing.T, session string) string {
	t.Helper()
	const row = "- id: history\n  plugin: history\n"
	yml := replayConfig(session)
	if !strings.Contains(yml, row) {
		t.Fatalf("replayConfig no longer has a plain history row:\n%s", yml)
	}
	return strings.Replace(yml, row, row+"  config: {file: \""+session+"\"}\n", 1)
}

func (a *app) undoRun() {
	a.typeText("/undo")
	a.key(uv.KeyEnter, 0)
}

func TestUndoRevertsListedFiles(t *testing.T) {
	t.Parallel()
	home, session := undoWorkspace(t)
	a := undoStart(t, home, 100, 30, undoConfig(t, session))

	t.Run("TestUndoResumedTurnListed", func(t *testing.T) {
		a.waitFor("rewrite a.txt and add new.txt")
		a.check("resumed")
	})

	t.Run("TestUndoListing", func(t *testing.T) {
		a.undoRun()
		a.waitFor("reverted 2 files from turn 2")
		s := a.settled()
		for _, want := range []string{"a.txt", "new.txt"} {
			if !strings.Contains(s, want) {
				t.Errorf("undo listing lacks %s:\n%s", want, s)
			}
		}
		if strings.Contains(s, "keep.txt") {
			t.Errorf("undo listed a file the turn never wrote:\n%s", s)
		}
		a.check("after /undo")
	})

	t.Run("TestUndoFilesOnDisk", func(t *testing.T) {
		if got, _ := os.ReadFile(filepath.Join(home, "a.txt")); string(got) != "before\n" {
			t.Errorf("a.txt = %q, want the checkpoint's %q:\n%s", got, "before\n", a.text())
		}
		if _, err := os.Stat(filepath.Join(home, "new.txt")); !os.IsNotExist(err) {
			t.Errorf("new.txt (absent from the checkpoint) survived /undo: %v\n%s", err, a.text())
		}
		if got, _ := os.ReadFile(filepath.Join(home, "keep.txt")); string(got) != "edited by hand\n" {
			t.Errorf("keep.txt (not listed) was touched: %q\n%s", got, a.text())
		}
	})

	t.Run("TestUndoRecordEntry", func(t *testing.T) {
		entries, err := history.Read(session)
		if err != nil {
			t.Fatalf("%v\n%s", err, a.text())
		}
		var undo *history.Entry
		for i := range entries {
			if entries[i].Kind == "undo" {
				undo = &entries[i]
			}
		}
		if undo == nil {
			t.Fatalf("no undo entry in %s:\n%s", session, a.text())
		}
		if seq, _ := undo.Data["seq_of_turn"].(float64); seq != 2 {
			t.Errorf("undo seq_of_turn = %v, want 2:\n%s", undo.Data["seq_of_turn"], a.text())
		}
		files, _ := undo.Data["files"].([]any)
		if len(files) != 2 || files[0] != "a.txt" || files[1] != "new.txt" {
			t.Errorf("undo files = %v, want [a.txt new.txt]:\n%s", undo.Data["files"], a.text())
		}
	})

	t.Run("TestUndoNothingLeft", func(t *testing.T) {
		a.undoRun()
		a.waitFor("nothing to undo")
		a.check("second /undo")
	})
}
