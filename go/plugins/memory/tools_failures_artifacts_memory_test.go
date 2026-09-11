package memory

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/andreylukin/bough/plugins/graph"
	"github.com/andreylukin/bough/plugins/history"
)

func memoryArtifactsKnown(t *testing.T, bug string) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_TOOLS_ARTIFACTS_MEMORY") != "1" {
		t.Skip("known bug: " + bug)
	}
}

// memoryArtifactsGraph fails its first n asserts, then records.
type memoryArtifactsGraph struct {
	mu    sync.Mutex
	fail  int
	saved []string
}

func (g *memoryArtifactsGraph) AssertAs(author, src, rel, dst, evidence string) (graph.Edge, error) {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.fail > 0 {
		g.fail--
		return graph.Edge{}, errors.New("database is locked (5) (SQLITE_BUSY)")
	}
	g.saved = append(g.saved, evidence)
	return graph.Edge{}, nil
}

type memoryArtifactsHist struct {
	entries []history.Entry
	path    string
}

func (h *memoryArtifactsHist) Entries() []history.Entry { return h.entries }
func (h *memoryArtifactsHist) Path() string             { return h.path }

// tools.evidence with empty, malformed and hostile refs errors cleanly.
func TestMemoryArtifactsEvidenceBadArgs(t *testing.T) {
	dir := t.TempDir()
	m, _ := newMem(t, &stubLLM{})
	m.hist = &memoryArtifactsHist{entries: []history.Entry{{Seq: 1, Kind: "result", Data: map[string]any{"text": "x"}}}, path: filepath.Join(dir, "sess-a.jsonl")}
	m.session = "sess-a"
	if err := os.WriteFile(filepath.Join(dir, "corrupt.jsonl"), []byte("{{{\nnot json\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	for ref, want := range map[string]string{
		"":          "want session#seq",
		"   ":       "want session#seq",
		"#":         "bad seq",
		"#abc":      "bad seq",
		"sess-a#":   "bad seq",
		"#99":       "no entry #99",
		"../x#1":    "bad session",
		"a\\b#1":    "bad session",
		"missing#1": "session missing",
		"corrupt#1": "no entry #1",
		"#-1":       "no entry #-1",
		"#1e9999":   "bad seq",
	} {
		got, err := m.evidence(ref)
		if err == nil || !strings.Contains(err.Error(), want) {
			t.Errorf("evidence(%q) = %q, %v; want error %q", ref, got, err, want)
		}
	}
	m.hist = &memoryArtifactsHist{path: filepath.Join(dir, "gone", "sess-a.jsonl")}
	if _, err := m.evidence("other#1"); err == nil {
		t.Fatal("evidence from a missing history dir")
	}
}

// A graph that refuses a write (locked, closed) is reported, and the
// fact is not lost: the next harvest of the same fact writes it.
func TestMemoryArtifactsGraphLockedFactRetried(t *testing.T) {
	g := &memoryArtifactsGraph{fail: 1}
	m, _ := newMem(t, &stubLLM{reply: "decision | the gate greps FAIL rather than tailing output | the gate"})
	m.graph = g
	var mu sync.Mutex
	var emitted []string
	m.emit = func(kind, text string) { mu.Lock(); emitted = append(emitted, text); mu.Unlock() }
	m.harvest()
	if len(emitted) != 1 || !strings.Contains(emitted[0], "memory: graph: database is locked") {
		t.Fatalf("locked graph not reported: %q", emitted)
	}
	m.harvest()
	if len(g.saved) != 1 {
		t.Fatalf("fact lost after a failed write: saved=%q emitted=%q", g.saved, emitted)
	}
}

// Without a graph, an unwritable memory file is reported, not swallowed.
func TestMemoryArtifactsFileUnwritable(t *testing.T) {
	m, _ := newMem(t, &stubLLM{reply: "location | it lives in go/x | the gate"})
	flat := filepath.Join(t.TempDir(), "flat")
	_ = os.WriteFile(flat, nil, 0o644)
	m.file = filepath.Join(flat, "memory.md")
	var emitted []string
	m.emit = func(kind, text string) { emitted = append(emitted, text) }
	m.harvest()
	if len(emitted) != 1 || !strings.HasPrefix(emitted[0], "memory: ") {
		t.Fatalf("unwritable file: %q", emitted)
	}
}

// A graph db that is corrupt or cannot be created fails at open with
// an error, never a panic.
func TestMemoryArtifactsGraphDBCorruptOrMissing(t *testing.T) {
	dir := t.TempDir()
	bad := filepath.Join(dir, "graph.db")
	if err := os.WriteFile(bad, []byte(strings.Repeat("this is not sqlite ", 400)), 0o644); err != nil {
		t.Fatal(err)
	}
	if st, err := graph.Open(bad); err == nil {
		st.Close()
		t.Fatal("corrupt db opened")
	}
	flat := filepath.Join(dir, "flat")
	_ = os.WriteFile(flat, nil, 0o644)
	if st, err := graph.Open(filepath.Join(flat, "graph.db")); err == nil {
		st.Close()
		t.Fatal("db under a file opened")
	}
}
