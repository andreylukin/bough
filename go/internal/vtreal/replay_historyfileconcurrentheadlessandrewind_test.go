package vtreal

// A TUI session rewinds turn by turn while `bough --headless` runs
// whole turns into the same $HOME: one history directory, one graph
// sqlite database. Five rewinds, each raced against one headless run
// of a fixed tape (the headless runs are sequential among themselves).
// Rewind must only move the TUI's own conversation: every headless
// session file stays byte-identical, the TUI's original file keeps
// all its turns, and each fork holds exactly the turns before the
// rewound one. Nobody may surface a locked/busy database, and after
// both processes are gone each session resumes to what it holds.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

const historyFileConcurrentHeadlessAndRewindTurns = 6

// historyFileConcurrentHeadlessAndRewindBusy is what a contended
// sqlite database says, in any of its spellings.
var historyFileConcurrentHeadlessAndRewindBusy = []string{"database is locked", "SQLITE_BUSY", "database is busy"}

func historyFileConcurrentHeadlessAndRewindNoBusy(t *testing.T, where, s string) {
	t.Helper()
	for _, b := range historyFileConcurrentHeadlessAndRewindBusy {
		if strings.Contains(s, b) {
			t.Errorf("%s surfaced %q:\n%s", where, b, s)
		}
	}
}

