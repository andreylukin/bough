package vtreal

// Resuming a replayed session: the second bough boots with
// `history: {file: <the first run's jsonl>}` and must show the same
// transcript, the tally the file records, and a live turn that
// continues from a fresh tape.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
	"github.com/andreylukin/bough/plugins/loop"
)

// resumeConfig is replayConfig with the history row pointed at a
// specific file (what -r sets), so two runs share one session log.
func resumeConfig(tape, hist string) string {
	return fmt.Sprintf(`
- id: llm
  plugin: replay
  config: {file: %q}
- id: codemode
  plugin: replay
  config: {file: %q, provide: codemode}
- id: commands
  plugin: commands
- id: history
  plugin: history
  config: {file: %q}
- id: loop
  plugin: loop
- id: ui
  plugin: ui
# Rows that call the model on their own would eat tape replies.
- id: session-title
  plugin: session-title
  disabled: true
- id: auto-memory
  plugin: auto-memory
  disabled: true
- id: memory-tier
  plugin: memory-tier
  disabled: true
- id: activity
  plugin: activity
  disabled: true
- id: attention
  plugin: attention
  disabled: true
`, tape, tape, hist)
}

// resumeDones counts finished turns in one session file (the shared
// log lives outside either run's $HOME, so doneCount cannot see it).
func resumeDones(path string) int {
	entries, err := history.Read(path)
	if err != nil {
		return 0
	}
	n := 0
	for _, e := range entries {
		if e.Kind == "done" || e.Kind == "cancelled" {
			n++
		}
	}
	return n
}

