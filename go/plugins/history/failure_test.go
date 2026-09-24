package history

import (
	"bytes"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

func kindsOf(t *testing.T, path string) []string {
	t.Helper()
	es, err := Read(path)
	if err != nil {
		t.Fatal(err)
	}
	var ks []string
	for _, e := range es {
		ks = append(ks, e.Kind)
	}
	return ks
}

// A failed append stays pending and lands, in order, once the disk
// frees; a fragment a cut write left is dropped first, so the line after
// it is not glued to it and lost on read. The sink hears the failure and
// the recovery once each.
func TestAppendKeepsFailedLinesPending(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	path := filepath.Join(dir, "s.jsonl")
	s, err := Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	s.faultDir = dir
	var got []string
	s.SetErrorSink(func(err error) { got = append(got, err.Error()) })
	s.Append("input", nil)
	full := filepath.Join(dir, "full")
	os.WriteFile(full, nil, 0o644)
	s.Append("assistant", nil)
	s.Append("done", nil)
	if k := kindsOf(t, path); strings.Join(k, " ") != "input" {
		t.Fatalf("file while full: %v", k)
	}
	os.Remove(full)
	f, _ := os.OpenFile(path, os.O_WRONLY|os.O_APPEND, 0)
	f.WriteString(`{"seq":9,"kind":"cut`)
	f.Close()
	s.Flush()
	if k := kindsOf(t, path); strings.Join(k, " ") != "input assistant done" {
		t.Fatalf("file after the retry: %v", k)
	}
	if b, _ := os.ReadFile(path); bytes.Contains(b, []byte("cut")) {
		t.Fatalf("the torn fragment is still there:\n%s", b)
	}
	if len(got) != 2 || !strings.Contains(got[0], "no space") || got[1] != (Saved{}).Error() {
		t.Fatalf("sink got %q, want the ENOSPC then Saved", got)
	}
}

// An entry over bufio.Scanner's 4 MiB cap reads back, and so does the
// file around it.
func TestHugeEntryReadsBack(t *testing.T) {
	t.Parallel()
	path := filepath.Join(t.TempDir(), "s.jsonl")
	s, err := Open(path)
	if err != nil {
		t.Fatal(err)
	}
	s.Append("input", nil)
	s.Append("result", map[string]any{"text": strings.Repeat("x", 5<<20)})
	s.Append("done", nil)
	s.Close()
	if k := kindsOf(t, path); strings.Join(k, " ") != "input result done" {
		t.Fatalf("read back %v", k)
	}
	if _, err := OpenExisting(path); err != nil {
		t.Fatal(err)
	}
}

// An unreadable sessions dir is an error, not an empty list; an
// unreadable file is still listed.
func TestListReportsUnreadable(t *testing.T) {
	t.Parallel()
	if runtime.GOOS == "windows" || os.Getuid() == 0 {
		t.Skip("chmod does not deny here")
	}
	dir := t.TempDir()
	for _, id := range []string{"a", "b"} {
		s, err := Open(filepath.Join(dir, id+".jsonl"))
		if err != nil {
			t.Fatal(err)
		}
		s.Append("meta", nil)
		s.Close()
	}
	os.Chmod(filepath.Join(dir, "b.jsonl"), 0)
	defer os.Chmod(filepath.Join(dir, "b.jsonl"), 0o644)
	infos, err := List(dir)
	if err != nil || len(infos) != 2 {
		t.Fatalf("List with an unreadable file = %d sessions, %v; want both", len(infos), err)
	}
	os.Chmod(dir, 0)
	defer os.Chmod(dir, 0o755)
	if infos, err := List(dir); err == nil {
		t.Fatalf("List of an unreadable dir = %d sessions, no error", len(infos))
	}
}
