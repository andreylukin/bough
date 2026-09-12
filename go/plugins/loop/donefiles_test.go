package loop

// What a turn records as changed: the write tools' own tally, plus
// what the checkpoints saw move. Neither seam sees the other's files.

import (
	"testing"
)

// fakeCP is the checkpoints seam: Changed reports what a shell command
// did, which the turn-stats tally never sees.
type fakeCP struct {
	changed []string
	asked   []string // the checkpoints Changed was called with
}

func (f *fakeCP) Snapshot() string          { return "tree-before" }
func (f *fakeCP) Pin(int64, string)         {}
func (f *fakeCP) Changed(b string) []string { f.asked = append(f.asked, b); return f.changed }

// doneStats is the turn-stats seam, named apart from the fakeStats
// another test owns.
type doneStats struct {
	files []string
	exit  int
	ran   bool
}

func (f *doneStats) Take() ([]string, int, bool) {
	files := f.files
	f.files = nil // Take resets, as the real one does
	return files, f.exit, f.ran
}

func filesOf(t *testing.T, data map[string]any) []string {
	t.Helper()
	files, ok := data["files"].([]string)
	if !ok {
		t.Fatalf("files = %#v, want []string", data["files"])
	}
	return files
}

// The whole point: a turn that wrote only through the shell still says
// what it changed.
func TestDoneFilesIncludeShellEditsTheToolsNeverSaw(t *testing.T) {
	t.Parallel()
	cp := &fakeCP{changed: []string{"edited_by_sed.go"}}
	r := &runner{cp: cp, turnTree: "tree-before"}

	got := filesOf(t, r.doneData())
	if len(got) != 1 || got[0] != "edited_by_sed.go" {
		t.Fatalf("files = %v, want the shell's edit", got)
	}
	if len(cp.asked) != 1 || cp.asked[0] != "tree-before" {
		t.Errorf("Changed asked with %v, want this turn's checkpoint", cp.asked)
	}
}

// Both seams contribute, the tools' record first, and a file both saw
// appears once.
func TestDoneFilesUnionToolsAndCheckpointsWithoutDuplicates(t *testing.T) {
	t.Parallel()
	r := &runner{
		stats:    &doneStats{files: []string{"written.go", "shared.go"}, ran: true},
		cp:       &fakeCP{changed: []string{"shared.go", "sed.go"}},
		turnTree: "tree-before",
	}

	got := filesOf(t, r.doneData())
	want := []string{"written.go", "shared.go", "sed.go"}
	if len(got) != len(want) {
		t.Fatalf("files = %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("files = %v, want %v (tools first, then what moved)", got, want)
		}
	}
}

// Outside a repo there is no checkpoint, and the tally is all there is.
func TestDoneFilesWithoutCheckpointsAreTheToolsAlone(t *testing.T) {
	t.Parallel()
	r := &runner{stats: &doneStats{files: []string{"written.go"}, exit: 2, ran: true}}

	data := r.doneData()
	if got := filesOf(t, data); len(got) != 1 || got[0] != "written.go" {
		t.Fatalf("files = %v", got)
	}
	// The bash exit code still rides along; it moved out of the branch
	// that used to return early.
	if data["exit"] != 2 {
		t.Errorf("exit = %v, want 2", data["exit"])
	}
}

// A turn with neither seam records an empty list, never nil: the UI
// and /undo both read it as a list.
func TestDoneFilesEmptyNotNil(t *testing.T) {
	t.Parallel()
	r := &runner{}
	got := filesOf(t, r.doneData())
	if got == nil || len(got) != 0 {
		t.Fatalf("files = %#v, want an empty list", got)
	}
}

// A turn that changed nothing says so, and asks nothing of git beyond
// the one diff.
func TestDoneFilesNothingChanged(t *testing.T) {
	t.Parallel()
	r := &runner{stats: &doneStats{ran: true}, cp: &fakeCP{}, turnTree: "tree-before"}
	if got := filesOf(t, r.doneData()); len(got) != 0 {
		t.Fatalf("files = %v, want none", got)
	}
}

func TestMergeFilesKeepsOrderAndDropsBlanks(t *testing.T) {
	t.Parallel()
	got := mergeFiles([]string{"a", "", "b"}, []string{"b", "c", ""})
	want := []string{"a", "b", "c"}
	if len(got) != len(want) {
		t.Fatalf("merge = %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("merge = %v, want %v", got, want)
		}
	}
}

// Snapshotting the tree is real git work over every file in the repo,
// so a turn that ran no shell command must not pay for it: the write
// tools already saw everything such a turn did.
func TestDoneFilesSkipTheDiffWhenNoShellCommandRan(t *testing.T) {
	t.Parallel()
	cp := &fakeCP{changed: []string{"never-asked.go"}}
	r := &runner{stats: &doneStats{files: []string{"written.go"}}, cp: cp, turnTree: "tree-before"}

	got := filesOf(t, r.doneData())
	if len(got) != 1 || got[0] != "written.go" {
		t.Fatalf("files = %v, want the tools' record alone", got)
	}
	if len(cp.asked) != 0 {
		t.Errorf("git was consulted %d times for a turn that ran nothing", len(cp.asked))
	}
}
