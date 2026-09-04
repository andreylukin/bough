package tools

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
)

func TestToolsViaCodemode(t *testing.T) {
	ctx := kernel.NewContext()
	ctx.Provide("codemode", codemode.New(5*time.Second))
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	cm, err := kernel.Get[*codemode.CodeMode](ctx, "codemode")
	if err != nil {
		t.Fatal(err)
	}

	out, err := cm.Run(`tools.bash("echo hi")`)
	if err != nil {
		t.Fatalf("bash: %v", err)
	}
	if !strings.Contains(out, "hi") {
		t.Errorf("bash output: %q", out)
	}

	path := filepath.Join(t.TempDir(), "f.txt")
	code := `tools.patch(` + jsStr(path) + `, "", "abc\ndef\n"); tools.view(` + jsStr(path) + `)`
	out, err = cm.Run(code)
	if err != nil {
		t.Fatalf("patch/view: %v", err)
	}
	if out != "1│abc\n2│def\n" {
		t.Errorf("view: %q", out)
	}
}

func TestBashTimeoutMessage(t *testing.T) {
	saved := bashTimeout
	bashTimeout = 200 * time.Millisecond
	t.Cleanup(func() { bashTimeout = saved })
	st := &Stats{}
	_, err := st.bash("sleep 5")
	if err == nil {
		t.Fatal("want error")
	}
	if !strings.HasPrefix(err.Error(), "bash: killed after 200ms: sleep 5") {
		t.Errorf("timeout message = %q", err.Error())
	}
	if _, exit, ran := st.Take(); !ran || exit != -1 {
		t.Errorf("exit = %d ran = %v, want -1 true", exit, ran)
	}
}

func TestTurnStats(t *testing.T) {
	ctx := kernel.NewContext()
	ctx.Provide("codemode", codemode.New(5*time.Second))
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	cm, _ := kernel.Get[*codemode.CodeMode](ctx, "codemode")
	st, err := kernel.Get[*Stats](ctx, "turn-stats")
	if err != nil {
		t.Fatal(err)
	}
	if files, _, ran := st.Take(); len(files) != 0 || ran {
		t.Fatalf("fresh stats = %v %v", files, ran)
	}
	path := filepath.Join(t.TempDir(), "f.txt")
	if _, err := cm.Run(`tools.patch(` + jsStr(path) + `, "", "x"); try { tools.bash("exit 3") } catch (e) {}`); err != nil {
		t.Fatal(err)
	}
	files, exit, ran := st.Take()
	if len(files) != 1 || files[0] != path || exit != 3 || !ran {
		t.Errorf("Take = %v %d %v", files, exit, ran)
	}
	if files, _, ran := st.Take(); len(files) != 0 || ran {
		t.Errorf("Take did not reset: %v %v", files, ran)
	}
}

func jsStr(s string) string {
	return `"` + strings.ReplaceAll(s, `"`, `\"`) + `"`
}

// view numbers lines (width from the last number) and honors a range;
// patch replaces exactly one occurrence, refuses zero or many, and
// creates a file only with an empty old.
func TestViewAndPatch(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "a.txt")
	st := &Stats{}
	if _, err := st.patch(path, "x", "y"); err == nil {
		t.Fatal("patch on a missing file with non-empty old should fail")
	}
	if out, err := st.patch(path, "", "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n"); err != nil || !strings.HasPrefix(out, "created") {
		t.Fatalf("create = (%q, %v)", out, err)
	}
	if _, err := st.patch(path, "", "again"); err == nil {
		t.Fatal("empty old on an existing file must not overwrite it")
	}
	got, err := view(path, 9, 10)
	if err != nil || got != " 9│nine\n10│ten\n" {
		t.Fatalf("view range = (%q, %v)", got, err)
	}
	if _, err := view(path, 99); err == nil {
		t.Fatal("start past the end should error")
	}
	if _, err := st.patch(path, "zzz", "q"); err == nil || !strings.Contains(err.Error(), "not found") {
		t.Fatalf("missing old = %v", err)
	}
	if _, err := st.patch(path, "e", "E"); err == nil || !strings.Contains(err.Error(), "occurs") {
		t.Fatalf("ambiguous old = %v", err)
	}
	if out, err := st.patch(path, "two\n", "2\n2b\n"); err != nil || out != "patched "+path+" (+1 lines)\n\n-two\n+2\n+2b" {
		t.Fatalf("patch = (%q, %v)", out, err)
	}
	if got, _ := view(path, 1, 3); got != "1│one\n2│2\n3│2b\n" {
		t.Fatalf("after patch: %q", got)
	}
	// Two writes to the same path count once: a turn that edits a file
	// three times used to end with "✔ wrote a.txt, a.txt, a.txt".
	files, _, _ := st.Take()
	if len(files) != 1 || !strings.HasSuffix(files[0], "a.txt") {
		t.Fatalf("stats files = %v", files)
	}
}

// Cancelling the run's context kills the command AND its children
// (the whole process group) at once: a cancelled turn must not wait
// out a sleep, and must not leave the sleep running.
func TestBashDiesWithTheRunContext(t *testing.T) {
	ctx, cancel := context.WithCancel(t.Context())
	st := &Stats{runCtx: func() context.Context { return ctx }}
	marker := fmt.Sprintf("bough-cancel-test-%d", os.Getpid())
	done := make(chan error, 1)
	start := time.Now()
	go func() {
		_, err := st.bash("sleep 30; echo " + marker + "; sleep 30")
		done <- err
	}()
	time.Sleep(300 * time.Millisecond)
	cancel()
	select {
	case err := <-done:
		if err == nil || !strings.HasPrefix(err.Error(), "bash: cancelled: ") {
			t.Fatalf("want a cancelled error, got %v", err)
		}
		if time.Since(start) > 5*time.Second {
			t.Fatalf("cancel took %s: waited for the command", time.Since(start))
		}
	case <-time.After(10 * time.Second):
		t.Fatal("bash did not return after cancel")
	}
	time.Sleep(200 * time.Millisecond)
	out, _ := exec.Command("pgrep", "-f", "sleep 30; echo "+marker).Output()
	if strings.TrimSpace(string(out)) != "" {
		t.Fatalf("the shell survived the cancel: pids %s", out)
	}
}

