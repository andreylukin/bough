package history

import (
	"path/filepath"
	"testing"

	"github.com/andreylukin/bough/kernel"
)

// AppendFile chains from the file's last line, and a Store holding the
// file numbers its next entry past it.
func TestAppendFileWhileStoreHolds(t *testing.T) {
	t.Parallel()
	path := filepath.Join(t.TempDir(), "s.jsonl")
	s, err := Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	s.Append("meta", map[string]any{"cwd": "/"})
	s.Append("input", map[string]any{"text": "hi"})
	e, err := AppendFile(path, "notice", map[string]any{"id": "n1"})
	if err != nil {
		t.Fatal(err)
	}
	if e.Seq != 3 || e.Parent != 2 {
		t.Fatalf("AppendFile entry seq=%d parent=%d, want 3/2", e.Seq, e.Parent)
	}
	if next := s.Append("assistant", map[string]any{"text": "yo"}); next.Seq != 4 {
		t.Fatalf("store seq after AppendFile = %d, want 4", next.Seq)
	}
	es, err := ReadFile(path)
	if err != nil || len(es) != 4 || es[2].Kind != "notice" {
		t.Fatalf("ReadFile = %v, %v", es, err)
	}
}

func TestAppendFileMissing(t *testing.T) {
	t.Parallel()
	if _, err := AppendFile(filepath.Join(t.TempDir(), "none.jsonl"), "notice", nil); err == nil {
		t.Fatal("AppendFile on a missing file succeeded")
	}
}

// A fresh session honours session-id and records session-spawned-by.
func TestApplySessionIDAndSpawnedBy(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	path := filepath.Join(dir, "child-1.jsonl")
	ctx := kernel.NewContext()
	ctx.Provide("session-spawned-by", "parent-1")
	if err := (plugin{}).Apply(ctx, map[string]any{"file": path}); err != nil {
		t.Fatal(err)
	}
	rec, err := kernel.Get[func(string, map[string]any)](ctx, "history-record")
	if err != nil {
		t.Fatalf("history-record: %v", err)
	}
	rec("input", map[string]any{"text": "task"})
	ctx.Unmount()
	infos, err := List(dir)
	if err != nil || len(infos) != 1 || infos[0].SpawnedBy != "parent-1" || infos[0].ID != "child-1" {
		t.Fatalf("List = %+v, %v", infos, err)
	}
}
