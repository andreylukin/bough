package ask

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// A file named for the open ask in the expire dir times it out now, the
// same way the timeout would: the call errors, nothing is answered, and
// the file is consumed. The ask/answer model test needs the timeout at a
// step it picks, and the real one is minutes.
func TestExpireDirTimesOutNow(t *testing.T) {
	t.Parallel()
	fn, a, _, hist, _ := mount(t, nil)
	dir := t.TempDir()
	a.expireDir = dir
	errc := make(chan error, 1)
	go func() { _, err := fn("still there?"); errc <- err }()
	deadline := time.Now().Add(5 * time.Second)
	for {
		a.mu.Lock()
		_, open := a.pending["ask-1"]
		a.mu.Unlock()
		if open {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("ask-1 never opened")
		}
		time.Sleep(5 * time.Millisecond)
	}
	select {
	case err := <-errc:
		t.Fatalf("returned before the expire file: %v", err)
	case <-time.After(100 * time.Millisecond):
	}
	file := filepath.Join(dir, "ask-1")
	if err := os.WriteFile(file, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-errc:
		if err == nil || !strings.Contains(err.Error(), "no answer after") {
			t.Fatalf("err = %v, want the timeout's", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("the expire file did not time the ask out")
	}
	if _, err := os.Stat(file); !os.IsNotExist(err) {
		t.Fatalf("expire file left behind: %v", err)
	}
	if err := a.Answer("ask-1", "late"); err == nil {
		t.Fatal("an expired ask still took an answer")
	}
	for _, e := range hist.all() {
		if e.Kind == "ask/answer" {
			t.Fatalf("an expired ask recorded an answer: %v", e.Data)
		}
	}
}
