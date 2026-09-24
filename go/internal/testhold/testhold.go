// Package testhold parks a process at a named point while a file says
// so. The windows it opens last milliseconds in a real run (a child
// between its SIGINT's "cancelled" and its exit, serve between a
// child's exit and the drop of its lease, a stdin line between the pipe
// and the loop), and a model test must hold one open to act inside it
// (go/tests/model/mbt/send_into_dying_child_test.go).
//
// Everything is a no-op unless BOUGH_TEST_HOLD_DIR is set: nothing a
// person runs sets it.
package testhold

import (
	"fmt"
	"os"
	"path/filepath"
	"time"
)

// Dir is BOUGH_TEST_HOLD_DIR, "" when holds are off.
func Dir() string { return os.Getenv("BOUGH_TEST_HOLD_DIR") }

// At parks while <dir>/<name> exists, after writing <name>.reached so
// the test knows the process got there.
func At(name string) {
	dir := Dir()
	if dir == "" {
		return
	}
	p := filepath.Join(dir, name)
	if _, err := os.Stat(p); err != nil {
		return
	}
	_ = os.WriteFile(p+".reached", nil, 0o644)
	for {
		if _, err := os.Stat(p); err != nil {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
}

// Line parks this process's n-th stdin line until the test lets it go:
// it writes <dir>/<pid>.line.<n> holding the text and waits for
// <pid>.line.<n>.go. The caller runs each line on its own goroutine, so
// the test chooses the order the lines are read in, and a line it never
// lets go dies with the process, unread.
func Line(n int, text string) {
	dir := Dir()
	if dir == "" {
		return
	}
	p := filepath.Join(dir, fmt.Sprintf("%d.line.%d", os.Getpid(), n))
	_ = os.WriteFile(p, []byte(text), 0o644)
	for {
		if _, err := os.Stat(p + ".go"); err == nil {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
}
