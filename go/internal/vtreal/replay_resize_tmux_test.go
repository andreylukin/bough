package vtreal

// Resize under tmux. x/vt drops the bottom rows on resize, so the
// resize checks live here, on a real tmux server, replaying the basic
// fixture: between turns and mid-stream (delay_ms), through 40x12,
// 200x50, 60x8 and 100x30.
//
// What every size must still hold: the status bar and the composer on
// the last rows, the transcript text preserved, the open boxes
// re-wrapped to the new width, and no row wider than the pane — a
// wider row is what hands the whole transcript a sideways scroll.

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

var resizeTmuxSizes = [][2]int{{40, 12}, {200, 50}, {60, 8}, {100, 30}}

// resizeTmuxStart is startTmux with a config of our own (the replay
// rows) and the run's $HOME handed back, so a turn can be waited on.
func resizeTmuxStart(t *testing.T, cols, rows int, yml string) (*tmuxApp, string) {
	t.Helper()
	if _, err := exec.LookPath("tmux"); err != nil {
		t.Skip("tmux not installed")
	}
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	tm := &tmuxApp{t: t, sock: fmt.Sprintf("vtresize-%d-%d", os.Getpid(), time.Now().UnixNano())}
	shell := fmt.Sprintf("cd %s && HOME=%s TERM=xterm-256color %s -config %s", home, home, bin, cfg)
	tm.run("new-session", "-d", "-x", fmt.Sprint(cols), "-y", fmt.Sprint(rows), shell)
	t.Cleanup(func() {
		_ = exec.Command("tmux", "-L", tm.sock, "kill-server").Run()
		dir := os.Getenv("TMUX_TMPDIR")
		if dir == "" {
			dir = "/tmp"
		}
		_ = os.Remove(filepath.Join(dir, fmt.Sprintf("tmux-%d", os.Getuid()), tm.sock))
	})
	tm.waitFor("say something")
	return tm, home
}

