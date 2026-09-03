package history

import (
	"bufio"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

// newRepo makes a git repo with one committed file, a.txt = "one\n".
// Skips without git. The commit runs under a null global config so a
// developer's signing/hook settings never leak in.
func newRepo(t *testing.T) string {
	t.Helper()
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not installed")
	}
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "a.txt"), []byte("one\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	for _, args := range [][]string{{"init", "-q"}, {"add", "a.txt"}, {"commit", "-q", "-m", "init"}} {
		runGit(t, dir, args...)
	}
	return dir
}

func runGit(t *testing.T, dir string, args ...string) string {
	t.Helper()
	c := exec.Command("git", append([]string{"-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"}, args...)...)
	c.Dir = dir
	c.Env = append(os.Environ(), "GIT_CONFIG_GLOBAL=/dev/null", "GIT_CONFIG_NOSYSTEM=1")
	out, err := c.CombinedOutput()
	if err != nil {
		t.Fatalf("git %v: %v\n%s", args, err, out)
	}
	return strings.TrimSpace(string(out))
}

// Snapshot captures modified and untracked files as a tree, leaves
// the index and HEAD exactly as they were, and PinRef names the tree
// under a ref-safe turn ref. Outside a repo it errors.
func TestSnapshotPinsTreeWithoutTouchingIndex(t *testing.T) {
	repo := newRepo(t)
	os.WriteFile(filepath.Join(repo, "a.txt"), []byte("two\n"), 0o644)
	os.WriteFile(filepath.Join(repo, "b.txt"), []byte("new\n"), 0o644)
	head := runGit(t, repo, "rev-parse", "HEAD")
	before := runGit(t, repo, "status", "--porcelain")
	if !strings.Contains(before, "?? b.txt") {
		t.Fatalf("setup: b.txt should be untracked: %q", before)
	}

	tree, err := Snapshot(repo)
	if err != nil {
		t.Fatalf("Snapshot: %v", err)
	}
	if got := runGit(t, repo, "ls-tree", "--name-only", tree); got != "a.txt\nb.txt" {
		t.Fatalf("tree lists %q, want a.txt and b.txt", got)
	}
	if got := runGit(t, repo, "show", tree+":a.txt"); got != "two" {
		t.Fatalf("tree a.txt = %q, want the working copy", got)
	}
	if after := runGit(t, repo, "status", "--porcelain"); after != before {
		t.Fatalf("index touched: status before %q, after %q", before, after)
	}
	if runGit(t, repo, "rev-parse", "HEAD") != head {
		t.Fatal("HEAD moved")
	}

	if err := PinRef(repo, "2026-09-03T10:00:00Z-42", 7, tree); err != nil {
		t.Fatalf("PinRef: %v", err)
	}
	if got := runGit(t, repo, "rev-parse", "refs/bough/turns/2026-09-03T10-00-00Z-42/7"); got != tree {
		t.Fatalf("pinned ref = %q, want %q", got, tree)
	}

	if _, err := Snapshot(t.TempDir()); err == nil {
		t.Fatal("Snapshot outside a repo: want error")
	}
}