// write puts a whole file down (dirs made) and counts as a written file;
// bash takes its script on stdin, so a NUL byte or a very long script
// does not fail at exec.
func TestWriteAndStdinBash(t *testing.T) {
	st := &Stats{}
	dir := t.TempDir()
	p := dir + "/a/b/c.txt"
	out, err := st.write(p, "one\ntwo\n")
	if err != nil || !strings.Contains(out, "2 lines") && !strings.Contains(out, "3 lines") {
		t.Fatalf("write: %q %v", out, err)
	}
	if b, _ := os.ReadFile(p); string(b) != "one\ntwo\n" {
		t.Fatalf("content = %q", b)
	}
	if files, _, _ := st.Take(); len(files) != 1 || files[0] != p {
		t.Fatalf("written files = %v", files)
	}
	long := "echo start; x='" + strings.Repeat("y", 300*1024) + "'; echo ${#x}"
	if out, err := st.bash(long); err != nil || !strings.Contains(out, "307200") {
		t.Fatalf("long script: %q %v", out, err)
	}
	if out, err := st.bash("printf 'a\\0b' | tr '\\0' -"); err != nil || strings.TrimSpace(out) != "a-b" {
		t.Fatalf("nul in output: %q %v", out, err)
	}
}

func TestLineDiff(t *testing.T) {
	got := lineDiff("a\nb\nc\nd\ne\n", "a\nb\nX\nd\ne\n")
	want := "\n\n b\n-c\n+X\n d"
	if got != want {
		t.Fatalf("diff = %q, want %q", got, want)
	}
	if lineDiff("same", "same") != "" {
		t.Fatal("equal texts should have no diff")
	}
	if got := lineDiff("", "new\n"); got != "\n\n+new" {
		t.Fatalf("new content diff = %q", got)
	}
}

func TestLineDiffGap(t *testing.T) {
	got := lineDiff("a\nb\nc\nd\ne\nf\ng\n", "A\nb\nc\nd\ne\nf\nG\n")
	if want := "\n\n-a\n+A\n b\n…\n f\n-g\n+G"; got != want {
		t.Fatalf("diff = %q, want %q", got, want)
	}
}

// A missing file names its real neighbours: a model that guessed
// turn.go is told loop.go exists instead of guessing again.
func TestViewMissingFileSuggestsNeighbours(t *testing.T) {
	dir := t.TempDir()
	for _, n := range []string{"loop.go", "cancel.go", "unrelated.md"} {
		if err := os.WriteFile(dir+"/"+n, []byte("x"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	_, err := view(dir + "/turn.go")
	if err == nil {
		t.Fatal("want an error")
	}
	if !strings.Contains(err.Error(), "loop.go") || !strings.Contains(err.Error(), dir) {
		t.Fatalf("error should name the directory and its files: %v", err)
	}
	if _, err := view(dir + "/nope/deep.go"); err == nil || !strings.Contains(err.Error(), "no directory") {
		t.Fatalf("a missing directory says so: %v", err)
	}
	if _, err := (&Stats{}).patch(dir+"/turn.go", "a", "b"); err == nil || !strings.Contains(err.Error(), "loop.go") {
		t.Fatalf("patch suggests neighbours too: %v", err)
	}
}

// A not-found patch points at what is nearly there: "hellp" is a typo
// of "hello", so the error shows that line's neighbourhood.
func TestPatchNotFoundShowsClosestMatch(t *testing.T) {
	dir := t.TempDir()
	path := dir + "/f.txt"
	if err := os.WriteFile(path, []byte("hello\nworld\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	_, err := (&Stats{}).patch(path, "hellp", "hi")
	if err == nil {
		t.Fatal("want an error")
	}
	if !strings.Contains(err.Error(), "old text not found") {
		t.Fatalf("error should keep the old message: %v", err)
	}
	if !strings.Contains(err.Error(), "closest match near line 1") || !strings.Contains(err.Error(), "hello") {
		t.Fatalf("error should show the closest match's neighbourhood: %v", err)
	}
}

func TestClosestMatch(t *testing.T) {
	if line, ok := closestMatch("aa\nbb\ncc\n", "bb\ncc"); line != 3 || !ok {
		t.Fatalf("closest to \"bb\\ncc\" = line %d, %v; want 3, true", line, ok)
	}
	if line, ok := closestMatch("hello\nworld\n", "hellp"); line != 1 || !ok {
		t.Fatalf("closest to \"hellp\" = line %d, %v; want 1, true", line, ok)
	}
	if _, ok := closestMatch("one line\n", ""); ok {
		t.Fatal("an empty old has no match")
	}
	if _, ok := closestMatch("one\n", "two\nlines\n"); ok {
		t.Fatal("a file shorter than old has no window")
	}
	if d := editDistance("kitten", "sitting"); d != 3 {
		t.Fatalf("editDistance(kitten, sitting) = %d, want 3", d)
	}
	if got := nearestLines("a\nb\nc\nd\ne\n", 3, 2); got != "1│a\n2│b\n3│c\n4│d\n5│e\n" {
		t.Fatalf("nearestLines = %q", got)
	}
	if got := nearestLines("a\nb\n", 99, 2); got != "" {
		t.Fatalf("nearestLines past EOF = %q, want empty", got)
	}
}