func resizeTmuxTape(t *testing.T) string {
	t.Helper()
	p, err := filepath.Abs("testdata/replay/basic.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	return p
}

// resizeTmuxDone counts finished turns in the run's own history.
func resizeTmuxDone(home string) int {
	paths, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
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

func resizeTmuxWaitDone(t *testing.T, tm *tmuxApp, home string, n int) {
	t.Helper()
	deadline := time.Now().Add(60 * time.Second)
	for time.Now().Before(deadline) {
		if resizeTmuxDone(home) >= n {
			return
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatalf("turn %d never finished\nscreen:\n%s", n, resizeTmuxScreen(tm))
}

// resizeTmuxBorder matches a full box border row: ╭───╮ or ╰───╯.
var resizeTmuxBorder = regexp.MustCompile(`^\s*[╭╰]─+[╮╯]\s*$`)

// resizeTmuxScreen is the pane as cells: capture-pane writes a run of
// spaces as a tab, which would make every width measurement here lie.
func resizeTmuxScreen(tm *tmuxApp) string {
	ls := strings.Split(tm.screen(), "\n")
	for i, l := range ls {
		var sb strings.Builder
		col := 0
		for _, r := range l {
			if r == '\t' {
				n := 8 - col%8
				sb.WriteString(strings.Repeat(" ", n))
				col += n
				continue
			}
			sb.WriteRune(r)
			col++
		}
		ls[i] = sb.String()
	}
	return strings.Join(ls, "\n")
}

// resizeTmuxBarWidth is the width of the status bar row, -1 when it is
// not on screen. The bar is rendered at exactly the pane width (its
// trailing blank trimmed off by the capture), which makes it the tell
// for "this frame was redrawn at the new size": a bare "two captures
// match" settles happily on the frame from before the resize.
func resizeTmuxBarWidth(lines []string) int {
	for _, l := range lines {
		if strings.Contains(l, "bough ·") {
			return len([]rune(l))
		}
	}
	return -1
}

// resizeTmuxSettled waits for a redrawn frame that has stopped moving.
func resizeTmuxSettled(tm *tmuxApp, cols int) string {
	tm.waitUntil(func(string) bool {
		ls := strings.Split(resizeTmuxScreen(tm), "\n")
		return composerRow(ls) >= 0 && resizeTmuxBarWidth(ls) >= cols-2
	}, fmt.Sprintf("a frame redrawn at %d columns", cols))
	prev := resizeTmuxScreen(tm)
	for range 40 {
		time.Sleep(80 * time.Millisecond)
		cur := resizeTmuxScreen(tm)
		if cur == prev {
			return cur
		}
		prev = cur
	}
	return prev
}

// resizeTmuxCheck is every invariant a settled screen must hold at
// this size, whatever it was resized from.
func resizeTmuxCheck(t *testing.T, tm *tmuxApp, where string, cols int) {
	t.Helper()
	s := resizeTmuxSettled(tm, cols)
	ls := strings.Split(s, "\n")
	if panicky.MatchString(s) {
		t.Errorf("%s: crash text on screen:\n%s", where, s)
	}
	if r := composerRow(ls); r < 0 || r < len(ls)-3 {
		t.Errorf("%s: composer not on the last rows (row %d of %d):\n%s", where, r, len(ls), s)
	}
	if !strings.Contains(s, "? keys") {
		t.Errorf("%s: status bar missing:\n%s", where, s)
	}
	for i, l := range ls {
		if w := len([]rune(l)); w > cols {
			t.Errorf("%s: row %d is %d cells wide in a %d-column pane (sideways scroll):\n%s", where, i, w, cols, s)
		}
	}
	// An open box is drawn at pane width - 4; a box still at the old
	// width is the re-wrap that did not happen. None on screen (small
	// pane, everything collapsed) is fine: the width rule above still
	// covers the rest.
	for i, l := range ls {
		if !resizeTmuxBorder.MatchString(l) {
			continue
		}
		if w := len([]rune(strings.TrimSpace(l))); w != cols-4 {
			t.Errorf("%s: box border on row %d is %d cells wide, want %d (pane %d):\n%s", where, i, w, cols-4, cols, s)
		}
	}
}

// resizeTmuxSweep resizes through every size, checking after each.
func resizeTmuxSweep(t *testing.T, tm *tmuxApp, where string) {
	t.Helper()
	for _, sz := range resizeTmuxSizes {
		tm.resize(sz[0], sz[1])
		resizeTmuxCheck(t, tm, fmt.Sprintf("%s @ %dx%d", where, sz[0], sz[1]), sz[0])
		if t.Failed() {
			return
		}
	}
}

func resizeTmuxSend(tm *tmuxApp, text string) {
	tm.keys(text)
	tm.keys("Enter")
}

// resizeTmuxExpandAll opens every block (the boxes the re-wrap checks
// need), then clears the action's flash note — it would crowd "? keys"
// out of a narrow bar — with a keystroke pair that leaves the composer
// as it was.
func resizeTmuxExpandAll(tm *tmuxApp) {
	resizeTmuxSend(tm, "/expand_all")
	tm.waitFor("expanded")
	tm.keys("x")
	tm.keys("BSpace")
}

// TestResizeTmuxBetweenTurns replays the fixture turn by turn and
// sweeps every size between turns.
func TestResizeTmuxBetweenTurns(t *testing.T) {
	t.Parallel()
	tape := resizeTmuxTape(t)
	tm, home := resizeTmuxStart(t, 100, 30, replayConfig(tape))
	inputs := []string{"list the files here", "run the tests and show me a very long separator line"}
	for i, in := range inputs {
		resizeTmuxSend(tm, in)
		resizeTmuxWaitDone(t, tm, home, i+1)
		resizeTmuxSweep(t, tm, fmt.Sprintf("after turn %d", i+1))
		if t.Failed() {
			return
		}
		tm.resize(100, 30)
	}
}

// TestResizeTmuxMidStream resizes while the reply is still streaming
// (delay_ms slows the tape down to a live-looking stream).
func TestResizeTmuxMidStream(t *testing.T) {
	t.Parallel()
	tape := resizeTmuxTape(t)
	yml := strings.Replace(replayConfig(tape),
		fmt.Sprintf("config: {file: %q}", tape),
		fmt.Sprintf("config: {file: %q, delay_ms: 800}", tape), 1)
	tm, home := resizeTmuxStart(t, 100, 30, yml)

	resizeTmuxSend(tm, "run the tests and show me a very long separator line")
	// Do not wait for the turn: sweep while the words are arriving.
	resizeTmuxSweep(t, tm, "mid-stream")
	if t.Failed() {
		return
	}
	if resizeTmuxDone(home) > 0 {
		t.Fatalf("the turn finished before the sweep did, so it was not mid-stream; raise delay_ms:\n%s", resizeTmuxScreen(tm))
	}
	resizeTmuxWaitDone(t, tm, home, 1)
	resizeTmuxCheck(t, tm, "after the streamed turn", 100)
}

// TestResizeTmuxOpenBoxesRewrap expands every block — the fixture's
// 180-dash rule and unbreakable URL among them — and resizes through
// every size: each open box must be redrawn at the new pane width.
func TestResizeTmuxOpenBoxesRewrap(t *testing.T) {
	t.Parallel()
	tape := resizeTmuxTape(t)
	tm, home := resizeTmuxStart(t, 100, 30, replayConfig(tape))
	resizeTmuxSend(tm, "list the files here")
	resizeTmuxWaitDone(t, tm, home, 1)
	resizeTmuxSend(tm, "run the tests and show me a very long separator line")
	resizeTmuxWaitDone(t, tm, home, 2)
	resizeTmuxExpandAll(tm)

	if s := resizeTmuxSettled(tm, 100); !strings.Contains(s, "╭") {
		t.Fatalf("expand_all opened no box at 100x30, nothing to re-wrap:\n%s", s)
	}
	resizeTmuxSweep(t, tm, "expanded")
}

// TestResizeTmuxTranscriptPreserved shrinks to the smallest size and
// back: the earlier turn must still be there, re-wrapped, not lost.
func TestResizeTmuxTranscriptPreserved(t *testing.T) {
	t.Parallel()
	tape := resizeTmuxTape(t)
	tm, home := resizeTmuxStart(t, 100, 30, replayConfig(tape))
	resizeTmuxSend(tm, "list the files here")
	resizeTmuxWaitDone(t, tm, home, 1)

	const marker = "Two Go files" // the first turn's answer
	for _, sz := range resizeTmuxSizes {
		tm.resize(sz[0], sz[1])
		s := resizeTmuxSettled(tm, sz[0])
		if sz[1] < 20 {
			continue // too few rows to hold the turn: scrolled off, not lost
		}
		if !strings.Contains(s, marker) {
			t.Fatalf("%dx%d: %q gone from the transcript:\n%s", sz[0], sz[1], marker, s)
		}
	}
	tm.resize(100, 30)
	if s := resizeTmuxSettled(tm, 100); !strings.Contains(s, marker) {
		t.Fatalf("back at 100x30: %q gone from the transcript:\n%s", marker, s)
	}
	resizeTmuxCheck(t, tm, "back at 100x30", 100)
}
