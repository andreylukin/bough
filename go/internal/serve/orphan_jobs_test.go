package serve

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/andreylukin/bough/plugins/history"
)

// A job still running when its child dies died with it: the reap says
// so in history, or RunningJobs lists it again under the next child and
// the footer waits on it forever.
func TestEndOrphanJobs(t *testing.T) {
	t.Parallel()
	path := filepath.Join(t.TempDir(), "s.jsonl")
	if err := os.WriteFile(path, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	for _, d := range []map[string]any{
		{"id": 1, "event": "started", "cmd": "make build", "call": "c1"},
		{"id": 2, "event": "started", "cmd": "sleep 1"},
		{"id": 2, "event": "finished", "cmd": "sleep 1", "exit": 0},
	} {
		if _, err := history.AppendFile(path, "job", d); err != nil {
			t.Fatal(err)
		}
	}
	if err := endOrphanJobs(path); err != nil {
		t.Fatal(err)
	}
	entries, err := history.Read(path)
	if err != nil {
		t.Fatal(err)
	}
	if got := RunningJobs(entries, true); len(got) != 0 {
		t.Fatalf("still running after the child died: %v", got)
	}
	if len(entries) != 4 {
		t.Fatalf("want one finish appended, have %d entries", len(entries))
	}
	last := entries[3].Data
	if last["event"] != "finished" || last["stopped"] != true || last["call"] != "c1" || num(last["id"]) != 1 {
		t.Fatalf("finish = %v, want job 1 stopped with its call", last)
	}
	// Nothing left to end: a second reap writes nothing.
	if err := endOrphanJobs(path); err != nil {
		t.Fatal(err)
	}
	if again, _ := history.Read(path); len(again) != 4 {
		t.Fatalf("second reap appended: %d entries", len(again))
	}
}
