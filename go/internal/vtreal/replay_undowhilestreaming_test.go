package vtreal

// /undo (and its leader chord, ctrl+x u) pressed while a turn is still
// streaming. The replay llm answers from a tape; codemode and
// tools-basic are real, so the tape's first block really writes files
// into a git-repo $HOME (the checkpoint needs one). The second reply
// streams slowly. Mid-stream, undo must be refused with a visible
// message and leave the tree exactly as the block wrote it; once the
// turn ends (esc or done), undo reverts exactly the written files.

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

// undoWhileStreamingTape writes a tape whose turn writes a.txt and
// new.txt through a real block, then streams a long reply.
func undoWhileStreamingTape(t *testing.T, dir string) string {
	t.Helper()
	long := "UWSSTART " + strings.Repeat("filler ", 300) + "UWSEND\n\n```stop\nUWSEND\n```"
	block := "```js\ntools.write(\"a.txt\", \"after\\n\")\ntools.write(\"new.txt\", \"created\\n\")\nconsole.log(\"WROTE\")\n```"
	at := "2026-09-11T10:00:00Z"
	entries := []map[string]any{
		{"seq": 1, "at": at, "kind": "meta", "data": map[string]any{"cwd": dir}},
		{"seq": 2, "at": at, "kind": "input", "data": map[string]any{"text": "write then talk"}},
		{"seq": 3, "at": at, "kind": "assistant", "data": map[string]any{"text": block}},
		{"seq": 4, "at": at, "kind": "result", "data": map[string]any{"text": "WROTE"}},
		{"seq": 5, "at": at, "kind": "assistant", "data": map[string]any{"text": long}},
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
	p := filepath.Join(dir, "uws-tape.jsonl")
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// undoWhileStreamingConfig is the replay llm (slowed) with a real
// codemode and tools-basic, so the block's writes land on disk.
func undoWhileStreamingConfig(tape string, delayMS int) string {
	cfg := cancelConfig(tape, delayMS)
	out := strings.Replace(cfg,
		"- id: codemode\n  plugin: replay\n  config: {file: \""+tape+"\", provide: codemode}\n",
		"- id: codemode\n  plugin: codemode\n- id: tools\n  plugin: tools-basic\n", 1)
	if out == cfg {
		panic("undoWhileStreamingConfig: replayConfig's codemode row changed shape")
	}
	return out
}

func undoWhileStreamingFile(t *testing.T, home, name string) string {
	t.Helper()
	b, err := os.ReadFile(filepath.Join(home, name))
	if os.IsNotExist(err) {
		return "<absent>"
	}
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}

// undoWhileStreamingTree asserts the working tree file by file.
func undoWhileStreamingTree(t *testing.T, a *app, where string, want map[string]string) {
	t.Helper()
	for name, w := range want {
		if got := undoWhileStreamingFile(t, a.home, name); got != w {
			t.Errorf("%s: %s = %q, want %q:\n%s", where, name, got, w, a.text())
		}
	}
}

// undoWhileStreamingStart boots bough in a git-repo $HOME holding
// a.txt="before\n" and keep.txt, with the tape beside them.
func undoWhileStreamingStart(t *testing.T, delayMS int) *app {
	t.Helper()
	home, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	if out, err := exec.Command("git", "-C", home, "init", "-q").CombinedOutput(); err != nil {
		t.Fatalf("git init: %v %s", err, out)
	}
	for name, s := range map[string]string{
		".gitignore": ".bough/\nbough.yml\nuws-tape.jsonl\n",
		"a.txt":      "before\n",
		"keep.txt":   "keep\n",
	} {
		if err := os.WriteFile(filepath.Join(home, name), []byte(s), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	tape := undoWhileStreamingTape(t, home)
	return undoStart(t, home, 100, 30, undoWhileStreamingConfig(tape, delayMS))
}

var (
	undoWhileStreamingWritten = map[string]string{"a.txt": "after\n", "new.txt": "created\n", "keep.txt": "keep\n"}
	undoWhileStreamingBefore  = map[string]string{"a.txt": "before\n", "new.txt": "<absent>", "keep.txt": "keep\n"}
)

// undoWhileStreamingMidTurn sends the prompt and returns once the
// block's files exist and the long reply has started streaming.
func undoWhileStreamingMidTurn(t *testing.T, a *app) {
	t.Helper()
	a.typeText("write then talk")
	a.key(uv.KeyEnter, 0)
	deadline := time.Now().Add(30 * time.Second)
	for undoWhileStreamingFile(t, a.home, "new.txt") != "created\n" {
		if time.Now().After(deadline) {
			t.Fatalf("the block never wrote new.txt:\n%s", a.text())
		}
		time.Sleep(20 * time.Millisecond)
	}
	a.waitFor("UWSSTART")
}

func TestUndoWhileStreaming(t *testing.T) {
	t.Parallel()

	t.Run("SlashUndoRefusedMidStream", func(t *testing.T) {
		t.Parallel()
		a := undoWhileStreamingStart(t, 100)
		undoWhileStreamingMidTurn(t, a)
		a.undoRun()
		a.waitFor("is still running")
		if !cancelSpinner.MatchString(a.text()) {
			t.Errorf("the turn stopped because of /undo:\n%s", a.text())
		}
		undoWhileStreamingTree(t, a, "after refused /undo", undoWhileStreamingWritten)
		a.check("refused /undo")
	})

	t.Run("ChordUndoRefusedMidStream", func(t *testing.T) {
		t.Parallel()
		a := undoWhileStreamingStart(t, 100)
		undoWhileStreamingMidTurn(t, a)
		a.key('x', uv.ModCtrl)
		a.key('u', 0)
		a.waitFor("is still running")
		undoWhileStreamingTree(t, a, "after refused chord undo", undoWhileStreamingWritten)
		a.check("refused chord undo")
	})

	t.Run("UndoAfterEscCancel", func(t *testing.T) {
		t.Parallel()
		a := undoWhileStreamingStart(t, 100)
		undoWhileStreamingMidTurn(t, a)
		a.key(uv.KeyEsc, 0)
		a.waitFor("■ cancelled")
		if !a.waitDone(1, 10*time.Second) {
			t.Fatalf("no done/cancelled entry after esc:\n%s", a.text())
		}
		undoWhileStreamingTree(t, a, "after esc", undoWhileStreamingWritten)
		a.undoRun()
		a.waitFor("reverted 2 files from turn")
		undoWhileStreamingTree(t, a, "after /undo", undoWhileStreamingBefore)
		a.check("after /undo")
	})

	t.Run("UndoAfterStreamFinishes", func(t *testing.T) {
		t.Parallel()
		a := undoWhileStreamingStart(t, 5)
		undoWhileStreamingMidTurn(t, a)
		a.undoRun() // refused or, if the stream already ended, a revert
		if !a.waitDone(1, 60*time.Second) {
			t.Fatalf("turn never finished:\n%s", a.text())
		}
		s := a.settled()
		if strings.Contains(s, "reverted") {
			// The stream beat the keystroke: the one revert must be exact.
			undoWhileStreamingTree(t, a, "early /undo", undoWhileStreamingBefore)
			return
		}
		undoWhileStreamingTree(t, a, "turn done", undoWhileStreamingWritten)
		a.undoRun()
		a.waitFor("reverted 2 files from turn")
		undoWhileStreamingTree(t, a, "after /undo", undoWhileStreamingBefore)
		a.check("after /undo")
	})
}
