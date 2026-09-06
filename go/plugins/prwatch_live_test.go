package plugins_test

import "testing"

// A deliberately failing test: pr-watch's live check should fix or remove it.
func TestPRWatchLive(t *testing.T) {
	t.Fatal("pr-watch live test: this test is meant to fail until the watcher fixes CI")
}
