package vtreal

// The memory stack offline: graph + auto-memory rows are on, the
// llm-small tape answers the harvest with malformed JSON (and then runs
// off its end), and the graph database is read-only. The main turn
// must not notice: every one of 50 turns still gets its own reply off
// the main tape, and the memory side says what is wrong at most once
// instead of once per turn.

import (
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/graph"
)

const memoryGraphOfflineFailureTurns = 50

// memoryGraphOfflineFailureMain writes a main tape of n one-reply turns;
// turn i answers "MGOF-REPLY-i".
func memoryGraphOfflineFailureMain(t *testing.T, n int) string {
	t.Helper()
	var b strings.Builder
	seq := 1
	line := func(kind, text string) {
		fmt.Fprintf(&b, `{"seq":%d,"at":"2026-09-11T10:00:00Z","kind":%q,"data":{"text":%q}}`+"\n", seq, kind, text)
		seq++
	}
	fmt.Fprintf(&b, `{"seq":%d,"at":"2026-09-11T10:00:00Z","kind":"meta","data":{"cwd":"/tmp/demo"}}`+"\n", seq)
	seq++
	for i := 1; i <= n; i++ {
		line("input", fmt.Sprintf("turn %d", i))
		line("assistant", fmt.Sprintf("```stop\nMGOF-REPLY-%d\n```", i))
		line("done", "")
	}
	p := filepath.Join(t.TempDir(), "main.jsonl")
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// memoryGraphOfflineFailureSmall writes the llm-small tape: one reply
// per given text (the tape then answers "end of tape" forever).
func memoryGraphOfflineFailureSmall(t *testing.T, replies []string) string {
	t.Helper()
	var b strings.Builder
	b.WriteString(`{"seq":1,"at":"2026-09-11T10:00:00Z","kind":"meta","data":{"cwd":"/tmp/demo"}}` + "\n")
	for i, r := range replies {
		fmt.Fprintf(&b, `{"seq":%d,"at":"2026-09-11T10:00:01Z","kind":"assistant","data":{"text":%q}}`+"\n", i+2, r)
	}
	p := filepath.Join(t.TempDir(), "small.jsonl")
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// memoryGraphOfflineFailureReadonlyDB creates a real graph database and
// makes it (and its directory, so no -wal/-shm can be created) read-only.
func memoryGraphOfflineFailureReadonlyDB(t *testing.T) string {
	t.Helper()
	dir := filepath.Join(t.TempDir(), "graph")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	p := filepath.Join(dir, "graph.db")
	st, err := graph.Open(p)
	if err != nil {
		t.Fatal(err)
	}
	if err := st.Close(); err != nil {
		t.Fatal(err)
	}
	for _, f := range []string{p + "-wal", p + "-shm"} {
		_ = os.Remove(f)
	}
	if err := os.Chmod(p, 0o444); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(dir, 0o555); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.Chmod(dir, 0o755); _ = os.Chmod(p, 0o644) })
	return p
}

func memoryGraphOfflineFailureConfig(main, small, db string) string {
	return llmSmallConfig(main, small, fmt.Sprintf(`
- id: graph
  plugin: graph
  config: {path: %q, embed: false}
- id: auto-memory
  plugin: auto-memory
`, db))
}

// memoryGraphOfflineFailureNotices counts the memory receipt rows on
// the settled screen (history does not keep them; a per-turn notice
// would stack several into a 30-row screen).
func memoryGraphOfflineFailureNotices(screen string) int {
	n := 0
	for _, l := range strings.Split(screen, "\n") {
		if strings.Contains(l, "memory:") {
			n++
		}
	}
	return n
}

// memoryGraphOfflineFailureRun drives n turns, checks each one got its
// own main-tape reply, and returns the most memory notices seen at once.
func memoryGraphOfflineFailureRun(a *app, n int) int {
	a.t.Helper()
	most := 0
	for i := 1; i <= n; i++ {
		llmSmallTurn(a, fmt.Sprintf("turn %d", i), i)
		want := fmt.Sprintf("MGOF-REPLY-%d", i)
		a.waitUntil(func(s string) bool { return strings.Contains(s, want) }, want)
		s := a.settled()
		if strings.Contains(s, "end of tape") {
			a.t.Fatalf("turn %d: the main tape ran out (the memory side ate it):\n%s", i, s)
		}
		most = max(most, memoryGraphOfflineFailureNotices(s))
	}
	time.Sleep(time.Second) // let the last harvest land
	return max(most, memoryGraphOfflineFailureNotices(a.settled()))
}

// The small tape: two malformed-JSON replies, then "end of tape" (a
// stop block, just as unparseable) for every later harvest.
var memoryGraphOfflineFailureMalformed = []string{
	`{"facts": [{"kind": "location", "text": "unterminated`,
	`[{,,}`,
}

func memoryGraphOfflineFailureSkipPOSIX(t *testing.T) {
	t.Helper()
	if runtime.GOOS == "windows" {
		t.Skip("read-only directories via chmod are POSIX")
	}
	if os.Geteuid() == 0 {
		t.Skip("root ignores the read-only bits")
	}
}

// Malformed JSON from the small model over 50 turns: nothing parses,
// nothing is saved, the main tape answers every turn, and the memory
// side never stacks up notices.
func TestMemoryGraphOfflineFailureMalformed(t *testing.T) {
	t.Parallel()
	main := memoryGraphOfflineFailureMain(t, memoryGraphOfflineFailureTurns)
	small := memoryGraphOfflineFailureSmall(t, memoryGraphOfflineFailureMalformed)
	db := filepath.Join(t.TempDir(), "graph.db")
	a := startCfg(t, 100, 30, memoryGraphOfflineFailureConfig(main, small, db))
	a.check("boot")
	most := memoryGraphOfflineFailureRun(a, memoryGraphOfflineFailureTurns)
	a.check("after 50 turns")
	if most > 1 {
		t.Fatalf("%d memory notices on screen at once over %d turns, want at most one:\n%s",
			most, memoryGraphOfflineFailureTurns, a.text())
	}
}

// A read-only graph database: the graph row cannot come up, and that
// must cost the memory feature only — bough still boots and all 50
// turns answer, with the failure said at most once.
func TestMemoryGraphOfflineFailureReadonly(t *testing.T) {
	memoryGraphOfflineFailureSkipPOSIX(t)
	t.Parallel()
	main := memoryGraphOfflineFailureMain(t, memoryGraphOfflineFailureTurns)
	small := memoryGraphOfflineFailureSmall(t, memoryGraphOfflineFailureMalformed)
	a := startCfg(t, 100, 30, memoryGraphOfflineFailureConfig(main, small, memoryGraphOfflineFailureReadonlyDB(t)))
	a.check("boot")
	most := memoryGraphOfflineFailureRun(a, memoryGraphOfflineFailureTurns)
	a.check("after 50 turns")
	if most > 1 {
		t.Fatalf("%d memory notices on screen at once over %d turns, want at most one:\n%s",
			most, memoryGraphOfflineFailureTurns, a.text())
	}
}
