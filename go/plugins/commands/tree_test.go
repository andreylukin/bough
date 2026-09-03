package commands_test

// /undo and /tree over a real history.Store and a temp git repo. These
// chdir into the repo (the tools record cwd-relative paths), so they
// are sequential, never t.Parallel.

import (
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/history"
)

// newRepo makes a git repo with committed keep.txt and other.txt.
// Skips without git; the commit runs under a null global config.
func newRepo(t *testing.T) string {
	t.Helper()
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not installed")
	}
	dir := t.TempDir()
	for _, n := range []string{"keep.txt", "other.txt"} {
		if err := os.WriteFile(filepath.Join(dir, n), []byte("orig\n"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	for _, args := range [][]string{{"init", "-q"}, {"add", "."}, {"commit", "-q", "-m", "init"}} {
		c := exec.Command("git", append([]string{"-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"}, args...)...)
		c.Dir = dir
		c.Env = append(os.Environ(), "GIT_CONFIG_GLOBAL=/dev/null", "GIT_CONFIG_NOSYSTEM=1")
		if out, err := c.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v\n%s", args, err, out)
		}
	}
	return dir
}

func write(t *testing.T, name, s string) {
	t.Helper()
	if err := os.WriteFile(name, []byte(s), 0o644); err != nil {
		t.Fatal(err)
	}
}

func read(t *testing.T, name string) string {
	t.Helper()
	b, err := os.ReadFile(name)
	if err != nil {
		return "<missing>"
	}
	return string(b)
}

// mountCommands provides store as the history service and mounts the
// commands row over it.
func mountCommands(t *testing.T, store *history.Store) *commands.Registry {
	t.Helper()
	ctx := kernel.NewContext()
	ctx.Provide("history", store)
	if err := ctx.Mount([]kernel.Row{{ID: "commands", Plugin: "commands"}}); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(ctx.Unmount)
	reg, err := kernel.Get[*commands.Registry](ctx, "commands")
	if err != nil {
		t.Fatal(err)
	}
	return reg
}

// turn records one turn the way the loop does: a checkpoint of the
// working tree on the input entry, the files written on its done.
func turn(t *testing.T, store *history.Store, repo, text string, work func(), files ...string) {
	t.Helper()
	tree, err := history.Snapshot(repo)
	if err != nil {
		t.Fatal(err)
	}
	store.Append("input", map[string]any{"text": text, "checkpoint": tree})
	work()
	store.Append("done", map[string]any{"files": files})
}

// Two turns: /undo reverts exactly the newer turn's files, /undo again
// the older one's (its created file deleted, its edited file restored),
// a file dirtied outside any turn is never touched, and a third /undo
// has nothing left.
func TestUndoRevertsListedFilesOnly(t *testing.T) {
	repo := newRepo(t)
	t.Chdir(repo)
	store, err := history.Open(filepath.Join(t.TempDir(), "s.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	reg := mountCommands(t, store)

	turn(t, store, repo, "one", func() {
		write(t, "keep.txt", "turn1\n")
		write(t, "new.txt", "n1\n")
	}, "keep.txt", "new.txt")
	turn(t, store, repo, "two", func() {
		write(t, "new.txt", "n2\n")
	}, "new.txt")
	write(t, "other.txt", "dirty\n") // the user's own edit, in no list
	write(t, "scratch.txt", "mine\n")

	out, err := reg.Run("undo", "")
	if err != nil || out != "reverted 1 file from turn 3\n  new.txt" {
		t.Fatalf("/undo = %q, %v", out, err)
	}
	if got := read(t, "new.txt"); got != "n1\n" {
		t.Fatalf("new.txt after first undo = %q, want turn 1's content", got)
	}
	if read(t, "keep.txt") != "turn1\n" {
		t.Fatal("keep.txt (not in turn 2's list) was touched")
	}

	out, err = reg.Run("undo", "")
	if err != nil || out != "reverted 2 files from turn 1\n  keep.txt\n  new.txt" {
		t.Fatalf("second /undo = %q, %v", out, err)
	}
	if read(t, "keep.txt") != "orig\n" {
		t.Fatalf("keep.txt = %q, want the committed content", read(t, "keep.txt"))
	}
	if _, err := os.Stat("new.txt"); !os.IsNotExist(err) {
		t.Fatalf("new.txt (created in turn 1) should be deleted: %v", err)
	}
	if read(t, "other.txt") != "dirty\n" || read(t, "scratch.txt") != "mine\n" {
		t.Fatal("a file outside the turn's list was touched")
	}

	if _, err := reg.Run("undo", ""); err == nil || !strings.Contains(err.Error(), "nothing to undo") {
		t.Fatalf("third /undo = %v, want nothing to undo", err)
	}
	var undos []history.Entry
	for _, e := range store.Entries() {
		if e.Kind == "undo" {
			undos = append(undos, e)
		}
	}
	if len(undos) != 2 || undos[0].Data["seq_of_turn"] != int64(3) || undos[1].Data["seq_of_turn"] != int64(1) {
		t.Fatalf("undo entries = %+v", undos)
	}
}

// /undo refuses while the last turn has no done yet: the model may
// still be writing the files it would revert.
func TestUndoRefusesWhileRunning(t *testing.T) {
	repo := newRepo(t)
	t.Chdir(repo)
	store, err := history.Open(filepath.Join(t.TempDir(), "s.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	reg := mountCommands(t, store)
	turn(t, store, repo, "one", func() { write(t, "keep.txt", "turn1\n") }, "keep.txt")
	store.Append("input", map[string]any{"text": "two"}) // no done yet

	if _, err := reg.Run("undo", ""); err == nil || !strings.Contains(err.Error(), "turn 3 is still running") {
		t.Fatalf("/undo mid-turn = %v", err)
	}
	if read(t, "keep.txt") != "turn1\n" {
		t.Fatal("keep.txt reverted under a running turn")
	}
}

// /tree lists the turns newest first; "/tree <seq>" writes the fork
// next to the current file (ancestors through that turn's done, the
// origin on its meta) and answers with the resume action for it.
func TestTreeListsAndForks(t *testing.T) {
	dir := t.TempDir()
	store, err := history.Open(filepath.Join(dir, "orig.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	store.Append("meta", map[string]any{"cwd": "/proj"})
	store.Append("input", map[string]any{"text": "first question\nmore"})
	store.Append("done", map[string]any{"files": []string{}})
	store.Append("input", map[string]any{"text": "second"})
	store.Append("done", map[string]any{"files": []string{}})
	reg := mountCommands(t, store)

	out, err := reg.Run("tree", "")
	if err != nil {
		t.Fatal(err)
	}
	lines := strings.Split(out, "\n")
	if len(lines) != 3 || !strings.HasPrefix(lines[0], "   4  ") || !strings.HasSuffix(lines[0], "  second") ||
		!strings.HasPrefix(lines[1], "   2  ") || !strings.HasSuffix(lines[1], "  first question") ||
		lines[2] != "fork at a turn: /tree <seq>" {
		t.Fatalf("/tree =\n%s", out)
	}

	_, err = reg.Run("tree", "2")
	act, ok := errors.AsType[commands.UIAction](err)
	if !ok {
		t.Fatalf("/tree 2 = %v, want a UIAction", err)
	}
	id, ok := commands.ResumeID(act)
	if !ok || !strings.HasSuffix(id, "-f2") {
		t.Fatalf("/tree 2 action = %q, want resume:<id>-f2", act)
	}
	infos, err := history.List(dir)
	if err != nil || len(infos) != 2 {
		t.Fatalf("List = %v, %v; want the original and the fork", infos, err)
	}
	var fork history.SessionInfo
	for _, s := range infos {
		if s.ID == id {
			fork = s
		}
	}
	if fork.ID == "" || fork.Entries != 3 || fork.Title != "first question" {
		t.Fatalf("fork info = %+v", fork)
	}
	f, err := history.OpenExisting(fork.Path)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	es := f.Entries()
	if es[0].Data["forked_from"] != store.Path() || es[0].Data["at_seq"] != float64(2) || es[2].Kind != "done" {
		t.Fatalf("fork entries = %+v", es)
	}

	if _, err := reg.Run("tree", "9"); err == nil || !strings.Contains(err.Error(), "no turn 9") {
		t.Fatalf("/tree 9 = %v", err)
	}
	if _, err := reg.Run("tree", "x"); err == nil || !strings.Contains(err.Error(), "usage") {
		t.Fatalf("/tree x = %v", err)
	}
}
