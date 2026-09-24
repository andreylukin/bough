package testhold

import (
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// At parks only while its file exists, and says it got there; Line
// parks until its .go appears. Not parallel: they read the environment.
func TestHolds(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("BOUGH_TEST_HOLD_DIR", dir)

	At("absent") // no file: returns at once
	if _, err := os.Stat(filepath.Join(dir, "absent.reached")); err == nil {
		t.Fatal("a point with no hold file wrote .reached")
	}

	p := filepath.Join(dir, "x")
	if err := os.WriteFile(p, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	done := make(chan struct{})
	go func() { At("x"); close(done) }()
	waitFor(t, p+".reached")
	select {
	case <-done:
		t.Fatal("At returned while its hold file exists")
	case <-time.After(50 * time.Millisecond):
	}
	os.Remove(p)
	<-done

	line := make(chan struct{})
	go func() { Line(3, "hello"); close(line) }()
	lp := filepath.Join(dir, fmt.Sprintf("%d.line.3", os.Getpid()))
	waitFor(t, lp)
	if b, _ := os.ReadFile(lp); string(b) != "hello" {
		t.Fatalf("parked line holds %q, want hello", b)
	}
	select {
	case <-line:
		t.Fatal("Line returned before its .go")
	case <-time.After(50 * time.Millisecond):
	}
	os.WriteFile(lp+".go", nil, 0o644)
	<-line
}

func TestOffWithoutTheDir(t *testing.T) {
	t.Setenv("BOUGH_TEST_HOLD_DIR", "")
	At("anything")
	Line(1, "x") // would park forever if holds were on
}

func waitFor(t *testing.T, p string) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for {
		if _, err := os.Stat(p); err == nil {
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("%s never appeared", p)
		}
		time.Sleep(5 * time.Millisecond)
	}
}
