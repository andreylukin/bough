package memory

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

// Only a well-formed four-field line becomes a fact. A small model that
// wandered off the format has nothing worth saving in that line, and a
// guess would put junk in the graph forever.
func TestParseFacts(t *testing.T) {
	reply := `file:go/plugins/loop/loop.go | holds | tool:system-prompt | the base prompt is a const there
- decision:one-block-per-reply | replaces | decision:run-every-fence | one glm reply carried 138 blocks
this line is prose, not a fact
too | few | fields
no-kind | lives_in | file:x.go | subject has no kind
service:llm-small | used_by | tool:auto-memory | the extraction runs on it
repo:bough | prefers | person:andrey | a fourth fact, over the cap`

	got := ParseFacts(reply, 3)
	if len(got) != 3 {
		t.Fatalf("got %d facts, want 3 (the cap):\n%+v", len(got), got)
	}
	if got[0].Src != "file:go/plugins/loop/loop.go" || got[0].Rel != "holds" {
		t.Fatalf("first fact = %+v", got[0])
	}
	if got[1].Src != "decision:one-block-per-reply" {
		t.Fatalf("a list bullet must not survive into the subject: %+v", got[1])
	}
	if got[2].Src != "service:llm-small" {
		t.Fatalf("malformed lines were not skipped: %+v", got)
	}
	if n := len(ParseFacts("NOTHING", 3)); n != 0 {
		t.Fatalf("NOTHING yielded %d facts", n)
	}
}

// The digest is the turn's shape, not its bulk: the user's message, the
// agent's words, what it ran, the files. Tool output is left out.
func TestDigest(t *testing.T) {
	entries := []history.Entry{
		{Kind: "input", Data: map[string]any{"text": "an older turn"}},
		{Kind: "done", Data: map[string]any{}},
		{Kind: "input", Data: map[string]any{"text": "fix the flaky test"}},
		{Kind: "code", Data: map[string]any{"text": `tools.bash("go test ./...")`}},
		{Kind: "result", Data: map[string]any{"text": "ENORMOUS TOOL OUTPUT"}},
		{Kind: "assistant", Data: map[string]any{"text": "it was a stale golden file"}},
		{Kind: "done", Data: map[string]any{"files": []string{"golden_test.go"}}},
	}
	got := Digest(entries)
	for _, want := range []string{"fix the flaky test", "go test ./...", "stale golden file", "golden_test.go"} {
		if !strings.Contains(got, want) {
			t.Fatalf("digest missing %q:\n%s", want, got)
		}
	}
	if strings.Contains(got, "ENORMOUS") {
		t.Fatalf("tool output must stay out of the digest:\n%s", got)
	}
	if strings.Contains(got, "an older turn") {
		t.Fatalf("digest reached back past the last input:\n%s", got)
	}
}

type stubLLM struct {
	mu     sync.Mutex
	reply  string
	err    error
	calls  int
	system string
}

func (s *stubLLM) Complete(ctx context.Context, system string, msgs []llm.Message) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.calls++
	s.system = system
	return s.reply, s.err
}

type stubHist struct{ entries []history.Entry }

func (s *stubHist) Entries() []history.Entry { return s.entries }

func newMem(t *testing.T, l llm.LLM) (*Memory, string) {
	t.Helper()
	file := filepath.Join(t.TempDir(), "memory.md")
	return &Memory{
		llm: l, small: true, hist: &stubHist{entries: []history.Entry{
			{Kind: "input", Data: map[string]any{"text": "why is the gate red?"}},
			{Kind: "assistant", Data: map[string]any{"text": "a stale golden file"}},
			{Kind: "done", Data: map[string]any{}},
		}},
		file: file, maxFacts: 3, written: map[string]bool{}, ctx: context.Background(),
		emit: func(kind, text string) {},
	}, file
}

// A harvest writes each fact once: the same turn twice (or the same
// fact re-derived later) does not stack duplicates.
func TestHarvestWritesAndDedupes(t *testing.T) {
	l := &stubLLM{reply: "file:golden_test.go | holds | tool:stale-golden | the gate went red on a stale golden file"}
	m, file := newMem(t, l)

	m.harvest()
	m.harvest()

	body, err := os.ReadFile(file)
	if err != nil {
		t.Fatal(err)
	}
	if n := strings.Count(string(body), "stale-golden"); n != 1 {
		t.Fatalf("fact written %d times:\n%s", n, body)
	}
	if l.calls != 2 {
		t.Fatalf("the model was called %d times, want 2 (dedup is on the write, not the call)", l.calls)
	}
	if !strings.Contains(l.system, "worth remembering") {
		t.Fatalf("extraction prompt not used:\n%s", l.system)
	}
}

// A turn that established nothing writes nothing and says nothing: the
// transcript must not grow a row per turn.
func TestHarvestSilentOnNothing(t *testing.T) {
	l := &stubLLM{reply: "NOTHING"}
	m, file := newMem(t, l)
	var events []string
	m.emit = func(kind, text string) { events = append(events, kind) }

	m.harvest()
	if _, err := os.Stat(file); !os.IsNotExist(err) {
		t.Fatal("a file was written for nothing")
	}
	if len(events) != 0 {
		t.Fatalf("silent turn emitted %v", events)
	}
}

// A failed extraction is announced once and never breaks the turn (the
// turn is over by then).
func TestHarvestReportsFailure(t *testing.T) {
	l := &stubLLM{err: context.DeadlineExceeded}
	m, _ := newMem(t, l)
	var texts []string
	m.emit = func(kind, text string) { texts = append(texts, text) }
	m.harvest()
	if len(texts) != 1 || !strings.Contains(texts[0], "extraction failed") {
		t.Fatalf("events = %v", texts)
	}
}

// Two turns landing at once do not run two extractions: the second is
// skipped rather than queued behind a slow model.
func TestHarvestSkipsWhileBusy(t *testing.T) {
	release := make(chan struct{})
	l := &slowLLM{release: release, started: make(chan struct{})}
	m, _ := newMem(t, l)

	done := make(chan struct{})
	go func() { m.harvest(); close(done) }()
	<-l.started
	m.harvest() // must return at once
	close(release)
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("the first harvest never finished")
	}
	if l.calls.Load() != 1 {
		t.Fatalf("%d extractions ran, want 1", l.calls.Load())
	}
}

type slowLLM struct {
	started chan struct{}
	release chan struct{}
	once    sync.Once
	calls   atomic.Int32
}

func (s *slowLLM) Complete(ctx context.Context, system string, msgs []llm.Message) (string, error) {
	s.calls.Add(1)
	s.once.Do(func() { close(s.started) })
	<-s.release
	return "NOTHING", nil
}
