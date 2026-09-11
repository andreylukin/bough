package vtreal

// Background jobs, replayed on a real PTY: the model side comes from a
// tape, but codemode and tools-basic are REAL, so `tools.bash(cmd,
// limit)` starts an actual detached `sleep` in the run's temp $HOME.
// That is the only way the job seam is exercised end to end — the
// replay runtime never executes a block, so it can never start a job,
// and the job strip only ever lists live jobs.
//
// What is asserted: the strip renders under the composer while a job
// runs; a finished job's notice lands in the transcript as a collapsed
// "job" block that opens to the job line; and the wake turn the loop
// starts on its own never leaks its "[background job] …" preamble onto
// the screen — not into the transcript, not into the status bar.

import (
	"fmt"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// jobsConfig is replayConfig with a real codemode and tools-basic: the
// tape answers the model, the machine runs the blocks.
func jobsConfig(tape string) string {
	return fmt.Sprintf(`
- id: llm
  plugin: replay
  config: {file: %q}
- id: codemode
  plugin: codemode
- id: tools
  plugin: tools-basic
- id: commands
  plugin: commands
- id: history
  plugin: history
- id: loop
  plugin: loop
- id: ui
  plugin: ui
- id: session-title
  plugin: session-title
  disabled: true
- id: activity
  plugin: activity
  disabled: true
`, tape)
}

// jobsStart boots on a tape and sends its first recorded input.
func jobsStart(t *testing.T, name string, cols, rows int) *app {
	t.Helper()
	tape, err := filepath.Abs(filepath.Join("testdata", "replay", name))
	if err != nil {
		t.Fatal(err)
	}
	a := startCfg(t, cols, rows, jobsConfig(tape))
	a.check("boot")
	return a
}

// jobsSay types a line and sends it.
func jobsSay(a *app, text string) {
	a.typeText(text)
	a.key(uv.KeyEnter, 0)
}

// jobsHasRow reports whether some screen row is exactly text.
func jobsHasRow(lines []string, text string) bool {
	for _, l := range lines {
		if strings.TrimSpace(l) == text {
			return true
		}
	}
	return false
}

// jobsHistory is every entry bough wrote under this run's $HOME.
func jobsHistory(a *app) []history.Entry {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var out []history.Entry
	for _, p := range paths {
		entries, err := history.Read(p)
		if err != nil {
			continue
		}
		out = append(out, entries...)
	}
	return out
}

// The strip under the composer names a job that outlived its turn.
func TestJobsStripRow(t *testing.T) {
	t.Parallel()
	a := jobsStart(t, "jobs-running.jsonl", 100, 30)
	jobsSay(a, "start the long build")
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.waitUntil(func(s string) bool {
		return strings.Contains(s, "job 1") && strings.Contains(s, "sleep 45")
	}, "the job strip to name job 1")
	a.check("job running")

	ls := a.lines()
	row := -1
	for i, l := range ls {
		if strings.Contains(l, "job 1") && strings.Contains(l, "sleep 45") {
			row = i
		}
	}
	c := composerRow(ls)
	if row < 0 || c < 0 || row < c {
		t.Fatalf("want the job strip below the composer (strip row %d, composer %d):\n%s", row, c, a.text())
	}
	if !strings.Contains(ls[row], "·") {
		t.Fatalf("job row %d has no elapsed/label separator:\n%s", row, a.text())
	}
}

// A job that finishes while the agent is idle reports back as a
// collapsed "job" block that opens to the job's own line.
func TestJobsFinishedNoteCollapses(t *testing.T) {
	t.Parallel()
	a := jobsStart(t, "jobs-finish.jsonl", 100, 30)
	jobsSay(a, "start the short build")
	// Two turns: the one that started the job, and the wake turn the
	// finished job opens on its own.
	if !a.waitDone(2, 60*time.Second) {
		t.Fatalf("the finished job never woke a turn:\n%s", a.text())
	}
	a.check("job landed")

	s := a.settled()
	if !strings.Contains(s, "▸ job") {
		t.Fatalf("want a collapsed \"job\" header in the transcript:\n%s", s)
	}
	// The header counts the lines it is hiding; the body — the job's
	// captured output — is not on screen yet. ("BUILT-OK" also appears
	// inside the command itself, so the body is matched as its own row.)
	if !strings.Contains(s, "▸ job (2 lines)") {
		t.Fatalf("want the job header to count the lines it hides:\n%s", s)
	}
	if jobsHasRow(a.lines(), "BUILT-OK") {
		t.Fatalf("a collapsed job note must not show its body:\n%s", s)
	}

	// Open it: the header row is clickable, and the job's line is what
	// is inside.
	ls := a.lines()
	row := -1
	for i, l := range ls {
		if strings.Contains(l, "▸ job") {
			row = i
		}
	}
	a.click(2, row)
	a.waitUntil(func(s string) bool { return jobsHasRow(strings.Split(s, "\n"), "BUILT-OK") },
		"the opened job note to show the job's output")
	if s := a.settled(); !strings.Contains(s, "exited 0") {
		t.Fatalf("the opened job note does not carry the job's status line:\n%s", s)
	}
	a.check("job note open")
}

// The wake turn's injected input is machinery, not something anyone
// typed: it is recorded in history, drawn as the "job" block, and its
// bracketed preamble never reaches the screen or the status bar.
func TestJobsWakeInputDoesNotLeak(t *testing.T) {
	t.Parallel()
	a := jobsStart(t, "jobs-finish.jsonl", 100, 30)
	jobsSay(a, "start the short build")
	if !a.waitDone(2, 60*time.Second) {
		t.Fatalf("the finished job never woke a turn:\n%s", a.text())
	}
	a.waitFor("Job 1 finished cleanly") // the wake turn's reply landed
	a.check("after the wake turn")

	s := a.settled()
	if strings.Contains(s, "[background job]") {
		t.Fatalf("the injected wake preamble leaked onto the screen:\n%s", s)
	}
	// The status bar is the last row; nothing bracketed belongs there.
	ls := a.lines()
	bar := ls[len(ls)-1]
	if !strings.Contains(strings.Join(ls, "\n"), "? keys") {
		t.Fatalf("status bar missing:\n%s", s)
	}
	if strings.Contains(bar, "[") || strings.Contains(bar, "background job") {
		t.Fatalf("status bar carries job-wake text (%q):\n%s", bar, s)
	}
	// The user's own turn is still on screen above it all.
	if !strings.Contains(s, "start the short build") {
		t.Fatalf("the typed turn is gone from the transcript:\n%s", s)
	}

	// History recorded the wake as an input entry — that is where the
	// bracketed text lives, and it is the proof the turn was injected
	// rather than typed.
	injected := false
	for _, e := range jobsHistory(a) {
		if e.Kind != "input" {
			continue
		}
		if text, _ := e.Data["text"].(string); strings.HasPrefix(text, "[background job] ") {
			injected = true
		}
	}
	if !injected {
		t.Fatalf("no injected \"[background job]\" input in history:\n%s", s)
	}
}