func resumeWaitDones(t *testing.T, a *app, path string, n int) {
	t.Helper()
	deadline := time.Now().Add(60 * time.Second)
	for time.Now().Before(deadline) {
		if resumeDones(path) >= n {
			return
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatalf("only %d of %d turns finished in %s:\n%s", resumeDones(path), n, path, a.text())
}

// resumeTranscript is the rendered transcript: the rows above the
// composer, blank rows dropped, minus two rows that differ by design:
// the "resumed …" row a resumed session ends with, and the live
// "▸ context (N lines)" row, which the loop never writes to history
// (so a resume cannot draw it). Dropping the context row leaves two
// rules touching; adjacent duplicates collapse to one.
func resumeTranscript(a *app) []string {
	ls := a.lines()
	end := composerRow(ls)
	if end < 0 {
		end = len(ls)
	}
	var out []string
	for _, l := range ls[:end] {
		if l == "" || strings.Contains(l, "resumed ") || strings.HasPrefix(l, "▸ context (") {
			continue
		}
		if len(out) > 0 && out[len(out)-1] == l {
			continue
		}
		out = append(out, l)
	}
	return out
}

// resumeSend types one line and waits for the shared log to record the
// turn as finished.
func resumeSend(t *testing.T, a *app, path, text string, wantDones int) {
	t.Helper()
	a.typeText(text)
	a.key(uv.KeyEnter, 0)
	resumeWaitDones(t, a, path, wantDones)
}

// A session replayed to the end, then resumed from its own log, shows
// the same transcript.
func TestResumeRendersSameTranscript(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/resume.jsonl")
	next, _ := filepath.Abs("testdata/replay/resume-next.jsonl")
	log := filepath.Join(t.TempDir(), "session.jsonl")

	first := startCfg(t, 100, 40, resumeConfig(tape, log))
	resumeSend(t, first, log, "greet me", 1)
	resumeSend(t, first, log, "count the files", 2)
	first.check("first run")
	before := resumeTranscript(first)
	firstScreen := first.settled()
	if len(before) == 0 {
		t.Fatalf("first run rendered no transcript:\n%s", firstScreen)
	}
	for _, want := range []string{"greet me", "Hello from turn one.", "Two files here."} {
		if !strings.Contains(firstScreen, want) {
			t.Fatalf("first run is missing %q:\n%s", want, firstScreen)
		}
	}
	first.key('c', uv.ModCtrl)
	first.key('c', uv.ModCtrl)

	// The resumed run: same size, same log, a tape it does not need
	// until a new prompt arrives.
	second := startCfg(t, 100, 40, resumeConfig(next, log))
	second.check("resumed boot")
	second.waitFor("resumed ")
	after := resumeTranscript(second)
	if strings.Join(after, "\n") != strings.Join(before, "\n") {
		t.Fatalf("resumed transcript differs.\nfirst run:\n%s\n\nresumed:\n%s\n\nresumed screen:\n%s",
			strings.Join(before, "\n"), strings.Join(after, "\n"), second.text())
	}
	if !strings.Contains(second.settled(), "resumed ") {
		t.Fatalf("no resumed row naming the session:\n%s", second.text())
	}
}

// resumeStampUsage rewrites a session log with a usage tally on each
// "done" entry, in order, as a priced provider would have recorded.
func resumeStampUsage(t *testing.T, path string, usage []map[string]any) {
	t.Helper()
	entries, err := history.Read(path)
	if err != nil {
		t.Fatal(err)
	}
	var b strings.Builder
	i := 0
	for _, e := range entries {
		if e.Kind == "done" && i < len(usage) {
			if e.Data == nil {
				e.Data = map[string]any{}
			}
			e.Data["usage"] = usage[i]
			i++
		}
		line, err := json.Marshal(e)
		if err != nil {
			t.Fatal(err)
		}
		b.Write(append(line, '\n'))
	}
	if i != len(usage) {
		t.Fatalf("%s has %d done entries, want %d", path, i, len(usage))
	}
	if err := os.WriteFile(path, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
}

// The status bar of a resumed session reports the tally recorded in
// the file, not a fresh zero.
func TestResumeStatusBarShowsRecordedUsage(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/resume.jsonl")
	next, _ := filepath.Abs("testdata/replay/resume-next.jsonl")
	log := filepath.Join(t.TempDir(), "session.jsonl")

	first := startCfg(t, 100, 40, resumeConfig(tape, log))
	resumeSend(t, first, log, "greet me", 1)
	resumeSend(t, first, log, "count the files", 2)
	first.key('c', uv.ModCtrl)
	first.key('c', uv.ModCtrl)
	resumeStampUsage(t, log, []map[string]any{
		{"in": 10000, "out": 2000, "cost": 0.03, "last_in": 4000},
		{"in": 2345, "out": 1456, "cost": 0.022, "last_in": 4000},
	})

	// What the cost row sums at mount: the tally on file.
	entries, err := history.Read(log)
	if err != nil {
		t.Fatal(err)
	}
	u := loop.SumUsage(entries)
	if u.InputTokens != 12345 || u.OutputTokens != 3456 || !u.Priced || resumeCostText(u) != "$0.052" {
		t.Fatalf("tally on file = %+v, want 12345 in, 3456 out, $0.052", u)
	}

	a := startCfg(t, 100, 40, resumeConfig(next, log))
	a.waitFor("resumed ")
	a.check("resumed boot")
	s := a.settled()
	for _, want := range []string{"Hello from turn one.", "Two files here."} {
		if !strings.Contains(s, want) {
			t.Fatalf("resumed transcript missing %q:\n%s", want, s)
		}
	}
	if !strings.Contains(s, "↑12.3k ↓3.5k · $0.052") {
		t.Fatalf("resumed status bar does not show the tally on file (↑12.3k ↓3.5k · $0.052):\n%s", s)
	}
}

// resumeCostText is the status bar's dollar figure for u (plugins/ui costText).
func resumeCostText(u llm.Usage) string { return fmt.Sprintf("$%.3f", u.Cost) }

// A prompt typed into the resumed session runs off the new tape and
// lands under the restored transcript; the log keeps growing.
func TestResumeContinuesOnFreshTape(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/resume.jsonl")
	next, _ := filepath.Abs("testdata/replay/resume-next.jsonl")
	log := filepath.Join(t.TempDir(), "session.jsonl")

	first := startCfg(t, 100, 40, resumeConfig(tape, log))
	resumeSend(t, first, log, "greet me", 1)
	resumeSend(t, first, log, "count the files", 2)
	first.key('c', uv.ModCtrl)
	first.key('c', uv.ModCtrl)

	second := startCfg(t, 100, 40, resumeConfig(next, log))
	second.waitFor("resumed ")
	resumeSend(t, second, log, "and now", 3)
	second.waitFor("Continued on a fresh tape.")
	second.check("after the resumed turn")
	s := second.settled()
	for _, want := range []string{"Hello from turn one.", "Two files here.", "and now", "Continued on a fresh tape."} {
		if !strings.Contains(s, want) {
			t.Fatalf("resumed turn lost %q:\n%s", want, s)
		}
	}
	entries, err := history.Read(log)
	if err != nil {
		t.Fatalf("reading %s: %v\n%s", log, err, s)
	}
	inputs := 0
	for _, e := range entries {
		if e.Kind == "input" {
			inputs++
		}
	}
	if inputs != 3 {
		t.Fatalf("the resumed turn did not append to the same log (%d input entries, want 3):\n%s", inputs, s)
	}
}
