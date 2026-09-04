package scratch

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func newPad(t *testing.T) *Pad {
	t.Helper()
	p, err := New(filepath.Join(t.TempDir(), "pad"))
	if err != nil {
		t.Fatal(err)
	}
	return p
}

// The thing the runtime cannot do: a value one block computes, read
// back by the next. It survives the process too, so a resumed session
// picks up where it left off.
func TestValuesSurviveBlocksAndSessions(t *testing.T) {
	p := newPad(t)
	if _, err := p.Set("files", []any{"a.go", "b.go"}); err != nil {
		t.Fatal(err)
	}
	if _, err := p.Set("count", 2.0); err != nil {
		t.Fatal(err)
	}
	got, err := p.Get("files")
	if err != nil {
		t.Fatal(err)
	}
	if list, ok := got.([]any); !ok || len(list) != 2 || list[0] != "a.go" {
		t.Fatalf("files = %#v", got)
	}
	if keys := p.Keys(); strings.Join(keys, ",") != "count,files" {
		t.Fatalf("keys = %v", keys)
	}

	// A second Pad on the same directory is the resumed session.
	again, err := New(p.Dir())
	if err != nil {
		t.Fatal(err)
	}
	if got, err := again.Get("count"); err != nil || got != 2.0 {
		t.Fatalf("count after reopen = %v, %v", got, err)
	}
}

// A mistyped key says what IS stored rather than just failing.
func TestGetNamesWhatIsStored(t *testing.T) {
	p := newPad(t)
	if _, err := p.Get("anything"); err == nil || !strings.Contains(err.Error(), "nothing stored yet") {
		t.Fatalf("empty pad: %v", err)
	}
	p.Set("plan", "do the thing")
	_, err := p.Get("plna")
	if err == nil || !strings.Contains(err.Error(), "the pad holds: plan") {
		t.Fatalf("err = %v", err)
	}
	if _, err := p.Drop("plan"); err != nil {
		t.Fatal(err)
	}
	if _, err := p.Drop("plan"); err == nil {
		t.Fatal("dropping twice should say so")
	}
}

// A value big enough to be a file is refused as a value, and told
// where to put it instead.
func TestBigValueIsRefusedWithTheAlternative(t *testing.T) {
	p := newPad(t)
	_, err := p.Set("dump", strings.Repeat("x", maxValue+1))
	if err == nil || !strings.Contains(err.Error(), "scratch.file") {
		t.Fatalf("err = %v", err)
	}
}

// Notes are the durable half: they outlive the context window.
func TestNotesAppend(t *testing.T) {
	p := newPad(t)
	if _, err := p.Note("the golden file was stale"); err != nil {
		t.Fatal(err)
	}
	if _, err := p.Note("cgo must stay off"); err != nil {
		t.Fatal(err)
	}
	notes := p.Notes()
	if !strings.Contains(notes, "golden file was stale") || !strings.Contains(notes, "cgo must stay off") {
		t.Fatalf("notes = %q", notes)
	}
	if n := strings.Count(notes, "\n"); n != 2 {
		t.Fatalf("want one line per note, got %d:\n%s", n, notes)
	}
	if _, err := p.Note("   "); err == nil {
		t.Fatal("an empty note should say so")
	}
}

// A throwaway file goes in the pad — and cannot escape it, whatever
// name the model asks for.
func TestFileStaysInsideThePad(t *testing.T) {
	p := newPad(t)
	path, err := p.File("probe/run.go")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(path, p.Dir()+string(os.PathSeparator)) {
		t.Fatalf("path %q escaped %q", path, p.Dir())
	}
	if err := os.WriteFile(path, []byte("package main"), 0o644); err != nil {
		t.Fatalf("the parent was not created: %v", err)
	}
	for _, bad := range []string{"../../etc/passwd", "/etc/passwd", "..", ""} {
		got, err := p.File(bad)
		if err == nil && !strings.HasPrefix(got, p.Dir()+string(os.PathSeparator)) {
			t.Fatalf("File(%q) = %q, outside the pad", bad, got)
		}
	}
}

// /scratch shows what is in it: values, notes, files, with sizes.
func TestListShowsTheContents(t *testing.T) {
	p := newPad(t)
	if got := p.List(); !strings.Contains(got, "(empty)") {
		t.Fatalf("a fresh pad = %q", got)
	}
	p.Set("plan", "step one")
	p.Note("a finding")
	path, _ := p.File("probe.go")
	os.WriteFile(path, []byte("package main\n"), 0o644)

	got := p.List()
	for _, want := range []string{p.Dir(), "values: plan", "notes.md", "probe.go"} {
		if !strings.Contains(got, want) {
			t.Fatalf("List missing %q:\n%s", want, got)
		}
	}
	if strings.Contains(got, stateFile) {
		t.Fatalf("the state file is plumbing, not content:\n%s", got)
	}
}

// The pad is shared with subagents — they run on the parent's tools —
// so a note says which one wrote it, and the parent's notes stay
// unattributed.
func TestNotesSayWhoWroteThem(t *testing.T) {
	p := newPad(t)
	p.Note("the parent looked here")
	p.Writer("subagent 2")
	p.Note("the child found this")
	p.Writer("")
	p.Note("and the parent again")

	notes := p.Notes()
	if !strings.Contains(notes, "[subagent 2] the child found this") {
		t.Fatalf("child note unattributed:\n%s", notes)
	}
	for _, mine := range []string{"the parent looked here", "and the parent again"} {
		for _, l := range strings.Split(notes, "\n") {
			if strings.Contains(l, mine) && strings.Contains(l, "[") {
				t.Fatalf("the parent's own note was tagged: %q", l)
			}
		}
	}
}

// A session that never uses the scratchpad leaves nothing behind: the
// previous era of this feature left 9,434 empty directories.
func TestNothingOnDiskUntilSomethingIsWritten(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "pad")
	p, err := New(dir)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(dir); !os.IsNotExist(err) {
		t.Fatalf("the directory was made before anything was written: %v", err)
	}
	if got := p.List(); !strings.Contains(got, "(empty)") {
		t.Fatalf("List on an unused pad = %q", got)
	}
	if _, err := os.Stat(dir); !os.IsNotExist(err) {
		t.Fatal("listing an unused pad created it")
	}

	if _, err := p.Set("k", 1); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(dir, stateFile)); err != nil {
		t.Fatalf("the first write did not create the pad: %v", err)
	}
}
