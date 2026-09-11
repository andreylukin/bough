package vtreal

// Recorded sessions replayed through the real binary on a real PTY.
// The replay plugin answers the loop with what the model and the
// runtime said at the time, so a whole session costs nothing and is
// deterministic. TestReplayFixture always runs; TestReplayHistory
// sweeps a directory of real recordings when asked:
//
//	BOUGH_REPLAY_DIR=~/.bough/history BOUGH_REPLAY_SIZES=100x30,80x24,200x50 \
//	  go test ./internal/vtreal -run TestReplayHistory -parallel 32 -timeout 30m
//
// Every failure prints the session, turn, size and screen.

import (
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/replay"
)

func replayConfig(tape string) string {
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
- id: loop
  plugin: loop
- id: ui
  plugin: ui
# Rows that call the model on their own would eat tape replies and
# shift every later turn (session-title did, and the tape's first
# code fence became the title).
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
`, tape, tape)
}

// panicky is what a Go crash or a lipgloss overflow leaves on screen.
var panicky = regexp.MustCompile(`panic:|goroutine \d+ \[|runtime error:`)

// check is the set of invariants every settled screen must hold.
func (a *app) check(where string) {
	a.t.Helper()
	s := a.settled()
	ls := strings.Split(s, "\n")
	if panicky.MatchString(s) {
		a.t.Errorf("%s: crash text on screen:\n%s", where, s)
	}
	if r := composerRow(ls); r < 0 || r < len(ls)-3 {
		a.t.Errorf("%s: composer not on the last rows (row %d of %d):\n%s", where, r, len(ls), s)
	}
	if !strings.Contains(s, "? keys") {
		a.t.Errorf("%s: status bar missing:\n%s", where, s)
	}
	for i, l := range ls {
		if w := len([]rune(l)); w > a.cols {
			a.t.Errorf("%s: row %d is %d cells wide in a %d-column pane:\n%s", where, i, w, a.cols, s)
		}
	}
}

// doneCount reads the session bough wrote under this run's $HOME.
func (a *app) doneCount() int {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	n := 0
	for _, p := range paths {
		entries, err := history.Read(p)
		if err != nil {
			continue
		}
		for _, e := range entries {
			if e.Kind == "done" || e.Kind == "cancelled" {
				n++
			}
		}
	}
	return n
}

func (a *app) waitDone(n int, timeout time.Duration) bool {
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if a.doneCount() >= n {
			return true
		}
		time.Sleep(50 * time.Millisecond)
	}
	return false
}

// drive types every recorded input in turn, waits for the turn to
// finish and checks the screen after each one.
func drive(t *testing.T, tape string, cols, rows int) {
	t.Helper()
	tp, err := replay.Load(tape)
	if err != nil {
		t.Fatal(err)
	}
	a := startCfg(t, cols, rows, replayConfig(tape))
	a.check("boot")
	turns := 0
	for i, in := range tp.Inputs {
		if strings.HasPrefix(in, "/") {
			continue // a command: never went to the model
		}
		where := fmt.Sprintf("turn %d/%d", i+1, len(tp.Inputs))
		if strings.Contains(in, "\n") || len(in) > 200 {
			a.term.Paste(in)
		} else {
			a.typeText(in)
		}
		a.key(uv.KeyEnter, 0)
		turns++
		if !a.waitDone(turns, 60*time.Second) {
			t.Fatalf("%s: turn never finished:\n%s", where, a.text())
		}
		a.check(where)
		if t.Failed() {
			return
		}
	}
	// Scroll to the top and back: the transcript must survive a wheel
	// trip after everything landed.
	for range 20 {
		a.term.SendMouse(uv.MouseWheelEvent{X: 5, Y: 3, Button: uv.MouseWheelUp})
	}
	a.check("scrolled up")
	for range 40 {
		a.term.SendMouse(uv.MouseWheelEvent{X: 5, Y: 3, Button: uv.MouseWheelDown})
	}
	a.check("scrolled back")
}

func sizes(t *testing.T) [][2]int {
	spec := os.Getenv("BOUGH_REPLAY_SIZES")
	if spec == "" {
		spec = "100x30"
	}
	var out [][2]int
	for _, s := range strings.Split(spec, ",") {
		c, r, ok := strings.Cut(strings.TrimSpace(s), "x")
		cols, err1 := strconv.Atoi(c)
		rows, err2 := strconv.Atoi(r)
		if !ok || err1 != nil || err2 != nil {
			t.Fatalf("BOUGH_REPLAY_SIZES: bad size %q", s)
		}
		out = append(out, [2]int{cols, rows})
	}
	return out
}

func TestReplayFixture(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/basic.jsonl")
	for _, sz := range sizes(t) {
		t.Run(fmt.Sprintf("%dx%d", sz[0], sz[1]), func(t *testing.T) {
			t.Parallel()
			drive(t, tape, sz[0], sz[1])
		})
	}
}

func TestReplayHistory(t *testing.T) {
	dir := os.Getenv("BOUGH_REPLAY_DIR")
	if dir == "" {
		t.Skip("set BOUGH_REPLAY_DIR to a directory of history .jsonl files")
	}
	if strings.HasPrefix(dir, "~/") {
		home, _ := os.UserHomeDir()
		dir = filepath.Join(home, dir[2:])
	}
	paths, err := filepath.Glob(filepath.Join(dir, "*.jsonl"))
	if err != nil || len(paths) == 0 {
		t.Fatalf("no .jsonl files in %s", dir)
	}
	sort.Strings(paths)
	for _, p := range paths {
		tp, err := replay.Load(p)
		if err != nil || tp.Turns() == 0 {
			continue // no model turns: nothing to replay
		}
		id := strings.TrimSuffix(filepath.Base(p), ".jsonl")
		for _, sz := range sizes(t) {
			t.Run(fmt.Sprintf("%s/%dx%d", id, sz[0], sz[1]), func(t *testing.T) {
				t.Parallel()
				drive(t, p, sz[0], sz[1])
			})
		}
	}
}
