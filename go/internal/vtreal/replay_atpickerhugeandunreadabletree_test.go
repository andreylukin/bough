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

// atPickerHugeAndUnreadableTreeStart boots bough and fills its cwd
// ($HOME) with 500 dirs × 100 files, "locked/" (mode 000), a "loop"
// symlink to ".", and "zz/needle.go", which sorts after every
// generated file.
func atPickerHugeAndUnreadableTreeStart(t *testing.T) *app {
	t.Helper()
	a := start(t, 100, 30)
	for d := range 500 {
		dir := filepath.Join(a.home, fmt.Sprintf("d%03d", d))
		if err := os.MkdirAll(dir, 0o755); err != nil {
			t.Fatal(err)
		}
		for f := range 100 {
			if err := os.WriteFile(filepath.Join(dir, fmt.Sprintf("f%03d.txt", f)), nil, 0o644); err != nil {
				t.Fatal(err)
			}
		}
	}
	locked := filepath.Join(a.home, "locked")
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
	if err := os.Symlink(".", filepath.Join(a.home, "loop")); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(filepath.Join(a.home, "zz"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(a.home, "zz", "needle.go"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
	return a
}

func TestAtPickerHugeAndUnreadableTree(t *testing.T) {
	t.Parallel()

	t.Run("opens fast and filters", func(t *testing.T) {
		a := atPickerHugeAndUnreadableTreeStart(t)
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
		if os.Getenv("BOUGH_KNOWN_ATPICKERHUGEANDUNREADABLETREE") == "" {
			t.Skip("known bug: listFiles stops at atMaxFiles (5000) before filtering, so files later in the walk are unfindable (plugins/ui/atfiles.go); set BOUGH_KNOWN_ATPICKERHUGEANDUNREADABLETREE=1")
		}
		a := atPickerHugeAndUnreadableTreeStart(t)
		a.typeText("@needle")
		a.atPickerWant("needle", []string{"zz/needle.go"}, "zz/needle.go")
	})
}
