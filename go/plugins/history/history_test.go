package history

import (
	"bufio"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"
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

func TestResumeRoundTrip(t *testing.T) {
	path := filepath.Join(t.TempDir(), "s.jsonl")
	s, err := Open(path)
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	s.Append("input", map[string]any{"text": "one"})
	s.Append("assistant", map[string]any{"text": "two"})
	s.Append("done", map[string]any{"text": ""})
	if err := s.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	r, err := OpenExisting(path)
	if err != nil {
		t.Fatalf("OpenExisting: %v", err)
	}
	if got := r.Entries(); len(got) != 3 || got[0].Data["text"] != "one" {
		t.Fatalf("resumed entries = %+v", got)
	}
	e4 := r.Append("input", map[string]any{"text": "four"})
	e5 := r.Append("assistant", map[string]any{"text": "five"})
	if e4.Seq != 4 || e5.Seq != 5 {
		t.Fatalf("resumed seq = %d, %d; want 4, 5", e4.Seq, e5.Seq)
	}
	if err := r.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	// File is valid JSONL with seq 1..5 — append-only held.
	f, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	sc := bufio.NewScanner(f)
	var seq int64
	for sc.Scan() {
		var e Entry
		if err := json.Unmarshal(sc.Bytes(), &e); err != nil {
			t.Fatalf("bad line %q: %v", sc.Text(), err)
		}
		if e.Seq != seq+1 {
			t.Fatalf("seq %d after %d", e.Seq, seq)
		}
		seq = e.Seq
	}
	if seq != 5 {
		t.Fatalf("file has %d entries, want 5", seq)
	}
}

func TestOpenExistingMissingFails(t *testing.T) {
	if _, err := OpenExisting(filepath.Join(t.TempDir(), "nope.jsonl")); err == nil {
		t.Fatal("OpenExisting on missing file: want error, got nil")
	}
}

func TestListOrderTitlesCorruptTolerance(t *testing.T) {
	dir := t.TempDir()
	write := func(name string, mtime time.Time, lines ...string) {
		p := filepath.Join(dir, name)
		var b []byte
		for _, l := range lines {
			b = append(b, l...)
			b = append(b, '\n')
		}
		if err := os.WriteFile(p, b, 0o644); err != nil {
			t.Fatal(err)
		}
		if err := os.Chtimes(p, mtime, mtime); err != nil {
			t.Fatal(err)
		}
	}
	base := time.Now().Add(-time.Hour)
	write("older.jsonl", base,
		`{"seq":1,"kind":"input","data":{"text":"old question\nsecond line"}}`,
		`{"seq":2,"kind":"assistant","data":{"text":"old answer"}}`,
	)
	// Newer file, one corrupt line in the middle: skipped, not fatal.
	write("newer.jsonl", base.Add(time.Minute),
		`{"seq":1,"kind":"input","data":{"text":"new question"}}`,
		`not json at all {{{`,
		`{"seq":3,"kind":"done","data":{"text":""}}`,
	)

	infos, err := List(dir)
	if err != nil {
		t.Fatalf("List: %v", err)
	}
	if len(infos) != 2 {
		t.Fatalf("List returned %d infos, want 2: %+v", len(infos), infos)
	}
	if infos[0].ID != "newer" || infos[1].ID != "older" {
		t.Fatalf("order = %s, %s; want newer, older", infos[0].ID, infos[1].ID)
	}
	if infos[0].Entries != 2 {
		t.Fatalf("newer entries = %d, want 2 (corrupt line skipped)", infos[0].Entries)
	}
	if infos[0].Title != "new question" {
		t.Fatalf("newer title = %q", infos[0].Title)
	}
	if infos[1].Title != "old question" {
		t.Fatalf("older title = %q (want first line only)", infos[1].Title)
	}
	if infos[1].Entries != 2 {
		t.Fatalf("older entries = %d, want 2", infos[1].Entries)
	}
}

func TestListMissingDirEmpty(t *testing.T) {
	infos, err := List(filepath.Join(t.TempDir(), "nosuch"))
	if err != nil || len(infos) != 0 {
		t.Fatalf("List(missing) = %v, %v; want empty, nil", infos, err)
	}
}
