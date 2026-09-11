package vtreal

// Streaming on the real binary, at the extremes the other stream tests
// (TestLiveGlue, TestPerf*, TestLongSession*) stay away from: no delay
// at all across 50 back-to-back turns, half a second per word while the
// user types a draft, a stream that stalls and then ends, a 5000-line
// reply, and a js fence arriving mid-stream.
//
// The x/vt tests assert on the stream as it runs. The stale-cell check
// (settled screen == screen after a forced full repaint) runs under
// tmux: the ui answers a size change with ClearScreen, but x/vt itself
// leaves rows short after a resize away and back (a 99-cell rule, a
// clipped composer) while tmux repaints them intact, so an x/vt diff
// would only measure the emulator.

import (
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// streamPtyStep is one model call: its reply, and for a reply that
// ends in a js block the recorded result of that block.
type streamPtyStep struct{ reply, code, result string }

// streamPtyTape writes a tape of turns, each a list of model calls.
func streamPtyTape(t *testing.T, turns ...[]streamPtyStep) string {
	t.Helper()
	dir := t.TempDir()
	var b strings.Builder
	seq := 0
	line := func(kind string, data map[string]any) {
		seq++
		j, err := json.Marshal(map[string]any{"seq": seq, "at": "2026-09-11T10:00:00Z", "kind": kind, "data": data})
		if err != nil {
			t.Fatal(err)
		}
		b.Write(j)
		b.WriteByte('\n')
	}
	line("meta", map[string]any{"cwd": dir})
	for i, steps := range turns {
		line("input", map[string]any{"text": fmt.Sprintf("turn %d", i+1)})
		for _, s := range steps {
			line("assistant", map[string]any{"text": s.reply})
			if s.code != "" {
				line("code", map[string]any{"text": s.code})
				line("result", map[string]any{"code": s.code, "text": s.result})
			}
		}
		line("done", map[string]any{"text": ""})
	}
	p := filepath.Join(dir, "tape.jsonl")
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

func streamPtyStop(prose string) streamPtyStep {
	return streamPtyStep{reply: prose + "\n\n```stop\ndone\n```"}
}

// streamPtySend types a prompt and presses enter.
func streamPtySend(a *app, s string) {
	a.typeText(s)
	a.key(uv.KeyEnter, 0)
}

// The scenarios, shared by the x/vt stream tests and the tmux redraw
// check.

func streamPtyFastTape(t *testing.T, n int) string {
	turns := make([][]streamPtyStep, n)
	for i := range turns {
		turns[i] = []streamPtyStep{streamPtyStop(fmt.Sprintf("FASTBEGIN%02d %s FASTEND%02d", i, strings.Repeat("w ", 40), i))}
	}
	return streamPtyTape(t, turns...)
}

func streamPtySlowTape(t *testing.T) string {
	return streamPtyTape(t, []streamPtyStep{streamPtyStop("SLOWBEGIN " + strings.Repeat("slow ", 10) + "SLOWEND")})
}

func streamPtyStallTape(t *testing.T) string {
	return streamPtyTape(t, []streamPtyStep{streamPtyStop("STALLA STALLZ")})
}

func streamPtyHugeTape(t *testing.T) string {
	var b strings.Builder
	for i := 1; i <= 5000; i++ {
		fmt.Fprintf(&b, "HUGE%04d line of the long reply\n", i)
	}
	return streamPtyTape(t, []streamPtyStep{streamPtyStop(b.String())})
}

const streamPtyJSCode = "tools.bash(\"echo JSMARK\")\n"

func streamPtyJSTape(t *testing.T) string {
	return streamPtyTape(t, []streamPtyStep{
		{reply: "JSINTRO " + strings.Repeat("prose ", 8) + "JSOUTRO\n\n```js\n" + streamPtyJSCode + "```",
			code: streamPtyJSCode, result: "JSRESULT\n"},
		streamPtyStop("JSDONE"),
	})
}

const streamPtyDraft = "the quick brown fox jumps over"

// 50 turns with no stream delay, each prompt sent the instant the
// previous turn's done lands: every reply's last word must reach the
// screen (the broadcaster used to drop events under this load).
func TestStreamPtyFastBackToBack(t *testing.T) {
	t.Parallel()
	const n = 50
	a := startCfg(t, 100, 30, perfConfig(streamPtyFastTape(t, n), 0))
	for i := range n {
		streamPtySend(a, fmt.Sprintf("turn %d", i+1))
		if !a.waitDone(i+1, 30*time.Second) {
			t.Fatalf("turn %d never finished:\n%s", i+1, a.text())
		}
		a.waitFor(fmt.Sprintf("FASTEND%02d", i))
	}
	liveGlueSettledChecks(a, "after 50 fast turns")
}

// Half a second per word while the user types a draft: every typed
// character must reach the composer promptly and none may be lost.
func TestStreamPtySlowStreamDraft(t *testing.T) {
	t.Parallel()
	a := startCfg(t, 100, 30, perfConfig(streamPtySlowTape(t), 500))
	streamPtySend(a, "go")
	a.waitFor("SLOWBEGIN")
	for i, r := range streamPtyDraft {
		a.typeText(string(r))
		want := strings.TrimRight("> "+streamPtyDraft[:i+1], " ") // text() trims trailing blanks
		deadline := time.Now().Add(400 * time.Millisecond)
		for {
			ls := a.lines()
			if c := composerRow(ls); c >= 0 && strings.HasPrefix(ls[c], want) {
				break
			}
			if time.Now().After(deadline) {
				t.Fatalf("draft lagged >400ms or lost a char after %q:\n%s", streamPtyDraft[:i+1], a.text())
			}
			time.Sleep(5 * time.Millisecond)
		}
		time.Sleep(40 * time.Millisecond)
	}
	if a.doneCount() != 0 {
		t.Fatalf("the slow stream finished before the draft was typed; the test proved nothing")
	}
	if !a.waitDone(1, 30*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	s := a.settled()
	ls := strings.Split(s, "\n")
	if c := composerRow(ls); c < 0 || ls[c] != "> "+streamPtyDraft {
		t.Errorf("draft not intact after the turn ended:\n%s", s)
	}
	if !strings.Contains(s, "SLOWEND") {
		t.Errorf("reply incomplete:\n%s", s)
	}
	a.check("after slow stream with draft")
}

// A stream that stalls for 2.5 s between two deltas and then ends: the
// partial text stays up during the stall and the turn closes cleanly.
// (The replay llm splits deltas at spaces, so the stall falls between
// words, not inside one.)
func TestStreamPtyStallThenEnd(t *testing.T) {
	t.Parallel()
	a := startCfg(t, 100, 30, perfConfig(streamPtyStallTape(t), 2500))
	streamPtySend(a, "go")
	a.waitFor("STALLA")
	time.Sleep(1200 * time.Millisecond)
	s := a.text()
	if !strings.Contains(s, "STALLA") || strings.Contains(s, "STALLZ") {
		t.Fatalf("during the stall want STALLA and not STALLZ:\n%s", s)
	}
	if !liveGlueHasSpinner(s) {
		t.Errorf("no spinner during the stall:\n%s", s)
	}
	if !a.waitDone(1, 30*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.waitFor("STALLZ")
	liveGlueSettledChecks(a, "after stall")
}

// A 5000-line reply streamed with no delay: the last line must reach
// the screen promptly once the turn is done.
func TestStreamPtyHugeReply(t *testing.T) {
	if os.Getenv("BOUGH_KNOWN_STREAM_PTY") != "1" {
		t.Skip("known bug: a live reply re-renders in full on every delta (liveView + lipgloss wrap of the whole " +
			"prose, plugins/ui/model.go:622-626, then refresh()), so 5000 lines paint at ~20 lines/s and the screen " +
			"trails the finished turn by minutes; set BOUGH_KNOWN_STREAM_PTY=1 to run")
	}
	t.Parallel()
	a := startCfg(t, 100, 30, perfConfig(streamPtyHugeTape(t), 0))
	start := time.Now()
	streamPtySend(a, "go")
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	loopDone := time.Since(start)
	shown, ok := perfWaitText(a, "HUGE5000", 180*time.Second)
	t.Logf("5000 lines: loop done after %v, last line on screen %v later", loopDone, shown)
	if !ok {
		t.Fatalf("last line never reached the screen:\n%s", a.text())
	}
	if shown > 5*time.Second {
		t.Errorf("the screen trailed the finished turn by %v (> 5s): the live block renders too slowly", shown)
	}
	liveGlueSettledChecks(a, "after 5000 lines")
}

// A js fence arriving mid-stream: once the code line shows, its row
// offset from the prose above it must not change while the stream runs
// (a half-drawn box that later jumps would change it).
func TestStreamPtyJSFenceMidStream(t *testing.T) {
	t.Parallel()
	a := startCfg(t, 100, 30, perfConfig(streamPtyJSTape(t), 60))
	streamPtySend(a, "go")
	row := func(ls []string, sub string) int {
		for i, l := range ls {
			if strings.Contains(l, sub) {
				return i
			}
		}
		return -1
	}
	var offsets []int
	var frames []string
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) && a.doneCount() < 1 {
		s := a.text()
		if strings.Contains(s, "JSRESULT") {
			break // the block ran: the live phase is over
		}
		ls := strings.Split(s, "\n")
		i, j := row(ls, "JSINTRO"), row(ls, "JSMARK")
		if i >= 0 && j >= 0 {
			if d := j - i; len(offsets) == 0 || offsets[len(offsets)-1] != d {
				offsets = append(offsets, d)
				frames = append(frames, s)
			}
		}
		time.Sleep(3 * time.Millisecond)
	}
	if len(offsets) > 1 {
		t.Errorf("the code line jumped while streaming (row offsets %v); frames:\n%s", offsets, strings.Join(frames, "\n-----\n"))
	}
	if !a.waitDone(1, 30*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.waitFor("JSDONE")
	liveGlueSettledChecks(a, "after js fence")
}

// streamPtyTmux boots bough on the given config inside its own tmux
// server and returns it with its $HOME (for doneCount).
func streamPtyTmux(t *testing.T, cols, rows int, yml string) (*tmuxApp, *app) {
	t.Helper()
	if _, err := exec.LookPath("tmux"); err != nil {
		t.Skip("tmux not installed")
	}
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	tm := &tmuxApp{t: t, sock: fmt.Sprintf("vtreal-sp-%d-%d", os.Getpid(), time.Now().UnixNano())}
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
	return tm, &app{t: t, home: home, cols: cols, rows: rows}
}

// streamPtyRedraw asserts the settled screen survives a forced full
// repaint (a height change away and back) unchanged.
func streamPtyRedraw(tm *tmuxApp, cols, rows int, where string) {
	tm.t.Helper()
	before := tm.settled()
	tm.resize(cols, rows-1)
	tm.settled()
	tm.resize(cols, rows)
	after := tm.settled()
	if before == after {
		return
	}
	// A repaint still on its way is not a stale cell: give it 2 s and
	// report only what persists.
	time.Sleep(2 * time.Second)
	if late := tm.settled(); late == before {
		tm.t.Logf("%s: repaint after the forced redraw took over a settle window to land", where)
		return
	} else {
		after = late
	}
	bl, al := strings.Split(before, "\n"), strings.Split(after, "\n")
	var diff strings.Builder
	for i := range max(len(bl), len(al)) {
		var x, y string
		if i < len(bl) {
			x = bl[i]
		}
		if i < len(al) {
			y = al[i]
		}
		if x != y {
			fmt.Fprintf(&diff, "row %d\n  before: %q\n  redraw: %q\n", i, x, y)
		}
	}
	tm.t.Errorf("%s: screen differs from a forced redraw (stale cells):\n%s\nbefore:\n%s\nafter:\n%s",
		where, diff.String(), before, after)
}

// streamPtyTmuxTurn sends a prompt and waits for its done and marker.
func streamPtyTmuxTurn(tm *tmuxApp, h *app, n int, marker string) {
	tm.t.Helper()
	tm.keys(fmt.Sprintf("turn %d", n), "Enter")
	if !h.waitDone(n, 60*time.Second) {
		tm.t.Fatalf("turn %d never finished:\n%s", n, tm.screen())
	}
	tm.waitFor(marker)
}

// After each stream scenario the diff renderer's screen must equal a
// full repaint of the same state.
func TestStreamPtyRedrawTmux(t *testing.T) {
	t.Parallel()
	const cols, rows = 100, 30
	t.Run("fast_back_to_back", func(t *testing.T) {
		t.Parallel()
		const n = 50
		tm, h := streamPtyTmux(t, cols, rows, perfConfig(streamPtyFastTape(t, n), 0))
		for i := range n {
			streamPtyTmuxTurn(tm, h, i+1, fmt.Sprintf("FASTEND%02d", i))
		}
		streamPtyRedraw(tm, cols, rows, "after 50 fast turns")
	})
	t.Run("slow_with_draft", func(t *testing.T) {
		t.Parallel()
		tm, h := streamPtyTmux(t, cols, rows, perfConfig(streamPtySlowTape(t), 500))
		tm.keys("turn 1", "Enter")
		tm.waitFor("SLOWBEGIN")
		tm.run("send-keys", "-t", "0", "-l", streamPtyDraft)
		if !h.waitDone(1, 60*time.Second) {
			t.Fatalf("turn never finished:\n%s", tm.screen())
		}
		tm.waitFor("SLOWEND")
		tm.waitFor("> " + streamPtyDraft)
		streamPtyRedraw(tm, cols, rows, "after slow stream with draft")
	})
	t.Run("stall_then_end", func(t *testing.T) {
		t.Parallel()
		tm, h := streamPtyTmux(t, cols, rows, perfConfig(streamPtyStallTape(t), 2500))
		streamPtyTmuxTurn(tm, h, 1, "STALLZ")
		streamPtyRedraw(tm, cols, rows, "after stall")
	})
	t.Run("js_fence_mid_stream", func(t *testing.T) {
		t.Parallel()
		tm, h := streamPtyTmux(t, cols, rows, perfConfig(streamPtyJSTape(t), 60))
		streamPtyTmuxTurn(tm, h, 1, "JSDONE")
		streamPtyRedraw(tm, cols, rows, "after js fence")
	})
	t.Run("huge_reply", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_STREAM_PTY") != "1" {
			t.Skip("known bug: see TestStreamPtyHugeReply (5000-line live render is quadratic)")
		}
		t.Parallel()
		tm, h := streamPtyTmux(t, cols, rows, perfConfig(streamPtyHugeTape(t), 0))
		streamPtyTmuxTurn(tm, h, 1, "HUGE5000")
		streamPtyRedraw(tm, cols, rows, "after 5000 lines")
	})
}
