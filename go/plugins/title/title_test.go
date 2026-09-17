package title

import (
	"context"
	"fmt"
	"io"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

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
		strings.Repeat("very long title ", 10):                  strings.Repeat("very long title ", 3) + "very long",
		strings.Repeat("ש", 59) + "🚀 tail":                      strings.Repeat("ש", 59) + "🚀",
	}
	for in, want := range cases {
		if got := Clean(in); got != want {
			t.Fatalf("Clean(%q) = %q, want %q", in, got, want)
		}
	}
}

// A log line is one line of at most 16 words, whatever came back.
func TestCleanLine(t *testing.T) {
	long := "You " + strings.Repeat("word ", 30)
	cases := map[string]string{
		"You fixed gate; agent ran go test, exit 0.":               "You fixed gate; agent ran go test, exit 0.",
		"\n\n3. You fixed gate; agent ran go test.\nExplanation…":  "You fixed gate; agent ran go test.",
		"You \x1b]2;PWNED\x07asked\u202e why; agent found \x07bug": "You asked why; agent found bug",
		`"You shared serve.go; agent awaits review."`:              "You shared serve.go; agent awaits review.",
		long: "You " + strings.TrimSpace(strings.Repeat("word ", 15)) + "…",
		"You asked start `sleep 120; echo A` and `sleep 200; echo B`; agent started both with jobs 1 and 2 running now, exit 0": "You asked start `sleep 120; echo A` and `sleep 200; echo B`; agent started both with jobs 1 and 2 running now…",
		"": "",
	}
	for in, want := range cases {
		if got := CleanLine(in); got != want {
			t.Errorf("CleanLine(%q) = %q, want %q", in, got, want)
		}
	}
	if n := len(words(CleanLine(long))); n != maxWords {
		t.Errorf("capped line has %d words", n)
	}
}

func TestParse(t *testing.T) {
	title, summary := Parse("Title: \"Rework the serve control room.\"\nSummary: The user is reshaping the web UI.\nJobs and cache now show.")
	if title != "Rework the serve control room" || summary != "The user is reshaping the web UI. Jobs and cache now show." {
		t.Fatalf("Parse = %q, %q", title, summary)
	}
	if title, summary := Parse("Fix the flaky golden test"); title != "Fix the flaky golden test" || summary != "" {
		t.Fatalf("bare reply: %q, %q", title, summary)
	}
}

func e(kind string, data map[string]any) history.Entry { return history.Entry{Kind: kind, Data: data} }
func input(t string) history.Entry                     { return e("input", map[string]any{"text": t}) }
func done() history.Entry                              { return e("done", map[string]any{}) }

// Turns groups like the approved prototype, except only a done closes a
// turn (a cancelled or error before it names the end); a new input closes
// an open one.
func TestTurns(t *testing.T) {
	ts := Turns([]history.Entry{
		e("meta", map[string]any{"cwd": "/x"}),
		input("the gate is red"),
		e("code", map[string]any{"text": `await tools.bash("go test ./...")`}),
		e("result", map[string]any{"text": "FAIL", "exit": 1.0}),
		e("code", map[string]any{"text": "console.log(1)\nmore"}),
		e("result", map[string]any{"text": "1"}),
		e("assistant", map[string]any{"text": "the golden file is stale"}),
		e("assistant", map[string]any{"text": " "}),
		done(),
		input("stop that"), e("cancelled", nil), done(),
		input("half"), input("still going"),
		e("error", map[string]any{"text": "steer blocked"}), // mid-turn: does not close it
		e("assistant", map[string]any{"text": "carried on"}), done(),
		input("open"),
	})
	if len(ts) != 5 {
		t.Fatalf("%d turns: %+v", len(ts), ts)
	}
	if ts[0].End != "done" || ts[0].Reply != "the golden file is stale" ||
		strings.Join(ts[0].Calls, "|") != "bash: go test ./... → exit 1|console.log(1)" {
		t.Fatalf("turn 1 = %+v", ts[0])
	}
	if ts[1].End != "cancelled" || ts[2].End != "" || ts[2].Ask != "half" ||
		ts[3].End != "error" || ts[3].Reply != "carried on" || ts[4].End != "" {
		t.Fatalf("turns = %+v", ts)
	}
}

func TestTurnInput(t *testing.T) {
	tr := Turn{Ask: "go", End: "done", Reply: "ok"}
	for i := range 15 {
		tr.Calls = append(tr.Calls, fmt.Sprintf("c%d", i))
	}
	var log []string
	for i := range 14 {
		log = append(log, fmt.Sprintf("%d. You x", i+1))
	}
	in := TurnInput(log, tr)
	for _, want := range []string{"Log so far:\n3. You x", "- c3\n- … 7 more …\n- c11", "Agent's last reply: ok\nTurn ended: done"} {
		if !strings.Contains(in, want) {
			t.Errorf("input lacks %q:\n%s", want, in)
		}
	}
	if strings.Contains(in, "\n2. You x") || strings.Contains(in, "- c4\n") {
		t.Errorf("input kept too much:\n%s", in)
	}
	if in := TurnInput(nil, Turn{Ask: "hi"}); !strings.Contains(in, "(empty)") || !strings.HasSuffix(in, "Turn ended: still open") {
		t.Errorf("empty log input:\n%s", in)
	}
}

