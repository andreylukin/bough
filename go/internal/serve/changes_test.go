package serve

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/plugins/history"
)

func TestChanges(t *testing.T) {
	t.Parallel()
	if _, ok := Changes(context.Background(), t.TempDir()); ok {
		t.Fatal("a plain directory is not a repository")
	}
	dir := t.TempDir()
	run := func(args ...string) {
		t.Helper()
		cmd := exec.Command("git", append([]string{"-C", dir, "-c", "user.email=t@t", "-c", "user.name=t"}, args...)...)
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v\n%s", args, err, out)
		}
	}
	write := func(name, body string) {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	run("init", "-q")
	write("a.go", "one\ntwo\n")
	run("add", ".")
	run("commit", "-qm", "init")
	write("a.go", "one\nthree\nfour\n")
	write("b.go", "new\n")

	files, ok := Changes(context.Background(), dir)
	if !ok || len(files) != 2 {
		t.Fatalf("changes = %+v ok=%v, want a.go and b.go", files, ok)
	}
	if files[0] != (Change{Path: "a.go", Add: 2, Del: 1}) || !files[1].New || files[1].Path != "b.go" {
		t.Fatalf("changes = %+v", files)
	}
	if d, err := Diff(context.Background(), dir, "a.go"); err != nil || !strings.Contains(d, "-two") || !strings.Contains(d, "+four") {
		t.Fatalf("diff a.go = %q, %v", d, err)
	}
	if d, err := Diff(context.Background(), dir, "b.go"); err != nil || !strings.Contains(d, "+new") {
		t.Fatalf("diff of an untracked file = %q, %v", d, err)
	}
}

// The session's review never counts dirt that was in the tree before its
// first turn: todo.go was already edited when the session started.
func TestSessionEdits(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	run := func(args ...string) {
		t.Helper()
		cmd := exec.Command("git", append([]string{"-C", dir, "-c", "user.email=t@t", "-c", "user.name=t"}, args...)...)
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v\n%s", args, err, out)
		}
	}
	write := func(name, body string) {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	run("init", "-q")
	write("todo.go", "a\n")
	write("a.go", "one\n")
	run("add", ".")
	run("commit", "-qm", "init")
	write("todo.go", "a\ndirt\n") // pre-existing, not the agent's
	tree, err := history.Snapshot(dir)
	if err != nil {
		t.Fatal(err)
	}
	write("a.go", "one\ntwo\n")
	write("b.go", "new\n")
	entries := []history.Entry{
		{Seq: 1, Kind: "input", Data: map[string]any{"text": "go", "checkpoint": tree}},
		{Seq: 2, Kind: "done", Data: map[string]any{"files": []any{"a.go", filepath.Join(dir, "b.go")}}},
	}
	edits, ok := SessionEdits(context.Background(), dir, entries)
	if !ok || len(edits) != 2 {
		t.Fatalf("edits = %+v ok=%v, want a.go and b.go only", edits, ok)
	}
	if edits[0].Path != "a.go" || edits[0].Add != 1 || edits[0].New || !edits[0].Patch {
		t.Fatalf("a.go = %+v", edits[0])
	}
	if edits[1].Path != "b.go" || !edits[1].New {
		t.Fatalf("b.go = %+v", edits[1])
	}
	if d, err := SessionDiff(context.Background(), dir, entries, "a.go"); err != nil || !strings.Contains(d, "+two") {
		t.Fatalf("session diff a.go = %q, %v", d, err)
	}
	// No checkpoint: the paths are known, the patches are not.
	edits, _ = SessionEdits(context.Background(), dir, entries[1:])
	if len(edits) != 2 || edits[0].Patch {
		t.Fatalf("without a checkpoint = %+v", edits)
	}
}

func TestSessionEditsNoCwd(t *testing.T) {
	entries := []history.Entry{{Kind: "done", Data: map[string]any{"files": []any{"a.go"}}}}
	if edits, ok := SessionEdits(context.Background(), "", entries); ok || edits != nil {
		t.Fatalf("no cwd read as a repository: %v %v", edits, ok)
	}
	if _, err := SessionDiff(context.Background(), "", entries, "a.go"); err == nil {
		t.Fatal("no cwd diffed the server's own checkout")
	}
}

// A repository with no commit yet still gets real patches: the
// checkpoint is a tree object, not HEAD.
func TestSessionEditsNoCommit(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	if out, err := exec.Command("git", "-C", dir, "init", "-q").CombinedOutput(); err != nil {
		t.Fatalf("git init: %v\n%s", err, out)
	}
	if err := os.WriteFile(filepath.Join(dir, "a.go"), []byte("one\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	tree, err := history.Snapshot(dir)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "a.go"), []byte("one\ntwo\nthree\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	entries := []history.Entry{
		{Seq: 1, Kind: "input", Data: map[string]any{"text": "go", "checkpoint": tree}},
		{Seq: 2, Kind: "done", Data: map[string]any{"files": []any{"a.go"}}},
	}
	edits, ok := SessionEdits(context.Background(), dir, entries)
	if !ok || len(edits) != 1 || !edits[0].Patch || edits[0].Add != 2 || edits[0].Del != 0 {
		t.Fatalf("edits = %+v ok=%v, want a.go +2 with a patch", edits, ok)
	}
}

// R2-G: a turn's edits run from its own checkpoint to the next turn's,
// so an earlier or later turn's lines are not counted as its.
func TestTurnEdits(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	run := func(args ...string) {
		t.Helper()
		cmd := exec.Command("git", append([]string{"-C", dir, "-c", "user.email=t@t", "-c", "user.name=t"}, args...)...)
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v\n%s", args, err, out)
		}
	}
	write := func(name, body string) {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	snap := func() string {
		tree, err := history.Snapshot(dir)
		if err != nil {
			t.Fatal(err)
		}
		return tree
	}
	run("init", "-q")
	write("a.go", "one\n")
	run("add", ".")
	run("commit", "-qm", "init")
	t1 := snap()
	write("a.go", "one\ntwo\n")
	t2 := snap()
	write("a.go", "one\ntwo\nthree\n")
	write("b.go", "b\n")
	entries := []history.Entry{
		{Seq: 1, Kind: "input", Data: map[string]any{"checkpoint": t1}},
		{Seq: 2, Kind: "done", Data: map[string]any{"files": []any{"a.go"}}},
		{Seq: 3, Kind: "input", Data: map[string]any{"checkpoint": t2}},
		{Seq: 4, Kind: "done", Data: map[string]any{"files": []any{"a.go", "b.go"}}},
	}
	ctx := context.Background()
	first, ok := TurnEdits(ctx, dir, entries, 1)
	if !ok || len(first) != 1 || first[0].Path != "a.go" || first[0].Add != 1 || !first[0].Patch {
		t.Fatalf("turn 1 = %+v ok=%v, want a.go +1 only", first, ok)
	}
	second, _ := TurnEdits(ctx, dir, entries, 3)
	if len(second) != 2 || second[0].Add != 1 || !second[1].New {
		t.Fatalf("turn 3 = %+v, want a.go +1 and new b.go", second)
	}
	if d, err := TurnDiff(ctx, dir, entries, 1, "a.go"); err != nil || !strings.Contains(d, "+two") || strings.Contains(d, "+three") {
		t.Fatalf("turn 1 diff = %q, %v", d, err)
	}
	if _, err := TurnDiff(ctx, dir, entries, 9, "a.go"); err == nil {
		t.Fatal("an unknown turn diffed")
	}
}
