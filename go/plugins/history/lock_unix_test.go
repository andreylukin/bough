//go:build unix

package history

import (
	"os"
	"path/filepath"
	"syscall"
	"testing"
	"time"
)

// Not parallel: it shortens the package-level lockWait so the test
// costs 50 ms instead of the production 2 s, and parallel tests (which
// read lockWait on every Append) only start once the sequential ones
// have finished. A real-time bound rather than synctest on purpose: the
// regression this guards is a blocking flock, which inside a bubble
// would hang the test instead of failing it.
func TestAppendDoesNotHangOnHeldLock(t *testing.T) {
	old := lockWait
	lockWait = 50 * time.Millisecond
	t.Cleanup(func() { lockWait = old })
	path := filepath.Join(t.TempDir(), "s.jsonl")
	s, err := Open(path)
	if err != nil {
		t.Fatal(err)
	}
	f, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX); err != nil {
		t.Fatal(err)
	}
	defer syscall.Flock(int(f.Fd()), syscall.LOCK_UN)
	done := make(chan struct{})
	go func() { s.Append("input", map[string]any{"text": "hi"}); close(done) }()
	select {
	case <-done:
	case <-time.After(lockWait + 3*time.Second):
		t.Fatal("Append blocked on a lock held elsewhere")
	}
}
