package history

import (
	"os"
	"path/filepath"
	"reflect"
	"sync"
	"testing"
)

func TestListWithReadObservesOnlyDecodedSnapshots(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	p := filepath.Join(dir, "s.jsonl")
	body := `{"seq":1,"kind":"meta","data":{"cwd":"/work","origin":"web"}}
{"seq":2,"kind":"input","data":{"text":"hello"}}
invalid line
{"seq":3,"kind":"title","data":{"text":"first","summary":"summary"}}
{"seq":4,"kind":"title","data":{"text":"par`
	if err := os.WriteFile(p, []byte(body), 0600); err != nil {
		t.Fatal(err)
	}
	calls := 0
	observe := func(in SessionInfo, entries []Entry) {
		calls++
		direct, err := Read(p)
		if err != nil {
			t.Fatal(err)
		}
		if !reflect.DeepEqual(entries, direct) || in.Entries != len(entries) {
			t.Fatalf("snapshot: %+v %v", in, entries)
		}
		// Cache publication precedes the callback, with no lock held. A
		// consumer can look up metadata without parsing or deadlocking.
		cached, ok, err := Lookup(dir, "s")
		if err != nil || !ok || !reflect.DeepEqual(in, cached) {
			t.Fatalf("lookup: %+v %v %v", cached, ok, err)
		}
	}
	first, err := ListWithRead(dir, observe)
	if err != nil || len(first) != 1 || calls != 1 || first[0].Title != "first" || first[0].Entries != 3 {
		t.Fatalf("first: %+v %v calls=%d", first, err, calls)
	}
	warm, err := ListWithRead(dir, observe)
	if err != nil || !reflect.DeepEqual(warm, first) || calls != 1 {
		t.Fatalf("warm: %+v %v calls=%d", warm, err, calls)
	}
	// Finish the torn line, without changing any previously valid entry.
	f, err := os.OpenFile(p, os.O_APPEND|os.O_WRONLY, 0600)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = f.WriteString("tial\"}}\n"); err != nil {
		t.Fatal(err)
	}
	f.Close()
	changed, err := ListWithRead(dir, observe)
	if err != nil || calls != 2 || len(changed) != 1 || changed[0].Title != "partial" || changed[0].Summary != "summary" || changed[0].Entries != 4 {
		t.Fatalf("changed: %+v %v calls=%d", changed, err, calls)
	}
	plain, err := List(dir)
	if err != nil || !reflect.DeepEqual(plain, changed) {
		t.Fatalf("plain: %+v %v", plain, err)
	}
}

func TestListWithReadConcurrentReaders(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "s.jsonl"), []byte("{\"seq\":1,\"kind\":\"input\",\"data\":{\"text\":\"hello\"}}\n"), 0600); err != nil {
		t.Fatal(err)
	}
	var wg sync.WaitGroup
	for range 8 {
		wg.Go(func() {
			infos, err := ListWithRead(dir, func(in SessionInfo, es []Entry) {
				if in.Title != "hello" || len(es) != 1 {
					t.Errorf("snapshot: %+v %v", in, es)
				}
			})
			if err != nil || len(infos) != 1 || infos[0].Title != "hello" {
				t.Errorf("list: %+v %v", infos, err)
			}
		})
	}
	wg.Wait()
}
