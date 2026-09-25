package main

import (
	"os"
	"path/filepath"
	"sync/atomic"
	"testing"

	"github.com/fsnotify/fsnotify"
)

// Serve starts sessions side by side in one HOME, and each one's scratch
// row probes ~/.bough with a temp file it removes at once. On kqueue,
// watching a directory opens every entry in it, so a sibling's probe
// that vanished in between failed the watch, and the session exited
// before reading its first prompt.
func TestWatchDirSurvivesVanishingEntries(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	var stop atomic.Bool
	done := make(chan struct{})
	go func() {
		defer close(done)
		for !stop.Load() {
			f, err := os.CreateTemp(dir, ".probe-*")
			if err != nil {
				continue
			}
			f.Close()
			os.Remove(f.Name())
		}
	}()
	defer func() { stop.Store(true); <-done }()
	for range 300 {
		w, err := fsnotify.NewWatcher()
		if err != nil {
			t.Fatal(err)
		}
		err = watchDir(w, dir)
		w.Close()
		if err != nil {
			t.Fatalf("watchDir: %v", err)
		}
	}
	w, err := fsnotify.NewWatcher()
	if err != nil {
		t.Fatal(err)
	}
	defer w.Close()
	if err := watchDir(w, filepath.Join(dir, "missing")); err == nil {
		t.Fatal("watchDir on a missing directory: want an error")
	}
}
