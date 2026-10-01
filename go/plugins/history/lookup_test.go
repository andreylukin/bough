package history

import (
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

func TestLookupMatchesListWithoutReadingOtherSessions(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	body := []byte(`{"seq":1,"kind":"meta","data":{"cwd":"/work","origin":"web","repo":"/work","branch":"topic","spawned_by":"parent","forked_from":"source.jsonl","at_seq":7}}
{"seq":2,"kind":"input","data":{"text":"expanded prompt","typed":"hello\nsecond line"}}
invalid line
{"seq":3,"kind":"title","data":{"text":"Session title","summary":"A summary"}}
`)
	for _, id := range []string{"selected", "unrelated"} {
		if err := os.WriteFile(filepath.Join(dir, id+".jsonl"), body, 0o600); err != nil {
			t.Fatal(err)
		}
	}
	got, ok, err := Lookup(dir, "selected")
	if err != nil || !ok {
		t.Fatalf("lookup = %+v, %v, %v", got, ok, err)
	}
	listMu.Lock()
	_, touched := listCache[filepath.Join(dir, "unrelated.jsonl")]
	listMu.Unlock()
	if touched {
		t.Fatal("lookup decoded an unrelated transcript")
	}
	if got.Title != "Session title" || got.Entries != 3 {
		t.Fatalf("lookup = %+v", got)
	}
	all, err := List(dir)
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, in := range all {
		if in.ID == got.ID {
			found = true
		}
		if in.ID == got.ID && !reflect.DeepEqual(in, got) {
			t.Fatalf("list = %+v; lookup = %+v", in, got)
		}
	}
	if !found {
		t.Fatal("selected session missing from List")
	}
	// The shared cache must invalidate on an append, whichever caller warmed it.
	body = append(body, []byte("{\"seq\":4,\"kind\":\"title\",\"data\":{\"text\":\"Renamed\"}}\n")...)
	if err := os.WriteFile(got.Path, body, 0o600); err != nil {
		t.Fatal(err)
	}
	got, ok, err = Lookup(dir, "selected")
	if err != nil || !ok || got.Title != "Renamed" || got.Entries != 4 {
		t.Fatalf("changed lookup = %+v, %v, %v", got, ok, err)
	}
}

func TestLookupMissingAndTraversal(t *testing.T) {
	t.Parallel()
	root := t.TempDir()
	dir := filepath.Join(root, "history")
	if err := os.Mkdir(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "outside.jsonl"), []byte("{}\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	for _, id := range []string{"", ".", "..", "missing", "../outside", filepath.Join(root, "outside"), "nested/session"} {
		if in, ok, err := Lookup(dir, id); err != nil || ok {
			t.Errorf("lookup(%q) = %+v, %v, %v", id, in, ok, err)
		}
	}
	if _, ok, err := Lookup(filepath.Join(root, "missing"), "s"); err != nil || ok {
		t.Fatalf("missing dir = %v, %v", ok, err)
	}
	// A failed lookup must remain distinguishable from a missing session.
	if _, _, err := Lookup(filepath.Join(root, "outside.jsonl"), "s"); err == nil {
		t.Fatal("file as directory did not fail")
	}
}
