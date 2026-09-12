package ui

// The picker's query searches transcripts, not only titles: a title is
// the session's first prompt, and what you remember about a session is
// usually something said in the middle of it.

import (
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// storedSession writes a real session file and returns the row that
// lists it, so the picker reads the same JSONL the launcher would.
func storedSession(t *testing.T, dir, id, title string, said ...string) history.SessionInfo {
	t.Helper()
	p := filepath.Join(dir, id+".jsonl")
	s, err := history.Open(p)
	if err != nil {
		t.Fatal(err)
	}
	s.Append("meta", map[string]any{"cwd": dir})
	s.Append("input", map[string]any{"text": title})
	for _, line := range said {
		s.Append("assistant", map[string]any{"text": line})
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	return history.SessionInfo{
		ID: id, Path: p, Title: title, Entries: 2 + len(said),
		ModTime: time.Now(),
	}
}

func pickerWith(t *testing.T, rows ...history.SessionInfo) *drv {
	t.Helper()
	cfg := cfgWith(t, nil, nil, nil)
	cfg.picker = true
	cfg.sessions = rows
	cfg.choose = func(string) {}
	return newDrv(t, 100, 24, cfg)
}

func TestPickerQueryMatchesTranscriptNotOnlyTitle(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	a := storedSession(t, dir, "aaa", "look at the deploy", "nothing of note here")
	b := storedSession(t, dir, "bbb", "check the queue", "the rate limiter drops the third retry")
	d := pickerWith(t, a, b)

	d.typeStr("rate limiter")
	p := d.plain()
	if !strings.Contains(p, "check the queue") {
		t.Errorf("a word said mid-session must find it:\n%s", p)
	}
	if strings.Contains(p, "look at the deploy") {
		t.Errorf("a session that never said it must drop out:\n%s", p)
	}
}

func TestPickerQueryStillMatchesTitles(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	a := storedSession(t, dir, "aaa", "refactor the loop")
	b := storedSession(t, dir, "bbb", "fix the parser")
	d := pickerWith(t, a, b)

	d.typeStr("refactor")
	p := d.plain()
	if !strings.Contains(p, "refactor the loop") || strings.Contains(p, "fix the parser") {
		t.Errorf("title matching must survive the transcript search:\n%s", p)
	}
}

func TestPickerQueryMatchingNothingSaysSo(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	d := pickerWith(t, storedSession(t, dir, "aaa", "refactor the loop", "some output"))

	d.typeStr("kubernetes")
	if p := d.plain(); !strings.Contains(p, "no matching sessions") {
		t.Errorf("an unmatched query should say so:\n%s", p)
	}
}

// The corpus is memoized per row set; a different set must not be
// filtered against the previous set's text.
func TestPickerCorpusRebuildsWhenRowsChange(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	first := storedSession(t, dir, "aaa", "first session", "alpha marker")
	d := pickerWith(t, first)
	d.typeStr("alpha")
	if p := d.plain(); !strings.Contains(p, "first session") {
		t.Fatalf("precondition: alpha should match the first row set:\n%s", p)
	}

	second := storedSession(t, dir, "bbb", "second session", "beta marker")
	d.m.sessRows = sessList{second} // what a mid-session re-read installs
	rows := d.m.pickerRows(d.m.cfg.Load())
	if len(rows) != 0 {
		t.Errorf("stale corpus: %q matched the new row set %+v", d.m.pickQuery, rows)
	}
	d.m.pickQuery = "beta"
	if rows := d.m.pickerRows(d.m.cfg.Load()); len(rows) != 1 || rows[0].ID != "bbb" {
		t.Errorf("the new row set's own text should match, got %+v", rows)
	}
}
