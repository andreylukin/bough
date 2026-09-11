package vtreal

// /undo after esc cancelled a block mid-edit. The tape's one block
// writes a.txt, runs a sentinel sleep, then writes b.txt; esc lands
// during the sleep. The cancelled turn must still be undoable: a.txt
// back to its original bytes, b.txt never created, and the undo
// listing naming only a.txt. Codemode and tools-basic are real (see
// undoWhileStreamingConfig), $HOME is a temp git repo.

import (
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const undoAfterCancelledEditToolOrig = "original bytes\x00\n\tline two\n"

func undoAfterCancelledEditToolTape(t *testing.T, dir string) string {
	t.Helper()
	block := "```js\ntools.write(\"a.txt\", \"UACE edited\\n\")\n" +
		"tools.bash(\"sleep 5 # UACESLEEP\")\n" +
		"tools.write(\"b.txt\", \"UACE b\\n\")\nconsole.log(\"UACEDONE\")\n```"
	at := "2026-09-11T10:00:00Z"
	entries := []map[string]any{
		{"seq": 1, "at": at, "kind": "meta", "data": map[string]any{"cwd": dir}},
		{"seq": 2, "at": at, "kind": "input", "data": map[string]any{"text": "edit then sleep"}},
		{"seq": 3, "at": at, "kind": "assistant", "data": map[string]any{"text": block}},
		{"seq": 4, "at": at, "kind": "result", "data": map[string]any{"text": "UACEDONE"}},
		{"seq": 5, "at": at, "kind": "assistant", "data": map[string]any{"text": "```stop\nUACEEND\n```"}},
		{"seq": 6, "at": at, "kind": "done", "data": map[string]any{"text": ""}},
	}
	var b strings.Builder
	for _, e := range entries {
		line, err := json.Marshal(e)
		if err != nil {
			t.Fatal(err)
		}
		b.Write(line)
		b.WriteByte('\n')
	}
	p := filepath.Join(dir, "uace-tape.jsonl")
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

func undoAfterCancelledEditToolStart(t *testing.T) *app {
	t.Helper()
	home, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	if out, err := exec.Command("git", "-C", home, "init", "-q").CombinedOutput(); err != nil {
		t.Fatalf("git init: %v %s", err, out)
	}
	for name, s := range map[string]string{
		".gitignore": ".bough/\nbough.yml\nuace-tape.jsonl\n",
		"a.txt":      undoAfterCancelledEditToolOrig,
	} {
		if err := os.WriteFile(filepath.Join(home, name), []byte(s), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	tape := undoAfterCancelledEditToolTape(t, home)
	return undoStart(t, home, 100, 30, undoWhileStreamingConfig(tape, 5))
}

func TestUndoAfterCancelledEditTool(t *testing.T) {
	t.Parallel()
	a := undoAfterCancelledEditToolStart(t)
	a.typeText("edit then sleep")
	a.key(uv.KeyEnter, 0)
	deadline := time.Now().Add(30 * time.Second)
	for undoWhileStreamingFile(t, a.home, "a.txt") != "UACE edited\n" {
		if time.Now().After(deadline) {
			t.Fatalf("the block never wrote a.txt:\n%s", a.text())
		}
		time.Sleep(20 * time.Millisecond)
	}
	time.Sleep(300 * time.Millisecond) // inside the 5 s sleep
	escAt := time.Now()
	a.key(uv.KeyEsc, 0)
	a.waitFor("■ cancelled")
	if !a.waitDone(1, 10*time.Second) {
		t.Fatalf("no done/cancelled entry after esc:\n%s", a.text())
	}
	if d := time.Since(escAt); d > 4*time.Second {
		t.Errorf("esc took %v to cancel the sleep", d)
	}

	t.Run("CancelStoppedTheBlock", func(t *testing.T) {
		time.Sleep(6 * time.Second) // past where the sleep would have ended
		if got := undoWhileStreamingFile(t, a.home, "b.txt"); got != "<absent>" {
			t.Errorf("b.txt written after esc: %q\n%s", got, a.text())
		}
		if s := a.settled(); strings.Contains(s, "UACEEND") {
			t.Errorf("the turn continued after esc:\n%s", s)
		}
	})

	t.Run("UndoRestoresOnlyA", func(t *testing.T) {
		a.undoRun()
		a.waitFor("reverted 1 file from turn")
		s := a.settled()
		if !strings.Contains(s, "  a.txt") {
			t.Errorf("undo listing lacks a.txt:\n%s", s)
		}
		if strings.Contains(s, "b.txt") {
			t.Errorf("undo listing names b.txt:\n%s", s)
		}
		got, err := os.ReadFile(filepath.Join(a.home, "a.txt"))
		if err != nil || string(got) != undoAfterCancelledEditToolOrig {
			t.Errorf("a.txt = %q (%v), want %q:\n%s", got, err, undoAfterCancelledEditToolOrig, s)
		}
		if _, err := os.Stat(filepath.Join(a.home, "b.txt")); !os.IsNotExist(err) {
			t.Errorf("b.txt exists after /undo: %v", err)
		}
		a.check("after /undo")
	})
}