type stubLLM struct {
	mu            sync.Mutex
	turns, finals int
}

func (s *stubLLM) Complete(ctx context.Context, system string, msgs []llm.Message) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if system == FinalPrompt {
		s.finals++
		return fmt.Sprintf("Title: Fix gate %d\nSummary: You fix gate; agent on it.", s.finals), nil
	}
	s.turns++
	return fmt.Sprintf("You fixed thing %d; agent ran go test.", s.turns), nil
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
	en := history.Entry{Kind: kind, Data: data}
	m.entries = append(m.entries, en)
	return en
}
func (m *memHist) add(es ...history.Entry) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.entries = append(m.entries, es...)
}

func kinds(h *memHist, kind string) []history.Entry {
	var out []history.Entry
	for _, en := range h.Entries() {
		if en.Kind == kind {
			out = append(out, en)
		}
	}
	return out
}

// fakeTimer is the quiet timer under test control.
type fakeTimer struct {
	mu      sync.Mutex
	fire    func()
	d       time.Duration
	armed   int
	stopped int
}

func newTitler(l llm.LLM, h History) (*Titler, *fakeTimer, *[]string) {
	ft := &fakeTimer{}
	var emitted []string
	ctx, cancel := context.WithCancel(context.Background())
	tr := &Titler{llm: l, hist: h, ctx: ctx, cancel: cancel,
		emit: func(kind, text string) { emitted = append(emitted, kind+":"+text) },
		ttl:  func() time.Duration { return llm.CacheTTL("openai/gpt-6-astra") },
		after: func(d time.Duration, f func()) func() bool {
			ft.mu.Lock()
			defer ft.mu.Unlock()
			ft.fire, ft.d = f, d
			ft.armed++
			return func() bool { ft.mu.Lock(); ft.stopped++; ft.mu.Unlock(); return true }
		},
	}
	return tr, ft, &emitted
}

// Each finished turn gets one log line, never two; the first one also
// names the session.
func TestLogsEachTurnOnce(t *testing.T) {
	l := &stubLLM{}
	h := &memHist{entries: []history.Entry{input("the gate is red"), done()}}
	tr, ft, emitted := newTitler(l, h)
	tr.turnDone()
	tr.turnDone() // a doubled event
	if l.turns != 1 || len(kinds(h, "turn-summary")) != 1 {
		t.Fatalf("%d calls, %d lines", l.turns, len(kinds(h, "turn-summary")))
	}
	line := kinds(h, "turn-summary")[0].Data
	if line["text"] != "You fixed thing 1; agent ran go test." || line["turn"] != 1 {
		t.Fatalf("line = %v", line)
	}
	ts := kinds(h, "title")
	if len(ts) != 1 || ts[0].Data["text"] != "Fix gate 1" || ts[0].Data["final"] != true || l.finals != 1 {
		t.Fatalf("title = %v (%d finals)", ts, l.finals)
	}
	if len(*emitted) != 1 || (*emitted)[0] != "title:Fix gate 1" {
		t.Fatalf("emitted %v", *emitted)
	}
	if ft.armed != 2 || ft.stopped != 1 || ft.d != 30*time.Minute {
		t.Fatalf("timer armed %d stopped %d for %s", ft.armed, ft.stopped, ft.d)
	}

	h.add(input("now ship it"), e("cancelled", nil), done())
	tr.turnDone()
	if got := kinds(h, "turn-summary"); len(got) != 2 || got[1].Data["turn"] != 2 || len(kinds(h, "title")) != 1 {
		t.Fatalf("after turn 2: lines %v, titles %v", got, kinds(h, "title"))
	}
}

// A session that predates the log gets only its latest turn logged, and
// replaces a non-final (placeholder) name once.
func TestOldSessionLogsOnlyLatestTurn(t *testing.T) {
	l := &stubLLM{}
	h := &memHist{entries: []history.Entry{
		input("a"), done(), e("title", map[string]any{"text": "Old name"}),
		input("b"), done(), input("c"), done(), input("d"), done(),
	}}
	tr, _, _ := newTitler(l, h)
	tr.turnDone()
	got := kinds(h, "turn-summary")
	if l.turns != 1 || len(got) != 1 || got[0].Data["turn"] != 4 || len(kinds(h, "title")) != 2 || l.finals != 1 {
		t.Fatalf("%d calls, lines %v", l.turns, got)
	}
}

