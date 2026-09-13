package title

import (
	"context"
	"fmt"
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
		`Fix the flaky golden test`:                             "Fix the flaky golden test",
		"Fix \x1b]0;PWNED\x07 the\x1b[31m red\u202e flaky test": "Fix the red flaky test",
		`"Fix the flaky golden test."`:                          "Fix the flaky golden test",
		"**Fix the flaky golden test**":                         "Fix the flaky golden test",
		"Fix the flaky golden test\n\nThis names the…":          "Fix the flaky golden test",
		strings.Repeat("very long title ", 10):                  strings.TrimSpace(strings.Repeat("very long title ", 10)[:60]) + "…",
		strings.Repeat("ש", 59) + "🚀 tail":                      strings.Repeat("ש", 59) + "🚀…",
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

func done() history.Entry { return history.Entry{Kind: "done", Data: map[string]any{}} }
func input(t string) history.Entry {
	return history.Entry{Kind: "input", Data: map[string]any{"text": t}}
}

func titles(h *memHist) []history.Entry {
	var out []history.Entry
	for _, e := range h.Entries() {
		if e.Kind == "title" {
			out = append(out, e)
		}
	}
	return out
}

// A session is named after its first turn, then renamed as it grows —
// at turns 3 and 8, then every 10th — never once per turn.
func TestDue(t *testing.T) {
	cases := []struct {
		turns, named int
		want         bool
	}{
		{0, 0, false}, {1, 0, true}, {1, 1, false}, {2, 1, false}, {3, 1, true},
		{5, 3, false}, {8, 3, true}, {9, 8, false}, {10, 8, true}, {15, 10, false},
		{20, 10, true}, {25, 0, true}, {4, 0, true},
	}
	for _, c := range cases {
		if got := due(c.turns, c.named); got != c.want {
			t.Errorf("due(%d turns, named at %d) = %v, want %v", c.turns, c.named, got, c.want)
		}
	}
}

func TestParse(t *testing.T) {
	title, summary := Parse("Title: \"Rework the serve control room.\"\nSummary: The user is reshaping the web UI.\nJobs and cache now show.")
	if title != "Rework the serve control room" || summary != "The user is reshaping the web UI. Jobs and cache now show." {
		t.Fatalf("Parse = %q, %q", title, summary)
	}
	// A model that ignored the format still yields its name.
	if title, summary := Parse("Fix the flaky golden test"); title != "Fix the flaky golden test" || summary != "" {
		t.Fatalf("bare reply: %q, %q", title, summary)
	}
}

// The name follows the conversation: the second naming reads every
// request, not only the first, and records the turn it was written at.
func TestRenamesAsTheSessionGrows(t *testing.T) {
	l := &stubLLM{reply: "Title: Fix the flaky golden test\nSummary: Chasing a red gate."}
	h := &memHist{entries: []history.Entry{input("the gate is red, find out why"), done()}}
	var emitted []string
	tr := &Titler{llm: l, hist: h, ctx: context.Background(),
		emit: func(kind, text string) { emitted = append(emitted, kind+":"+text) }}

	tr.name()
	tr.name() // same turn: not due again
	if l.calls != 1 || len(titles(h)) != 1 {
		t.Fatalf("after turn 1: %d calls, %d titles", l.calls, len(titles(h)))
	}
	if got := titles(h)[0].Data; got["summary"] != "Chasing a red gate." || got["turn"] != 1 {
		t.Fatalf("recorded %v", got)
	}

	h.entries = append(h.entries, input("now refactor the loader"), done(), input("and ship it"), done())
	l.reply = "Title: Refactor and ship the loader\nSummary: Fixed the gate, then refactored the loader."
	tr.name()
	ts := titles(h)
	if l.calls != 2 || len(ts) != 2 || ts[1].Data["text"] != "Refactor and ship the loader" || ts[1].Data["turn"] != 3 {
		t.Fatalf("after turn 3: %d calls, titles %v", l.calls, ts)
	}
	if len(emitted) != 2 || !strings.HasPrefix(emitted[1], "title:") {
		t.Fatalf("emitted %v", emitted)
	}
}

// A resumed session carries on the schedule from its last naming; one
// named before titles recorded their turn counts as named at turn 1.
func TestResumedSessionKeepsItsSchedule(t *testing.T) {
	l := &stubLLM{reply: "Title: A new name"}
	h := &memHist{entries: []history.Entry{
		input("the gate is red"), done(),
		{Kind: "title", Data: map[string]any{"text": "Fix the flaky golden test"}},
		input("now push it"), done(),
	}}
	tr := &Titler{llm: l, hist: h, ctx: context.Background(), emit: func(string, string) {}}
	tr.name()
	if l.calls != 0 {
		t.Fatalf("renamed at turn 2 (%d calls)", l.calls)
	}
}

// A session with nothing said yet is not named.
func TestEmptySessionNotNamed(t *testing.T) {
	l := &stubLLM{reply: "Something"}
	tr := &Titler{llm: l, hist: &memHist{entries: []history.Entry{done()}}, ctx: context.Background(), emit: func(string, string) {}}
	tr.name()
	if l.calls != 0 {
		t.Fatalf("named an empty session (%d calls)", l.calls)
	}
}

// A long session keeps its first request and the most recent ones.
func TestDigestKeepsTheEnds(t *testing.T) {
	var es []history.Entry
	for i := 0; i < 40; i++ {
		es = append(es, input(fmt.Sprintf("request %02d %s", i, strings.Repeat("x", 380))))
	}
	d := digest(es)
	if len(d) > maxInput+200 || !strings.Contains(d, "request 00") || !strings.Contains(d, "request 39") || strings.Contains(d, "request 05") {
		t.Fatalf("digest kept the wrong requests (%d bytes)", len(d))
	}
}
