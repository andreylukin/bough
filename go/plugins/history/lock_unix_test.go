//go:build unix

package history

import (
	"os"
	"path/filepath"
	"syscall"
	"testing"
	"time"
)

func TestAppendDoesNotHangOnHeldLock(t *testing.T) {
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
