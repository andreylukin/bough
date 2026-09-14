package vtreal

// Fork then /undo, file isolation: turn 1 of the original session
// writes a.txt (v0 -> v1); the session is forked at turn 1 and the
// fork's own turn writes a.txt again (v1 -> v2). /undo in the fork
// must put a.txt back to turn 1's v1 — not v0 — and leave the
// original session file, its checkpoint ref, and its own /undo
// untouched. A replayed turn writes nothing, so the edits and the
// checkpoints (real git trees pinned under each session's ref) are
// seeded here, the fork with history.Fork itself. Both are project
// sessions (only those take checkpoints), each with its own worktree of
// one repo, so the refs are shared and the files are not.

import (
	"bytes"
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

// forkThenUndoFileIsolationWorkspace defines project vt on one repo
// with the original session "fui-root" (turn 2: a.txt v0 -> v1) and its
// fork "fui-fork" (forked at turn 2, then turn 5: a.txt v1 -> v2), each
// checkpoint pinned under its own session's turn ref. The root
// worktree's a.txt is v1, the fork's v2. repo is where the refs live.
func forkThenUndoFileIsolationWorkspace(t *testing.T) (home, repo, root, fork string) {
	t.Helper()
	home, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	repo = projectRepo(t, home, map[string]string{"a.txt": "v0\n"})
	rootWT, forkWT := projectWorktree(home, "fui-root"), projectWorktree(home, "fui-fork")
	for s, wt := range map[string]string{"fui-root": rootWT, "fui-fork": forkWT} {
		// The orb reuses a worktree that already exists.
		if err := os.MkdirAll(filepath.Dir(wt), 0o755); err != nil {
			t.Fatal(err)
		}
		projectGit(t, repo, "worktree", "add", "-q", "-b", "bough/"+s, wt, "main")
	}
	write := func(wt, s string) {
		if err := os.WriteFile(filepath.Join(wt, "a.txt"), []byte(s), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	snap := func(wt string) string {
		tree, err := history.Snapshot(wt)
		if err != nil {
			t.Fatalf("snapshot: %v", err)
		}
		return tree
	}
	cp0 := snap(rootWT)
	write(rootWT, "v1\n")
	write(forkWT, "v1\n")
	cp1 := snap(forkWT)
	write(forkWT, "v2\n")

	dir := filepath.Join(home, ".bough", "history")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	at := time.Now().Add(-time.Hour).UTC()
	turn := func(seq int64, text, cp, reply string) []history.Entry {
		return []history.Entry{
			{Seq: seq, At: at, Kind: "input", Data: map[string]any{"text": text, "checkpoint": cp}},
			{Seq: seq + 1, At: at, Kind: "assistant", Data: map[string]any{"text": "```stop\n" + reply + "\n```"}},
			{Seq: seq + 2, At: at, Kind: "done", Data: map[string]any{"files": []string{"a.txt"}}},
		}
	}
	dump := func(path string, entries []history.Entry, flag int) {
		f, err := os.OpenFile(path, flag|os.O_WRONLY, 0o644)
		if err != nil {
			t.Fatal(err)
		}
		defer f.Close()
		for _, e := range entries {
			line, err := json.Marshal(e)
			if err != nil {
				t.Fatal(err)
			}
			if _, err := f.Write(append(line, '\n')); err != nil {
				t.Fatal(err)
			}
		}
	}
	root = filepath.Join(dir, "fui-root.jsonl")
	entries := []history.Entry{{Seq: 1, At: at, Kind: "meta", Data: projectMeta(rootWT)}}
	dump(root, append(entries, turn(2, "root edit prompt", cp0, "ROOT-ANSWER")...), os.O_CREATE|os.O_EXCL)
	fork = filepath.Join(dir, "fui-fork.jsonl")
	if err := history.Fork(root, 2, fork); err != nil {
		t.Fatal(err)
	}
	dump(fork, turn(5, "fork edit prompt", cp1, "FORK-ANSWER"), os.O_APPEND)
	if err := history.PinRef(rootWT, "fui-root", 2, cp0); err != nil {
		t.Fatal(err)
	}
	if err := history.PinRef(forkWT, "fui-fork", 5, cp1); err != nil {
		t.Fatal(err)
	}
	return home, repo, root, fork
}

// forkThenUndoFileIsolationA asserts a.txt's content.
func forkThenUndoFileIsolationA(t *testing.T, a *app, want, where string) {
	t.Helper()
	if got, _ := os.ReadFile(filepath.Join(a.wt, "a.txt")); string(got) != want {
		t.Errorf("%s: a.txt = %q, want %q:\n%s", where, got, want, a.text())
	}
}

// forkThenUndoFileIsolationRefs lists the turn refs pinned for session
// flattened as refname, tree, refname, tree, ...
func forkThenUndoFileIsolationRefs(t *testing.T, repo, session string) []string {
	t.Helper()
	out, err := exec.Command("git", "-C", repo, "for-each-ref", "--format=%(refname) %(objectname)",
		"refs/bough/turns/"+session+"/").CombinedOutput()
	if err != nil {
		t.Fatalf("for-each-ref: %v %s", err, out)
	}
	return strings.Fields(string(out))
}

// forkThenUndoFileIsolationKinds counts the entries of kind in path.
func forkThenUndoFileIsolationKinds(t *testing.T, path, kind string) int {
	t.Helper()
	entries, err := history.Read(path)
	if err != nil {
		t.Fatal(err)
	}
	n := 0
	for _, e := range entries {
		if e.Kind == kind {
			n++
		}
	}
	return n
}

// forkThenUndoFileIsolationConfig replays tape with the history row
// pinned to session.
func forkThenUndoFileIsolationConfig(t *testing.T, tape, session string) string {
	t.Helper()
	const row = "- id: history\n  plugin: history\n"
	yml := replayConfig(tape)
	if !strings.Contains(yml, row) {
		t.Fatalf("replayConfig no longer has a plain history row:\n%s", yml)
	}
	return strings.Replace(yml, row, row+"  config: {file: \""+session+"\"}\n", 1)
}

// forkThenUndoFileIsolationStart resumes session (id) as project vt;
// the history row, not -set, names the file, so --project is passed.
func forkThenUndoFileIsolationStart(t *testing.T, home, tape, path, id string) *app {
	t.Helper()
	a := undoStart(t, home, 100, 30, forkThenUndoFileIsolationConfig(t, tape, path)+projectRows, "--project", projectSlug)
	a.wt = projectWorktree(home, id)
	return a
}

func TestForkThenUndoFileIsolation(t *testing.T) {
	t.Parallel()
	home, repo, root, fork := forkThenUndoFileIsolationWorkspace(t)
	rootBytes, err := os.ReadFile(root)
	if err != nil {
		t.Fatal(err)
	}
	rootRefs := forkThenUndoFileIsolationRefs(t, repo, "fui-root")
	forkRefs := forkThenUndoFileIsolationRefs(t, repo, "fui-fork")
	if len(rootRefs) != 2 || len(forkRefs) != 2 || rootRefs[0] == forkRefs[0] || rootRefs[1] == forkRefs[1] {
		t.Fatalf("seeded refs not distinct: root %v fork %v", rootRefs, forkRefs)
	}
	tape := followUpTape(t) // the fork's live prompt gets REPLY-ALPHA
	a := forkThenUndoFileIsolationStart(t, home, tape, fork, "fui-fork")

	t.Run("ForkResumed", func(t *testing.T) {
		a.waitFor("FORK-ANSWER")
		if s := a.settled(); !strings.Contains(s, "ROOT-ANSWER") {
			t.Errorf("fork lacks the inherited turn 1:\n%s", s)
		}
		a.check("fork resumed")
	})

	t.Run("UndoInForkRestoresTurnOne", func(t *testing.T) {
		a.undoRun()
		a.waitFor("reverted 1 file from turn 5")
		forkThenUndoFileIsolationA(t, a, "v1\n", "after /undo in the fork")
		if n := forkThenUndoFileIsolationKinds(t, fork, "undo"); n != 1 {
			t.Errorf("fork has %d undo entries, want 1", n)
		}
		a.check("fork /undo")
	})

	t.Run("OriginalUntouchedByForkUndo", func(t *testing.T) {
		got, err := os.ReadFile(root)
		if err != nil {
			t.Fatal(err)
		}
		if !bytes.Equal(got, rootBytes) {
			t.Errorf("original session file rewritten by the fork:\nbefore %s\nafter %s", rootBytes, got)
		}
		if r := forkThenUndoFileIsolationRefs(t, repo, "fui-root"); strings.Join(r, " ") != strings.Join(rootRefs, " ") {
			t.Errorf("original refs = %v, want %v", r, rootRefs)
		}
		if r := forkThenUndoFileIsolationRefs(t, repo, "fui-fork"); strings.Join(r, " ") != strings.Join(forkRefs, " ") {
			t.Errorf("fork refs after /undo = %v, want %v", r, forkRefs)
		}
	})

	t.Run("LiveForkTurnPinsUnderForkRef", func(t *testing.T) {
		a.typeText("fork live prompt")
		a.key(uv.KeyEnter, 0)
		a.waitFor("REPLY-ALPHA")
		deadline := time.Now().Add(15 * time.Second)
		for forkThenUndoFileIsolationKinds(t, fork, "done") < 3 && time.Now().Before(deadline) {
			time.Sleep(50 * time.Millisecond)
		}
		if n := forkThenUndoFileIsolationKinds(t, fork, "done"); n != 3 {
			t.Fatalf("fork has %d done entries, want 3:\n%s", n, a.text())
		}
		// for-each-ref sorts by name ("11" before "5"): match by name.
		fr := forkThenUndoFileIsolationRefs(t, repo, "fui-fork")
		byName := map[string]string{}
		for i := 0; i+1 < len(fr); i += 2 {
			byName[fr[i]] = fr[i+1]
		}
		if len(byName) != 2 || byName[forkRefs[0]] != forkRefs[1] {
			t.Errorf("fork refs after a live turn = %v, want %v plus one new ref", fr, forkRefs)
		}
		for name, tree := range byName {
			// The live turn's checkpoint is the tree after the fork
			// /undo: a.txt = v1, the same tree as cp1.
			if name != forkRefs[0] && tree != forkRefs[1] {
				t.Errorf("live fork checkpoint %s = %s, want the post-undo tree %s", name, tree, forkRefs[1])
			}
		}
		if r := forkThenUndoFileIsolationRefs(t, repo, "fui-root"); strings.Join(r, " ") != strings.Join(rootRefs, " ") {
			t.Errorf("live fork turn pinned under the original: %v, want %v", r, rootRefs)
		}
		if got, _ := os.ReadFile(root); !bytes.Equal(got, rootBytes) {
			t.Errorf("original session file changed by the fork's live turn:\n%s", got)
		}
		forkThenUndoFileIsolationA(t, a, "v1\n", "after the live fork turn")
	})

	if t.Failed() {
		return
	}
	rewindAfterResumeAndForkQuit(t, a)

	b := forkThenUndoFileIsolationStart(t, home, tape, root, "fui-root")

	t.Run("OriginalResumedWithoutForkTurns", func(t *testing.T) {
		b.waitFor("ROOT-ANSWER")
		s := b.settled()
		for _, leak := range []string{"FORK-ANSWER", "fork edit prompt", "REPLY-ALPHA", "fork live prompt", "reverted"} {
			if strings.Contains(s, leak) {
				t.Errorf("original shows the fork's %q:\n%s", leak, s)
			}
		}
		forkThenUndoFileIsolationA(t, b, "v1\n", "after resuming the original")
		b.check("original resumed")
	})

	t.Run("OriginalUndoUsesItsOwnCheckpoint", func(t *testing.T) {
		b.undoRun()
		b.waitFor("reverted 1 file from turn 2")
		forkThenUndoFileIsolationA(t, b, "v0\n", "after /undo in the original")
		if n := forkThenUndoFileIsolationKinds(t, fork, "undo"); n != 1 {
			t.Errorf("original /undo wrote into the fork: %d undo entries", n)
		}
		if n := forkThenUndoFileIsolationKinds(t, root, "undo"); n != 1 {
			t.Errorf("original has %d undo entries, want 1", n)
		}
	})
}
