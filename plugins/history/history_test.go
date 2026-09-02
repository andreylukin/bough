package history

import (
	"bufio"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func TestAppendReadBack(t *testing.T) {
	path := filepath.Join(t.TempDir(), "sub", "s.jsonl")
	s, err := Open(path)
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	s.Append("input", map[string]any{"text": "hello"})
	s.Append("assistant", map[string]any{"text": "hi"})
	s.Append("done", map[string]any{"text": ""})

	got := s.Entries()
	if len(got) != 3 {
		t.Fatalf("Entries() = %d, want 3", len(got))
	}
	if got[0].Kind != "input" || got[0].Data["text"] != "hello" {
		t.Fatalf("entry 0 = %+v", got[0])
	}
	if got[2].Kind != "done" {
		t.Fatalf("entry 2 = %+v", got[2])
	}
	if s.Path() != path {
		t.Fatalf("Path() = %q, want %q", s.Path(), path)
	}
	if err := s.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
}

func TestFileIsValidJSONLSeqMonotonic(t *testing.T) {
	path := filepath.Join(t.TempDir(), "s.jsonl")
	s, err := Open(path)
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	for i := 0; i < 5; i++ {
		e := s.Append("result", map[string]any{"text": "out", "code": "1+1"})
		if e.Seq != int64(i+1) {
			t.Fatalf("Append seq = %d, want %d", e.Seq, i+1)
		}
	}
	// Flushed per entry: readable before Close.
	f, err := os.Open(path)
	if err != nil {
		t.Fatalf("open file: %v", err)
	}
	defer f.Close()
	sc := bufio.NewScanner(f)
	var last int64
	n := 0
	for sc.Scan() {
		var e Entry
		if err := json.Unmarshal(sc.Bytes(), &e); err != nil {
			t.Fatalf("line %d not valid JSON: %v", n+1, err)
		}
		if e.Seq <= last {
			t.Fatalf("seq not monotonic: %d after %d", e.Seq, last)
		}
		if e.Kind != "result" || e.Data["code"] != "1+1" {
			t.Fatalf("bad entry: %+v", e)
		}
		last = e.Seq
		n++
	}
	if n != 5 {
		t.Fatalf("read %d lines, want 5", n)
	}
	s.Close()
}