// Restore touches exactly the listed paths: a tracked file goes back
// to its checkpoint content, a path the checkpoint lacks is deleted,
// a listed path outside the repo is skipped (never deleted), and an
// unlisted dirty file is left alone.
func TestRestoreExactlyTheListedFiles(t *testing.T) {
	repo := newRepo(t)
	tree, err := Snapshot(repo)
	if err != nil {
		t.Fatal(err)
	}
	write := func(name, s string) {
		t.Helper()
		if err := os.WriteFile(filepath.Join(repo, name), []byte(s), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	write("a.txt", "changed\n")
	write("made.txt", "created\n")
	write("dirty.txt", "not mine\n")
	outside := filepath.Join(t.TempDir(), "outside.txt")
	if err := os.WriteFile(outside, []byte("keep\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	restored, skipped, err := Restore(repo, tree, []string{"a.txt", "made.txt", outside})
	if err != nil {
		t.Fatalf("Restore: %v", err)
	}
	if strings.Join(restored, ",") != "a.txt,made.txt" || strings.Join(skipped, ",") != outside {
		t.Fatalf("restored %v skipped %v", restored, skipped)
	}
	if b, _ := os.ReadFile(filepath.Join(repo, "a.txt")); string(b) != "one\n" {
		t.Fatalf("a.txt = %q, want the checkpoint content", b)
	}
	if _, err := os.Stat(filepath.Join(repo, "made.txt")); !os.IsNotExist(err) {
		t.Fatalf("made.txt should be deleted (absent from the checkpoint): %v", err)
	}
	if b, _ := os.ReadFile(filepath.Join(repo, "dirty.txt")); string(b) != "not mine\n" {
		t.Fatalf("dirty.txt touched: %q", b)
	}
	if b, _ := os.ReadFile(outside); string(b) != "keep\n" {
		t.Fatalf("outside file touched: %q", b)
	}
}

// Append records each entry's parent (the previous seq) in the JSONL,
// ParentOf defaults an absent parent to seq-1, and Ancestors walks
// the chain root first.
func TestAppendParentPointers(t *testing.T) {
	path := filepath.Join(t.TempDir(), "s.jsonl")
	s, err := Open(path)
	if err != nil {
		t.Fatal(err)
	}
	s.Append("meta", map[string]any{"cwd": "/x"})
	s.Append("input", map[string]any{"text": "hi"})
	e3 := s.Append("done", map[string]any{"files": []string{}})
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	if e3.Parent != 2 {
		t.Fatalf("third entry parent = %d, want 2", e3.Parent)
	}
	f, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	sc := bufio.NewScanner(f)
	var lines []map[string]any
	for sc.Scan() {
		var m map[string]any
		if err := json.Unmarshal(sc.Bytes(), &m); err != nil {
			t.Fatal(err)
		}
		lines = append(lines, m)
	}
	if _, has := lines[0]["parent"]; has {
		t.Fatalf("first entry carries a parent: %v", lines[0])
	}
	if lines[1]["parent"] != float64(1) || lines[2]["parent"] != float64(2) {
		t.Fatalf("parents = %v, %v; want 1, 2", lines[1]["parent"], lines[2]["parent"])
	}

	if p := ParentOf(Entry{Seq: 5}); p != 4 {
		t.Fatalf("ParentOf(seq 5, no parent) = %d, want 4", p)
	}
	entries := []Entry{{Seq: 1}, {Seq: 2, Parent: 1}, {Seq: 3, Parent: 2}, {Seq: 4, Parent: 2}}
	anc := Ancestors(entries, 4)
	if len(anc) != 3 || anc[0].Seq != 1 || anc[1].Seq != 2 || anc[2].Seq != 4 {
		t.Fatalf("Ancestors(4) = %v, want seqs 1,2,4", anc)
	}
}

// Fork copies the ancestors of a turn — through its done entry, not
// beyond — into a new file whose meta entry records the origin; the
// source is byte-for-byte untouched; an unfinished or unknown turn is
// an error.
func TestForkCopiesAncestorsAndMarksMeta(t *testing.T) {
	dir := t.TempDir()
	src := filepath.Join(dir, "src.jsonl")
	s, err := Open(src)
	if err != nil {
		t.Fatal(err)
	}
	s.Append("meta", map[string]any{"cwd": "/proj"})
	s.Append("input", map[string]any{"text": "first"})      // 2
	s.Append("assistant", map[string]any{"text": "reply"})  // 3
	s.Append("done", map[string]any{"files": []string{}})   // 4
	s.Append("input", map[string]any{"text": "second"})     // 5
	s.Append("done", map[string]any{"files": []string{}})   // 6
	s.Append("input", map[string]any{"text": "unfinished"}) // 7
	s.Close()
	before, _ := os.ReadFile(src)

	dst := filepath.Join(dir, "fork.jsonl")
	if err := Fork(src, 2, dst); err != nil {
		t.Fatalf("Fork: %v", err)
	}
	got, err := readEntries(dst)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 4 || got[0].Kind != "meta" || got[3].Kind != "done" || got[3].Seq != 4 {
		t.Fatalf("fork entries = %+v, want meta..done(4)", got)
	}
	if got[0].Data["forked_from"] != src || got[0].Data["at_seq"] != float64(2) || got[0].Data["cwd"] != "/proj" {
		t.Fatalf("fork meta = %v", got[0].Data)
	}
	if got[3].Parent != 3 {
		t.Fatalf("copied entry lost its parent: %+v", got[3])
	}
	if after, _ := os.ReadFile(src); string(after) != string(before) {
		t.Fatal("source file was rewritten")
	}

	// The fork resumes like any session: seq continues, parent chains.
	r, err := OpenExisting(dst)
	if err != nil {
		t.Fatal(err)
	}
	if e := r.Append("input", map[string]any{"text": "branch"}); e.Seq != 5 || e.Parent != 4 {
		t.Fatalf("append on fork = seq %d parent %d, want 5/4", e.Seq, e.Parent)
	}
	r.Close()

	if err := Fork(src, 7, filepath.Join(dir, "x.jsonl")); err == nil || !strings.Contains(err.Error(), "not finished") {
		t.Fatalf("fork of an unfinished turn: %v", err)
	}
	if err := Fork(src, 3, filepath.Join(dir, "y.jsonl")); err == nil || !strings.Contains(err.Error(), "no turn 3") {
		t.Fatalf("fork of a non-input seq: %v", err)
	}
	if err := Fork(src, 2, dst); err == nil {
		t.Fatal("fork onto an existing file: want error")
	}
}