// A first naming that failed is retried when the session goes quiet,
// but not while a turn is open; once named, quiet does nothing more.
func TestQuietTimerNamesSession(t *testing.T) {
	l := &flakyLLM{stubLLM: &stubLLM{}, failFinals: 1}
	h := &memHist{entries: []history.Entry{input("the gate is red"), done()}}
	tr, ft, _ := newTitler(l, h)
	tr.turnDone()
	if len(kinds(h, "title")) != 0 {
		t.Fatal("named despite a failed call")
	}
	h.add(input("and another thing")) // the person came back
	ft.fire()
	if l.finals != 0 {
		t.Fatal("named while a turn was open")
	}
	h.add(done())
	ft.fire()
	ts := kinds(h, "title")
	if len(ts) != 1 || ts[0].Data["final"] != true || ts[0].Data["text"] != "Fix gate 1" ||
		ts[0].Data["summary"] != "You fix gate; agent on it." {
		t.Fatalf("titles = %v", ts)
	}
	ft.fire()
	tr.shutdown()
	if l.finals != 1 {
		t.Fatalf("renamed (%d finals)", l.finals)
	}
}

type flakyLLM struct {
	*stubLLM
	failFinals int
}

func (f *flakyLLM) Complete(ctx context.Context, system string, msgs []llm.Message) (string, error) {
	if system == FinalPrompt && f.failFinals > 0 {
		f.failFinals--
		return "", context.DeadlineExceeded
	}
	return f.stubLLM.Complete(ctx, system, msgs)
}

// Shutting down names the session when turns were logged since the last
// final naming, logging any turn still unlogged first.
func TestShutdownNamesSession(t *testing.T) {
	l := &stubLLM{}
	h := &memHist{entries: []history.Entry{input("a"), done(), input("b"), done()}}
	tr, ft, _ := newTitler(l, h)
	tr.turnDone()
	ft.stopped = 0
	tr.shutdown()
	if ft.stopped != 1 || l.turns != 2 || l.finals != 1 {
		t.Fatalf("stopped %d, %d turn calls, %d finals", ft.stopped, l.turns, l.finals)
	}
	ts := kinds(h, "title")
	if last := ts[len(ts)-1].Data; last["final"] != true || last["turn"] != 2 {
		t.Fatalf("last title %v", last)
	}
	// A session with no turns says nothing on the way out.
	empty := &stubLLM{}
	tr2, _, _ := newTitler(empty, &memHist{})
	tr2.shutdown()
	if empty.turns+empty.finals != 0 {
		t.Fatal("named an empty session")
	}
}

