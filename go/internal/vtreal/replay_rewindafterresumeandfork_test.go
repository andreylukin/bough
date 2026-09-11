package vtreal

// Rewind after resume, then fork: a 3-turn session (seeded in a git
// $HOME with a real checkpoint per turn) is resumed, turns 3 and 2
// are /undo'ne, double esc rewinds to turn 1 and a new prompt is sent
// on the fork. Then bough quits and boots again: /sessions must nest
// the fork under the original, the undone files must stay reverted on
// disk, and resuming the original must show all three turns intact.
//
// Rewind moves the conversation only (plugins/ui/rewind.go); files
// travel with /undo, so "undone edits reverted" is checked on the
// /undo'ne turns, and the rewind is checked to leave them that way.

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

// rewindAfterResumeAndForkWorkspace makes $HOME a git repo and plays
// three turns into it, checkpointing before each:
//
//	turn 1: a.txt = "v1"
//	turn 2: a.txt = "v2", b.txt created
//	turn 3: c.txt created
//
// It writes the session file "rewind-root" and returns home and path.
func rewindAfterResumeAndForkWorkspace(t *testing.T) (home, session string) {
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
	snap := func() string {
		tree, err := history.Snapshot(home)
		if err != nil {
			t.Fatalf("snapshot: %v", err)
		}
		return tree
	}
	write(".gitignore", ".bough/\nbough.yml\n")
	write("a.txt", "v0\n")
	cp1 := snap()
	write("a.txt", "v1\n")
	cp2 := snap()
	write("a.txt", "v2\n")
	write("b.txt", "turn two\n")
	cp3 := snap()
	write("c.txt", "turn three\n")

	dir := filepath.Join(home, ".bough", "history")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	at := time.Now().Add(-time.Hour).UTC()
	turn := func(seq int64, text, cp, reply string, files ...string) []history.Entry {
		return []history.Entry{
			{Seq: seq, At: at, Kind: "input", Data: map[string]any{"text": text, "checkpoint": cp}},
			{Seq: seq + 1, At: at, Kind: "assistant", Data: map[string]any{"text": "```stop\n" + reply + "\n```"}},
			{Seq: seq + 2, At: at, Kind: "done", Data: map[string]any{"files": files}},
		}
	}
	entries := []history.Entry{{Seq: 1, At: at, Kind: "meta", Data: map[string]any{"cwd": home}}}
	entries = append(entries, turn(2, "turn one prompt", cp1, "T1-ANSWER", "a.txt")...)
	entries = append(entries, turn(5, "turn two prompt", cp2, "T2-ANSWER", "a.txt", "b.txt")...)
	entries = append(entries, turn(8, "turn three prompt", cp3, "T3-ANSWER", "c.txt")...)
	session = filepath.Join(dir, "rewind-root.jsonl")
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

// rewindAfterResumeAndForkConfig replays tape as the model with the
// history row pinned to session (resume it at boot).
func rewindAfterResumeAndForkConfig(t *testing.T, tape, session string) string {
	t.Helper()
	const row = "- id: history\n  plugin: history\n"
	yml := replayConfig(tape)
	if !strings.Contains(yml, row) {
		t.Fatalf("replayConfig no longer has a plain history row:\n%s", yml)
	}
	return strings.Replace(yml, row, row+"  config: {file: \""+session+"\"}\n", 1)
}

// rewindAfterResumeAndForkDisk asserts the post-undo state: turn 1's
// edit kept, turns 2 and 3 reverted.
func rewindAfterResumeAndForkDisk(t *testing.T, a *app, where string) {
	t.Helper()
	if got, _ := os.ReadFile(filepath.Join(a.home, "a.txt")); string(got) != "v1\n" {
		t.Errorf("%s: a.txt = %q, want turn 1's %q:\n%s", where, got, "v1\n", a.text())
	}
	for _, f := range []string{"b.txt", "c.txt"} {
		if _, err := os.Stat(filepath.Join(a.home, f)); !os.IsNotExist(err) {
			t.Errorf("%s: %s (written by an undone turn) is on disk: %v\n%s", where, f, err, a.text())
		}
	}
}

// rewindAfterResumeAndForkQuit double ctrl+c's and waits for exit.
func rewindAfterResumeAndForkQuit(t *testing.T, a *app) {
	t.Helper()
	a.key('c', uv.ModCtrl)
	a.waitFor("ctrl+c")
	a.key('c', uv.ModCtrl)
	done := make(chan error, 1)
	go func() { done <- a.term.Wait(a.cmd) }()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("exit: %v", err)
		}
	case <-time.After(8 * time.Second):
		t.Fatalf("process did not exit after two ctrl+c:\n%s", a.text())
	}
}

