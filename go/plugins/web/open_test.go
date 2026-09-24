package web

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// Open reports a browser that did not open: a missing opener, or one
// that exits non-zero, is an error, not silence.
func TestOpenReportsFailure(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("PATH", dir)
	// Under a loaded full-suite -race run the failing script took over
	// the 2 s grace to exit, and Open took the "still lingering" branch.
	// The test is about the exit status, not the grace, so it waits long.
	// Not parallel (t.Setenv), so nothing else reads openGrace meanwhile.
	grace := openGrace
	openGrace = time.Minute
	t.Cleanup(func() { openGrace = grace })
	if err := Open("http://x/"); err == nil {
		t.Fatal("no opener on PATH: want an error")
	}
	for _, name := range []string{"open", "xdg-open"} {
		script := "#!/bin/sh\necho no handler >&2\nexit 3\n"
		if err := os.WriteFile(filepath.Join(dir, name), []byte(script), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	err := Open("http://x/")
	if err == nil || !strings.Contains(err.Error(), "no handler") {
		t.Fatalf("opener exits 3: err = %v", err)
	}
}