// historyFileConcurrentHeadlessAndRewindSeed writes a session of n
// turns ("prompt k" answered "ANSWER-k") and returns its path.
func historyFileConcurrentHeadlessAndRewindSeed(t *testing.T, home string, n int) string {
	t.Helper()
	dir := filepath.Join(home, ".bough", "history")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	at := time.Now().Add(-time.Hour).UTC()
	entries := []history.Entry{{Seq: 1, At: at, Kind: "meta", Data: map[string]any{"cwd": home}}}
	for k := 1; k <= n; k++ {
		seq := int64(3*k - 1)
		entries = append(entries,
			history.Entry{Seq: seq, At: at, Kind: "input", Data: map[string]any{"text": fmt.Sprintf("prompt %d", k)}},
			history.Entry{Seq: seq + 1, At: at, Kind: "assistant", Data: map[string]any{"text": fmt.Sprintf("```stop\nANSWER-%d\n```", k)}},
			history.Entry{Seq: seq + 2, At: at, Kind: "done"}, // no data: the canonical bytes history writes

		)
	}
	var b strings.Builder
	for _, e := range entries {
		line, err := json.Marshal(e)
		if err != nil {
			t.Fatal(err)
		}
		b.Write(line)
		b.WriteByte('\n')
	}
	p := filepath.Join(dir, "rewind-tui.jsonl")
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// historyFileConcurrentHeadlessAndRewindConfig is a replay config with
// the graph row and, when session is set, the history row pinned to it.
func historyFileConcurrentHeadlessAndRewindConfig(t *testing.T, tape, session string) string {
	t.Helper()
	yml := replayConfig(tape) + headlessConcurrentWithTUISameDBGraph
	if session == "" {
		return yml
	}
	const row = "- id: history\n  plugin: history\n"
	if !strings.Contains(yml, row) {
		t.Fatalf("replayConfig no longer has a plain history row:\n%s", yml)
	}
	return strings.Replace(yml, row, row+"  config: {file: \""+session+"\"}\n", 1)
}

// historyFileConcurrentHeadlessAndRewindInputs lists a file's prompts.
func historyFileConcurrentHeadlessAndRewindInputs(t *testing.T, path string) []string {
	t.Helper()
	entries, err := history.Read(path)
	if err != nil {
		t.Fatalf("read %s: %v", path, err)
	}
	var in []string
	for _, e := range entries {
		if e.Kind == "input" {
			in = append(in, history.Prompt(e))
		}
	}
	return in
}

func historyFileConcurrentHeadlessAndRewindPrompts(n int) []string {
	var out []string
	for k := 1; k <= n; k++ {
		out = append(out, fmt.Sprintf("prompt %d", k))
	}
	return out
}

func TestHistoryFileConcurrentHeadlessAndRewind(t *testing.T) {
	t.Parallel()
	const n = historyFileConcurrentHeadlessAndRewindTurns
	tuiTape, _ := filepath.Abs("testdata/replay/follow-up.jsonl")
	hlTape, _ := filepath.Abs("testdata/replay/headless_answer.jsonl")
	home, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	orig := historyFileConcurrentHeadlessAndRewindSeed(t, home, n)
	origBytes, _ := os.ReadFile(orig)

	a := undoStart(t, home, 100, 30, historyFileConcurrentHeadlessAndRewindConfig(t, tuiTape, orig))
	a.waitFor(fmt.Sprintf("ANSWER-%d", n))
	a.check("resumed")

	known := map[string]bool{orig: true}
	hlBytes := map[string][]byte{} // headless session -> content right after its run
	var forks []string             // TUI fork files, in rewind order

	for i := 1; i <= 5; i++ {
		keep := n - i // turns the fork must hold
		type hlOut struct {
			r   headlessResult
			err string
		}
		ch := make(chan hlOut, 1)
		go func() {
			defer func() {
				if p := recover(); p != nil {
					ch <- hlOut{err: fmt.Sprint(p)}
				}
			}()
			ch <- hlOut{r: headlessConcurrentWithTUISameDBRun(t, home, hlTape, "how many go files are here")}
		}()

		// Rewind one turn back while the headless run is in flight.
		a.key(uv.KeyEscape, 0)
		a.waitFor("press esc again to rewind")
		a.key(uv.KeyEscape, 0)
		a.waitFor("(current)")
		a.key(uv.KeyUp, 0)
		a.key(uv.KeyEnter, 0)
		want := fmt.Sprintf("> prompt %d", keep+1)
		a.waitUntil(func(string) bool { return strings.HasPrefix(followUpComposer(a), want) }, "rewound prompt in the composer")
		a.key(uv.KeyEscape, 0)
		a.waitFor("press esc again to clear the draft")
		a.key(uv.KeyEscape, 0)
		a.waitFor("say something")

		out := <-ch
		if out.err != "" {
			t.Fatalf("rewind %d: headless run: %s", i, out.err)
		}
		r := out.r
		if r.code != 0 || !strings.Contains(r.stdout, "Two Go files: a.go and b.go.") {
			t.Errorf("rewind %d: headless run failed:\n%s", i, r.screen())
		}
		historyFileConcurrentHeadlessAndRewindNoBusy(t, fmt.Sprintf("rewind %d: headless", i), r.stdout+r.stderr)

		// Sort the new files: exactly one headless session and one fork.
		var fresh []string
		for _, p := range multiSessions(home) {
			if !known[p] {
				fresh = append(fresh, p)
				known[p] = true
			}
		}
		var hl, fork []string
		for _, p := range fresh {
			if slices.Equal(historyFileConcurrentHeadlessAndRewindInputs(t, p), []string{"how many go files are here"}) {
				hl = append(hl, p)
			} else {
				fork = append(fork, p)
			}
		}
		if len(hl) != 1 || len(fork) != 1 {
			t.Fatalf("rewind %d: want one new headless session and one fork, got headless %v fork %v", i, hl, fork)
		}
		b, _ := os.ReadFile(hl[0])
		hlBytes[hl[0]] = b
		forks = append(forks, fork[0])

		s := a.settled()
		if strings.Contains(s, fmt.Sprintf("ANSWER-%d", keep+1)) || !strings.Contains(s, fmt.Sprintf("ANSWER-%d", keep)) {
			t.Errorf("rewind %d: transcript should end at ANSWER-%d:\n%s", i, keep, s)
		}
		historyFileConcurrentHeadlessAndRewindNoBusy(t, fmt.Sprintf("rewind %d: tui", i), s)
		a.check(fmt.Sprintf("rewind %d", i))
		if t.Failed() {
			return
		}
	}

	t.Run("RewindOnlyTouchesTUISession", func(t *testing.T) {
		// The TUI's own file may log the rewind's /tree command; its
		// turns must stay intact.
		got, _ := os.ReadFile(orig)
		if !strings.HasPrefix(string(got), string(origBytes)) {
			t.Errorf("original TUI session turns changed by rewinds:\nwant prefix %s\ngot  %s", origBytes, got)
		} else if extra, err := history.Read(orig); err == nil {
			for _, e := range extra[2+3*n-1:] {
				if e.Kind != "command" {
					t.Errorf("original TUI session gained a %q entry after rewinds: %+v", e.Kind, e)
				}
			}
		}
		for p, want := range hlBytes {
			if got, _ := os.ReadFile(p); string(got) != string(want) {
				t.Errorf("headless session %s changed after its run:\nwant %s\ngot  %s", p, want, got)
			}
			if d := multiKinds(p, "done"); d != 1 {
				t.Errorf("headless session %s has %d done entries, want 1", p, d)
			}
		}
		for i, f := range forks {
			want := historyFileConcurrentHeadlessAndRewindPrompts(n - 1 - i)
			if got := historyFileConcurrentHeadlessAndRewindInputs(t, f); !slices.Equal(got, want) {
				t.Errorf("fork %d (%s) inputs = %q, want %q", i+1, f, got, want)
			}
		}
	})

	if t.Failed() {
		return
	}
	rewindAfterResumeAndForkQuit(t, a)

	// Resume each kind of session in a fresh TUI over the same $HOME.
	resume := func(t *testing.T, session string, want, absent []string) {
		t.Helper()
		b := undoStart(t, home, 100, 30, historyFileConcurrentHeadlessAndRewindConfig(t, tuiTape, session))
		b.waitFor(want[len(want)-1])
		s := b.settled()
		for _, w := range want {
			if !strings.Contains(s, w) {
				t.Errorf("resumed %s lacks %q:\n%s", session, w, s)
			}
		}
		for _, w := range absent {
			if strings.Contains(s, w) {
				t.Errorf("resumed %s shows %q:\n%s", session, w, s)
			}
		}
		historyFileConcurrentHeadlessAndRewindNoBusy(t, "resumed "+session, s)
		b.check("resumed " + session)
		rewindAfterResumeAndForkQuit(t, b)
	}

	t.Run("ResumeLastForkIdentical", func(t *testing.T) {
		resume(t, forks[len(forks)-1], []string{"prompt 1", "ANSWER-1"}, []string{"ANSWER-2", "Two Go files"})
	})
	t.Run("ResumeHeadlessIdentical", func(t *testing.T) {
		for p := range hlBytes {
			resume(t, p, []string{"how many go files are here", "Two Go files: a.go and b.go."}, []string{"ANSWER-1"})
			if got, _ := os.ReadFile(p); string(got) != string(hlBytes[p]) {
				t.Errorf("resuming headless session %s rewrote it", p)
			}
			break // one is enough: they are byte-identical runs of one tape
		}
	})
}