func TestRewindAfterResumeAndFork(t *testing.T) {
	t.Parallel()
	home, session := rewindAfterResumeAndForkWorkspace(t)
	tape := followUpTape(t) // the fork's new prompt gets REPLY-ALPHA
	a := undoStart(t, home, 100, 30, rewindAfterResumeAndForkConfig(t, tape, session))

	t.Run("ResumedThreeTurns", func(t *testing.T) {
		a.waitFor("T3-ANSWER")
		a.check("resumed")
	})

	t.Run("UndoTurnsThreeAndTwo", func(t *testing.T) {
		a.undoRun()
		a.waitFor("from turn 8")
		a.undoRun()
		a.waitFor("from turn 5")
		a.check("after two /undo")
		rewindAfterResumeAndForkDisk(t, a, "after /undo")
	})

	t.Run("RewindToTurnOne", func(t *testing.T) {
		a.key(uv.KeyEscape, 0)
		a.waitFor("press esc again to rewind")
		a.key(uv.KeyEscape, 0)
		a.waitFor("(current)")
		a.key(uv.KeyUp, 0) // turn three
		a.key(uv.KeyUp, 0) // turn two: back to before it
		a.key(uv.KeyEnter, 0)
		a.waitUntil(func(string) bool { return strings.HasPrefix(followUpComposer(a), "> turn two prompt") },
			"the rewound prompt in the composer")
		s := a.settled()
		if strings.Contains(s, "T2-ANSWER") || strings.Contains(s, "T3-ANSWER") || !strings.Contains(s, "T1-ANSWER") {
			t.Errorf("transcript after rewind to turn 1 is wrong:\n%s", s)
		}
		rewindAfterResumeAndForkDisk(t, a, "after rewind")
		a.check("after rewind")
	})

	t.Run("NewPromptOnFork", func(t *testing.T) {
		a.key(uv.KeyEscape, 0)
		a.waitFor("press esc again to clear the draft")
		a.key(uv.KeyEscape, 0)
		a.waitFor("say something")
		a.typeText("fork prompt delta")
		a.key(uv.KeyEnter, 0)
		followUpWaitDone(a, 2)
		a.waitFor("REPLY-ALPHA")
		if in := followUpKinds(a, "input"); strings.Join(in, "|") != "turn one prompt|fork prompt delta" {
			t.Errorf("fork inputs = %q:\n%s", in, a.text())
		}
		if paths, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*-f2.jsonl")); len(paths) != 1 {
			t.Errorf("want one fork file at seq 2, got %v", paths)
		}
		rewindAfterResumeAndForkDisk(t, a, "after fork turn")
		a.check("fork turn")
	})

	t.Run("OriginalFileUntouched", func(t *testing.T) {
		entries, err := history.Read(session)
		if err != nil {
			t.Fatal(err)
		}
		var in []string
		for _, e := range entries {
			if e.Kind == "input" {
				in = append(in, history.Prompt(e))
			}
		}
		if strings.Join(in, "|") != "turn one prompt|turn two prompt|turn three prompt" {
			t.Errorf("original inputs = %q", in)
		}
	})

	if t.Failed() {
		return
	}
	rewindAfterResumeAndForkQuit(t, a)

	// Boot again, unpinned: a fresh session over the same $HOME.
	b := undoStart(t, home, 100, 30, replayConfig(tape))
	b.typeText("/sessions")
	b.key(uv.KeyEnter, 0)
	b.waitFor("resume a session")

	t.Run("SessionsTreeNestsFork", func(t *testing.T) {
		// Rows carry the session's first prompt, which the fork shares:
		// the original is the unindented row, the fork the └─ one.
		s := b.settled()
		root, f := sessionTreeRow(b, "turn one prompt"), sessionTreeRow(b, "└─ ")
		if root < 0 || f < 0 || !strings.Contains(b.lines()[f], "turn one prompt") {
			t.Fatalf("original or fork row missing:\n%s", s)
		}
		if f != root+1 {
			t.Errorf("fork (row %d) not nested under the original (row %d):\n%s", f, root, s)
		}
		if strings.ContainsAny(b.lines()[root], "├└│") {
			t.Errorf("original row carries a connector:\n%s", s)
		}
	})

	t.Run("ResumeOriginalIntact", func(t *testing.T) {
		sessionTreeSelect(b, "turn one prompt") // first match: the original, above its fork
		b.key(uv.KeyEnter, 0)
		b.waitFor("resumed rewind-root")
		b.waitFor("T3-ANSWER")
		s := b.settled()
		for _, want := range []string{"turn two prompt", "T2-ANSWER", "turn three prompt"} {
			if !strings.Contains(s, want) {
				t.Errorf("original transcript lacks %q:\n%s", want, s)
			}
		}
		// Turn 1 has scrolled off the top: wheel up to it and back.
		for range 20 {
			b.term.SendMouse(uv.MouseWheelEvent{X: 5, Y: 3, Button: uv.MouseWheelUp})
		}
		up := b.settled()
		for _, want := range []string{"turn one prompt", "T1-ANSWER"} {
			if !strings.Contains(up, want) {
				t.Errorf("original transcript lacks %q at the top:\n%s", want, up)
			}
		}
		for range 40 {
			b.term.SendMouse(uv.MouseWheelEvent{X: 5, Y: 3, Button: uv.MouseWheelDown})
		}
		s = b.settled()
		if strings.Contains(s, "fork prompt delta") || strings.Contains(s, "REPLY-ALPHA") {
			t.Errorf("fork turn leaked into the original:\n%s", s)
		}
		rewindAfterResumeAndForkDisk(t, b, "after resuming the original")
		b.check("original resumed")
	})
}
