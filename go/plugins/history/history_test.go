package history

import (
	"bufio"
	"encoding/json"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
)

// Apply on a missing {file: path} creates it (with the meta entry)
// instead of failing with a raw open error; List exposes the cwd.
func TestApplyCreatesMissingFileWithMeta(t *testing.T) {
	ctx := kernel.NewContext()
	p := filepath.Join(t.TempDir(), "sub", "named.jsonl")
	if err := (plugin{}).Apply(ctx, map[string]any{"file": p}); err != nil {
		t.Fatalf("Apply on a missing file: %v", err)
	}
	s, err := kernel.Get[*Store](ctx, "history")
	if err != nil {
		t.Fatal(err)
	}
	s.Append("input", map[string]any{"text": "hello"})
	ctx.Unmount()
	if _, err := os.Stat(p); err != nil {
		t.Fatalf("named file must survive unmount: %v", err)
	}
	infos, err := List(filepath.Dir(p))
	if err != nil || len(infos) != 1 {
		t.Fatalf("List = %v, %v", infos, err)
	}
	cwd, _ := os.Getwd()
	if infos[0].Cwd != cwd {
		t.Fatalf("Cwd = %q, want %q", infos[0].Cwd, cwd)
	}
	if infos[0].Entries != 2 || infos[0].Title != "hello" {
		t.Fatalf("info = %+v", infos[0])
	}
}

func TestFreshSessionOnlyMetaIsRemoved(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	ctx := kernel.NewContext()
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	s, err := kernel.Get[*Store](ctx, "history")
	if err != nil {
		t.Fatal(err)
	}
	if es := s.Entries(); len(es) != 1 || es[0].Kind != "meta" {
		t.Fatalf("fresh session entries = %+v, want one meta", es)
	}
	ctx.Unmount()
	if _, err := os.Stat(s.Path()); !os.IsNotExist(err) {
		t.Fatalf("meta-only fresh session should be removed, stat = %v", err)
	}
}

func TestPreferCwdAndLastPrompt(t *testing.T) {
	infos := []SessionInfo{
		{ID: "a", Cwd: "/x"}, {ID: "b", Cwd: "/here"}, {ID: "c"}, {ID: "d", Cwd: "/here"},
	}
	got := PreferCwd(infos, "/here")
	if ids := got[0].ID + got[1].ID + got[2].ID + got[3].ID; ids != "bdac" {
		t.Fatalf("PreferCwd order = %s, want bdac", ids)
	}
	if infos[0].ID != "a" {
		t.Fatal("PreferCwd must not reorder its input")
	}
	es := []Entry{
		{Kind: "input", Data: map[string]any{"text": "first"}},
		{Kind: "input", Data: map[string]any{"text": "last\nmore"}},
		{Kind: "assistant", Data: map[string]any{"text": "reply"}},
	}
	if lp := LastPrompt(es); lp != "last" {
		t.Fatalf("LastPrompt = %q", lp)
	}
	if lp := LastPrompt(nil); lp != "" {
		t.Fatalf("LastPrompt(nil) = %q", lp)
	}
}

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
	for i := range 5 {
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

// Session ids are UUIDv7s: unique, and still sorting oldest-first by
// their leading timestamp so a name-ordered listing stays chronological.
func TestNewIDIsSortableAndUnique(t *testing.T) {
	seen := map[string]bool{}
	var ids []string
	for range 50 {
		id := NewID()
		if seen[id] {
			t.Fatalf("duplicate session id %s", id)
		}
		seen[id] = true
		ids = append(ids, id)
		if len(id) != 36 || strings.Count(id, "-") != 4 {
			t.Fatalf("not a uuid: %q", id)
		}
		if v := id[14]; v != '7' {
			t.Fatalf("uuid %s is version %c, want 7", id, v)
		}
	}
	if !slices.IsSorted(ids) {
		t.Fatal("ids do not sort in creation order")
	}
}

// A UUIDv7 name has no ":" and so needs no ref sanitizing, but old
// RFC3339-named sessions still resolve to the same refs they always did.
func TestTurnRefAcceptsBothNameStyles(t *testing.T) {
	if got := TurnRef("0199b0f1-2c3d-7a4b-8c5d-6e7f80912345", 4); got != "refs/bough/turns/0199b0f1-2c3d-7a4b-8c5d-6e7f80912345/4" {
		t.Fatalf("TurnRef = %q", got)
	}
	if got := TurnRef("2026-09-03T20:56:08Z-74482", 4); got != "refs/bough/turns/2026-09-03T20-56-08Z-74482/4" {
		t.Fatalf("TurnRef = %q", got)
	}
}

// A named session (the session-title plugin's "title" entry) lists
// under its name, not its opening sentence; an unnamed one still falls
// back to the first line of the first input.
func TestListPrefersTheTitleEntry(t *testing.T) {
	dir := t.TempDir()
	named := filepath.Join(dir, "named.jsonl")
	os.WriteFile(named, []byte(
		`{"seq":1,"kind":"input","data":{"text":"the gate is red, find out why\nand fix it"}}`+"\n"+
			`{"seq":2,"kind":"title","data":{"text":"Fix the flaky golden test"}}`+"\n"), 0o644)
	plain := filepath.Join(dir, "plain.jsonl")
	os.WriteFile(plain, []byte(`{"seq":1,"kind":"input","data":{"text":"just asking\nsecond line"}}`+"\n"), 0o644)

	infos, err := List(dir)
	if err != nil {
		t.Fatal(err)
	}
	got := map[string]string{}
	for _, in := range infos {
		got[in.ID] = in.Title
	}
	if got["named"] != "Fix the flaky golden test" {
		t.Fatalf("named session title = %q", got["named"])
	}
	if got["plain"] != "just asking" {
		t.Fatalf("unnamed session title = %q", got["plain"])
	}
}

// The log is append-only, so it must not drop its tail. On SIGTERM the
// history row unmounts and closes the file while the loop is still
// finishing its turn; every append after that used to fail with "file
// already closed" and be lost — the last entries of the session, which
// is the part someone resuming most wants.
func TestAppendAfterCloseStillReachesDisk(t *testing.T) {
	path := filepath.Join(t.TempDir(), "s.jsonl")
	s, err := Open(path)
	if err != nil {
		t.Fatal(err)
	}
	s.Append("input", map[string]any{"text": "before close"})
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	// What the loop does on its way out: the turn's last entries.
	s.Append("assistant", map[string]any{"text": "after close"})
	s.Append("done", map[string]any{})

	entries, err := readEntries(path)
	if err != nil {
		t.Fatal(err)
	}
	var kinds []string
	for _, e := range entries {
		kinds = append(kinds, e.Kind)
	}
	if len(entries) != 3 {
		t.Fatalf("every entry should be on disk, got %v", kinds)
	}
	if txt, _ := entries[1].Data["text"].(string); txt != "after close" {
		t.Errorf("the post-close entry is wrong: %v", entries[1].Data)
	}
	// Seq keeps counting, so a resume does not renumber.
	for i, e := range entries {
		if e.Seq != int64(i+1) {
			t.Errorf("entry %d has seq %d", i, e.Seq)
		}
	}
}

// Unmount can run more than once; closing twice is not an error.
func TestCloseIsIdempotent(t *testing.T) {
	s, err := Open(filepath.Join(t.TempDir(), "s.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	if err := s.Close(); err != nil {
		t.Errorf("second close: %v", err)
	}
}
