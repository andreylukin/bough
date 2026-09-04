package title

import (
	"context"
	"strings"
	"sync"
	"testing"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

// A small model wraps a title in quotes, a full stop, a preamble line.
// Clean takes the name out of whatever came back.
func TestClean(t *testing.T) {
	cases := map[string]string{
		`Fix the flaky golden test`:                    "Fix the flaky golden test",
		`"Fix the flaky golden test."`:                 "Fix the flaky golden test",
		"**Fix the flaky golden test**":                "Fix the flaky golden test",
		"Fix the flaky golden test\n\nThis names the…": "Fix the flaky golden test",
		strings.Repeat("very long title ", 10):         strings.TrimSpace(strings.Repeat("very long title ", 10)[:60]) + "…",
	}
	for in, want := range cases {
		if got := Clean(in); got != want {
			t.Fatalf("Clean(%q) = %q, want %q", in, got, want)
		}
	}
}

type stubLLM struct {
	mu    sync.Mutex
	reply string
	calls int
}

func (s *stubLLM) Complete(ctx context.Context, system string, msgs []llm.Message) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.calls++
	return s.reply, nil
}

type memHist struct {
	mu      sync.Mutex
	entries []history.Entry
}

func (m *memHist) Entries() []history.Entry {
	m.mu.Lock()
	defer m.mu.Unlock()
	return append([]history.Entry(nil), m.entries...)
}
func (m *memHist) Append(kind string, data map[string]any) history.Entry {
	m.mu.Lock()
	defer m.mu.Unlock()
	e := history.Entry{Kind: kind, Data: data}
	m.entries = append(m.entries, e)
	return e
}

// One cheap call per session, not per turn: the name is recorded once
// and a second turn does not buy another.
func TestNamesOnce(t *testing.T) {
	l := &stubLLM{reply: "Fix the flaky golden test"}
	h := &memHist{entries: []history.Entry{
		{Kind: "input", Data: map[string]any{"text": "the gate is red, find out why"}},
		{Kind: "done", Data: map[string]any{}},
	}}
	var emitted []string
	tr := &Titler{llm: l, hist: h, ctx: context.Background(),
		emit: func(kind, text string) { emitted = append(emitted, kind+":"+text) }}

	tr.name()
	tr.name()

	if l.calls != 1 {
		t.Fatalf("the model was called %d times, want 1", l.calls)
	}
	var titles int
	for _, e := range h.Entries() {
		if e.Kind == "title" {
			titles++
			if e.Data["text"] != "Fix the flaky golden test" {
				t.Fatalf("recorded %v", e.Data)
			}
		}
	}
	if titles != 1 {
		t.Fatalf("%d title entries", titles)
	}
	if len(emitted) != 1 || !strings.HasPrefix(emitted[0], "title:") {
		t.Fatalf("emitted %v", emitted)
	}
}

// A resumed session already has its name: no call, no second title.
func TestResumedSessionKeepsItsName(t *testing.T) {
	l := &stubLLM{reply: "A new name"}
	h := &memHist{entries: []history.Entry{
		{Kind: "input", Data: map[string]any{"text": "the gate is red"}},
		{Kind: "title", Data: map[string]any{"text": "Fix the flaky golden test"}},
		{Kind: "input", Data: map[string]any{"text": "now push it"}},
	}}
	tr := &Titler{llm: l, hist: h, ctx: context.Background(), emit: func(string, string) {}}
	tr.name()
	if l.calls != 0 {
		t.Fatalf("a named session was renamed (%d calls)", l.calls)
	}
}

// A session with nothing said yet is not named.
func TestEmptySessionNotNamed(t *testing.T) {
	l := &stubLLM{reply: "Something"}
	tr := &Titler{llm: l, hist: &memHist{}, ctx: context.Background(), emit: func(string, string) {}}
	tr.name()
	if l.calls != 0 {
		t.Fatalf("named an empty session (%d calls)", l.calls)
	}
}