func writeSession(t *testing.T, dir, id string, es ...history.Entry) string {
	t.Helper()
	p := filepath.Join(dir, id+".jsonl")
	s, err := history.Open(p)
	if err != nil {
		t.Fatal(err)
	}
	for _, en := range es {
		s.Append(en.Kind, en.Data)
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	return p
}

func TestBackfill(t *testing.T) {
	dir := t.TempDir()
	fresh := writeSession(t, dir, "01fresh", e("meta", map[string]any{"cwd": "/x"}),
		input("a"), done(), input("b"), e("error", map[string]any{"text": "boom"}), done(), input("c"))
	done1 := writeSession(t, dir, "02done", input("a"), done(),
		e("turn-summary", map[string]any{"text": "You a; agent b.", "turn": 1}))
	writeSession(t, dir, "03empty", e("meta", nil))

	var stubs []*stubLLM
	var mu sync.Mutex
	newLLM := func() (llm.LLM, error) {
		mu.Lock()
		defer mu.Unlock()
		s := &stubLLM{}
		stubs = append(stubs, s)
		return s, nil
	}
	read := func(p string) []history.Entry {
		es, err := history.Read(p)
		if err != nil {
			t.Fatal(err)
		}
		return es
	}
	var out strings.Builder
	before := len(read(fresh))
	if err := Backfill(context.Background(), &out, dir, summarizeOpts{all: true, dry: true, maxTurns: 2}, newLLM); err != nil {
		t.Fatal(err)
	}
	if len(read(fresh)) != before || len(stubs) != 1 || stubs[0].turns != 2 ||
		!strings.Contains(out.String(), "  2. You fixed thing 2") || !strings.Contains(out.String(), "total: 3 calls") {
		t.Fatalf("dry run wrote or miscounted:\n%s", out.String())
	}

	out.Reset()
	if err := Backfill(context.Background(), &out, dir, summarizeOpts{all: true}, newLLM); err != nil {
		t.Fatal(err)
	}
	var lines, finals int
	for _, en := range read(fresh) {
		switch {
		case en.Kind == "turn-summary":
			lines++
		case en.Kind == "title" && en.Data["final"] == true && en.Data["turn"] == 2.0:
			finals++
		}
	}
	if lines != 2 || finals != 1 || !strings.Contains(out.String(), "total: 3 calls") {
		t.Fatalf("%d lines, %d finals:\n%s", lines, finals, out.String())
	}
	if len(read(done1)) != 3 {
		t.Fatal("touched an already summarized session")
	}

	// Named explicitly, a summarized session says why it was skipped.
	out.Reset()
	if err := Backfill(context.Background(), &out, dir, summarizeOpts{ids: []string{"01fresh", "02"}}, newLLM); err != nil {
		t.Fatal(err)
	}
	if strings.Count(out.String(), "already summarized") != 2 || !strings.Contains(out.String(), "total: 0 calls") {
		t.Fatalf("skip output:\n%s", out.String())
	}
}

func TestParseSummarize(t *testing.T) {
	o, err := parseSummarize([]string{"abc", "--dry-run", "--max-turns", "5"})
	if err != nil || !o.dry || o.maxTurns != 5 || o.ids[0] != "abc" || o.model != defaultModel {
		t.Fatalf("%+v %v", o, err)
	}
	for _, bad := range [][]string{nil, {"--max-turns", "0", "x"}, {"--bogus"}, {"--max-turns"}} {
		if _, err := parseSummarize(bad); err == nil {
			t.Errorf("parseSummarize(%q) accepted", bad)
		}
	}
}

// Backfill and the live plugin number turns the same way: after a
// backfill of a session with a cut-off turn and an open one, the open
// turn's done logs it under its own number, once.
func TestBackfillThenLive(t *testing.T) {
	dir := t.TempDir()
	p := writeSession(t, dir, "01mix", input("a"), done(), input("cut"), input("b"),
		e("cancelled", nil), done(), input("open"))
	newLLM := func() (llm.LLM, error) { return &stubLLM{}, nil }
	if err := Backfill(context.Background(), io.Discard, dir, summarizeOpts{all: true}, newLLM); err != nil {
		t.Fatal(err)
	}
	s, err := history.OpenExisting(p)
	if err != nil {
		t.Fatal(err)
	}
	s.Append("done", map[string]any{})
	l := &stubLLM{}
	tr, _, _ := newTitler(l, s)
	tr.turnDone()
	tr.turnDone()
	var got []int
	for _, en := range s.Entries() {
		if en.Kind == "turn-summary" {
			got = append(got, intOf(en.Data["turn"]))
		}
	}
	s.Close()
	if fmt.Sprint(got) != "[1 2 3 4]" || l.turns != 1 {
		t.Fatalf("turns logged %v, %d live calls", got, l.turns)
	}
}

// A turn that ends after shutdown arms no timer; shutdown is bounded
// even when the model hangs.
func TestShutdownBoundedAndFinal(t *testing.T) {
	h := &memHist{entries: []history.Entry{input("a"), done()}}
	tr, ft, _ := newTitler(hangLLM{}, h)
	tr.grace = 50 * time.Millisecond
	start := time.Now()
	tr.shutdown()
	if time.Since(start) > 2*time.Second {
		t.Fatal("shutdown waited on the model")
	}
	tr.turnDone()
	if ft.armed != 0 {
		t.Fatal("armed a timer after shutdown")
	}
}

type hangLLM struct{}

func (hangLLM) Complete(ctx context.Context, _ string, _ []llm.Message) (string, error) {
	<-ctx.Done()
	return "", ctx.Err()
}

// The session is named once, right after its first turn, from the model
// (no interim "Asked …" name); turns 2 and 3 leave it alone, and no
// stored title ends in an ellipsis.
func TestTitleSettlesOnce(t *testing.T) {
	l := &stubLLM{}
	h := &memHist{entries: []history.Entry{input("the gate is red"), done()}}
	tr, ft, emitted := newTitler(l, h)
	tr.turnDone()
	ts := kinds(h, "title")
	if len(ts) != 1 || ts[0].Data["text"] != "Fix gate 1" || len(*emitted) != 1 {
		t.Fatalf("after turn 1: titles %v, emitted %v", ts, *emitted)
	}
	for _, ask := range []string{"now ship it", "and tag it"} {
		h.add(input(ask), done())
		tr.turnDone()
	}
	ft.fire()
	tr.shutdown()
	if ts := kinds(h, "title"); len(ts) != 1 || l.finals != 1 || len(*emitted) != 1 {
		t.Fatalf("retitled: titles %v, %d finals, emitted %v", ts, l.finals, *emitted)
	}
	if got := Clean(strings.Repeat("very long title ", 10)); strings.HasSuffix(got, "…") || len([]rune(got)) > 60 {
		t.Fatalf("Clean stored %q", got)
	}
	title, _ := Parse("Title: " + strings.Repeat("word ", 20))
	if strings.HasSuffix(title, "…") {
		t.Fatalf("Parse title %q", title)
	}
}
