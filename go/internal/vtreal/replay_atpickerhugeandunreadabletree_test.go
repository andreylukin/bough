package vtreal

// The "@" picker over a hostile tree: 50k files, a chmod 000 directory
// and a symlink loop in the session cwd. The picker must open fast,
// survive the unreadable/looping entries, and still filter.

import (
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// atPickerHugeAndUnreadableTreeFill fills root with 500 dirs × 100
// files, "locked/" (mode 000), a "loop" symlink to ".", and
// "zz/needle.go", which sorts after every generated file. Both cases
// share one tree (bough only reads its cwd); built per case, the 50k
// files were most of each case's time, and the cases ran in turn.
func atPickerHugeAndUnreadableTreeFill(t *testing.T, root string) {
	t.Helper()
	for d := range 500 {
		dir := filepath.Join(root, fmt.Sprintf("d%03d", d))
		if err := os.MkdirAll(dir, 0o755); err != nil {
			t.Fatal(err)
		}
		for f := range 100 {
			if err := os.WriteFile(filepath.Join(dir, fmt.Sprintf("f%03d.txt", f)), nil, 0o644); err != nil {
				t.Fatal(err)
			}
		}
	}
	locked := filepath.Join(root, "locked")
	if err := os.MkdirAll(locked, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(locked, "secret.txt"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(locked, 0); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.Chmod(locked, 0o755) })
	if err := os.Symlink(".", filepath.Join(root, "loop")); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(filepath.Join(root, "zz"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "zz", "needle.go"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
}

func TestAtPickerHugeAndUnreadableTree(t *testing.T) {
	t.Parallel()
	tree := t.TempDir()
	atPickerHugeAndUnreadableTreeFill(t, tree)

	t.Run("opens fast and filters", func(t *testing.T) {
		t.Parallel()
		a := startCfgIn(t, tree, 100, 30, config)
		t0 := time.Now()
		a.typeText("@")
		a.waitUntil(func(s string) bool { n, _ := atPickerRows(s); return len(n) > 0 }, "picker rows")
		if d := time.Since(t0); d > time.Second {
			t.Errorf("picker took %v to show rows, want < 1s", d)
		}
		// Alive after the loop / locked dir: a filtered query answers.
		a.typeText("d003/f099")
		a.atPickerWant("filter", []string{"d003/f099.txt"}, "d003/f099.txt")
	})

	t.Run("finds a file past the walk cap", func(t *testing.T) {
		t.Parallel()
		a := startCfgIn(t, tree, 100, 30, config)
		a.typeText("@needle")
		a.atPickerWant("needle", []string{"zz/needle.go"}, "zz/needle.go")
	})
}
