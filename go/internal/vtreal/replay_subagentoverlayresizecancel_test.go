package vtreal

// Subagent transcript overlay under resize and esc. Replay never spawns
// on its own, so this run mounts the real codemode and workers rows
// under a replay llm: the tape's first reply calls tools.spawn, the
// child's first reply runs a bash loop that blocks on a gate file, and
// the test only creates that file once it is done with the overlay. So
// the child is provably still running through the whole sequence.
//
// With the child blocked: open its transcript (tab + ctrl+o), resize
// 120 -> 40 -> 120 under tmux, press esc (overlay closes to the
// spawner, the turn is NOT cancelled), dive again and press esc a
// second time (same). Then release the gate: the child must finish ok,
// and the parent's history must match a run that never resized, entry
// for entry.
//
// Timing: the child's block runs under codemode's 30 s script timeout
// (tools.bash does not pause it), so everything between the spawn and
// the gate must fit well inside that. The spinner keeps the screen
// moving, so tmuxApp.settled never settles here (it burns its full
// 3.2 s); these helpers poll for the one thing they need instead.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// subagentOverlayResizeCancelTape writes the tape: parent spawns, the
// child waits on gate then reports, the parent answers.
func subagentOverlayResizeCancelTape(t *testing.T, dir, gate string) string {
	t.Helper()
	child := fmt.Sprintf("```js\ntools.bash(%q)\n```", fmt.Sprintf("while [ ! -f %s ]; do sleep 0.1; done; echo gate-open", gate))
	entries := []history.Entry{
		{Seq: 1, Kind: "meta", Data: map[string]any{"cwd": dir}},
		{Seq: 2, Kind: "input", Data: map[string]any{"text": "delegate the wait"}},
		{Seq: 3, Kind: "assistant", Data: map[string]any{"text": "Delegating.\n```js\ntools.spawn(\"wait for the gate file then report\")\n```"}},
		{Seq: 4, Kind: "assistant", Data: map[string]any{"text": child}},
		{Seq: 5, Kind: "assistant", Data: map[string]any{"text": "Status: ok\nFindings: the gate opened."}},
		{Seq: 6, Kind: "assistant", Data: map[string]any{"text": "The subagent reported back: the gate opened."}},
		{Seq: 7, Kind: "done", Data: map[string]any{}},
	}
	var sb strings.Builder
	for _, e := range entries {
		e.At = time.Date(2026, 9, 10, 12, 0, int(e.Seq), 0, time.UTC)
		b, err := json.Marshal(e)
		if err != nil {
			t.Fatal(err)
		}
		sb.Write(b)
		sb.WriteByte('\n')
	}
	p := filepath.Join(dir, "tape.jsonl")
	if err := os.WriteFile(p, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// subagentOverlayResizeCancelPoll polls the pane until pred holds or d
// passes; it returns the last screen and whether pred held.
func subagentOverlayResizeCancelPoll(tm *tmuxApp, d time.Duration, pred func(string) bool) (string, bool) {
	deadline := time.Now().Add(d)
	for {
		s := resizeTmuxScreen(tm)
		if pred(s) {
			return s, true
		}
		if time.Now().After(deadline) {
			return s, false
		}
		time.Sleep(40 * time.Millisecond)
	}
}

// subagentOverlayResizeCancelOpen: the overlay's first row names the
// subagent it shows (at 40 columns the bar's "esc to close" hint falls
// back to "? keys", so the bar is not the tell).
func subagentOverlayResizeCancelOpen(s string) bool {
	return strings.HasPrefix(strings.TrimSpace(s), "subagent 1 ·")
}

// subagentOverlayResizeCancelEsc is subagentsEsc for tmux: a bare ESC
// waits in the input parser for a disambiguating key, so a right-arrow
// follows it after a pause (back to back they read as alt+right).
func subagentOverlayResizeCancelEsc(tm *tmuxApp) {
	tm.keys("Escape")
	time.Sleep(200 * time.Millisecond)
	tm.keys("Right")
}

// subagentOverlayResizeCancelDive tabs until ctrl+o opens the child's
// transcript, and leaves it open.
func subagentOverlayResizeCancelDive(t *testing.T, tm *tmuxApp) {
	t.Helper()
	for range 8 {
		tm.keys("C-o")
		s, _ := subagentOverlayResizeCancelPoll(tm, 1500*time.Millisecond, func(s string) bool {
			return subagentOverlayResizeCancelOpen(s) || strings.Contains(s, "to close")
		})
		if subagentOverlayResizeCancelOpen(s) {
			return
		}
		if strings.Contains(s, "to close") {
			tm.keys("C-o") // the history inspector: its own key closes it
			time.Sleep(200 * time.Millisecond)
		}
		tm.keys("Tab")
		time.Sleep(200 * time.Millisecond)
	}
	t.Fatalf("never opened the running child's transcript\nscreen:\n%s", tm.screen())
}

// subagentOverlayResizeCancelClosed asserts esc closed the overlay back
// to the spawner and cancelled nothing.
func subagentOverlayResizeCancelClosed(t *testing.T, tm *tmuxApp, home, where string) {
	t.Helper()
	s, ok := subagentOverlayResizeCancelPoll(tm, 3*time.Second, func(s string) bool {
		return !subagentOverlayResizeCancelOpen(s) && !strings.Contains(s, "esc to close")
	})
	if !ok {
		t.Fatalf("%s: esc did not close the subagent overlay:\n%s", where, s)
	}
	time.Sleep(300 * time.Millisecond) // a cancel would land by now
	s = resizeTmuxScreen(tm)
	if strings.Contains(s, "cancelled") || strings.Contains(s, "cancelling") {
		t.Fatalf("%s: esc cancelled the turn instead of closing the overlay:\n%s", where, s)
	}
	if !strings.Contains(s, "delegate the wait") {
		t.Errorf("%s: not back on the spawner's transcript:\n%s", where, s)
	}
	if n := resizeTmuxDone(home); n != 0 {
		t.Fatalf("%s: the turn ended (%d done/cancelled) while the child was gated:\n%s", where, n, s)
	}
}

// subagentOverlayResizeCancelResize resizes and waits for a frame at the
// new width with the overlay still open.
func subagentOverlayResizeCancelResize(t *testing.T, tm *tmuxApp, cols, rows int) {
	t.Helper()
	tm.resize(cols, rows)
	s, ok := subagentOverlayResizeCancelPoll(tm, 5*time.Second, func(s string) bool {
		return resizeTmuxBarWidth(strings.Split(s, "\n")) >= cols-2 && subagentOverlayResizeCancelOpen(s)
	})
	if !ok {
		t.Fatalf("resize to %dx%d: no redrawn frame with the overlay still open:\n%s", cols, rows, s)
	}
	for i, l := range strings.Split(s, "\n") {
		if w := len([]rune(l)); w > cols {
			t.Errorf("resize to %dx%d: row %d is %d cells wide:\n%s", cols, rows, i, w, s)
		}
	}
}

// subagentOverlayResizeCancelRun drives one run and returns the
// parent's history (top-level kind + data, one JSON line each).
func subagentOverlayResizeCancelRun(t *testing.T, resize bool) []string {
	t.Helper()
	dir := t.TempDir()
	gate := filepath.Join(dir, "gate")
	tape := subagentOverlayResizeCancelTape(t, dir, gate)
	yml := strings.Replace(replayConfig(tape),
		fmt.Sprintf("plugin: replay\n  config: {file: %q, provide: codemode}", tape),
		"plugin: codemode", 1)
	tm, home := resizeTmuxStart(t, 120, 30, yml)

	resizeTmuxSend(tm, "delegate the wait")
	tm.waitFor("subagent 1")
	start := time.Now()
	subagentOverlayResizeCancelDive(t, tm)

	if resize {
		subagentOverlayResizeCancelResize(t, tm, 40, 30)
		subagentOverlayResizeCancelResize(t, tm, 120, 30)
	}

	subagentOverlayResizeCancelEsc(tm)
	subagentOverlayResizeCancelClosed(t, tm, home, "first esc")
	subagentOverlayResizeCancelDive(t, tm)
	subagentOverlayResizeCancelEsc(tm)
	subagentOverlayResizeCancelClosed(t, tm, home, "second esc")

	if el := time.Since(start); el > 25*time.Second {
		t.Fatalf("gated window took %s, too close to codemode's 30 s script timeout to prove anything", el)
	}
	if err := os.WriteFile(gate, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	resizeTmuxWaitDone(t, tm, home, 1)
	tm.waitFor("the gate opened")

	paths, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
	if len(paths) != 1 {
		t.Fatalf("want one session history, got %v", paths)
	}
	entries, err := history.Read(paths[0])
	if err != nil {
		t.Fatal(err)
	}
	var parent, sub []string
	subOK := false
	for _, e := range entries {
		b, _ := json.Marshal(e.Data)
		if strings.HasPrefix(e.Kind, "sub:") {
			sub = append(sub, e.Kind+" "+string(b))
		}
		if e.Kind == "cancelled" {
			t.Fatalf("the turn was cancelled: %v", e.Data)
		}
		if e.Kind == "sub:done" {
			subOK = e.Data["status"] == "ok"
		}
		if strings.HasPrefix(e.Kind, "sub:") || e.Kind == "meta" {
			continue
		}
		d := map[string]any{}
		for k, v := range e.Data {
			// Timings and usage vary run to run; the story does not.
			if k == "elapsed_ms" || k == "ms" || k == "usage" || k == "duration_ms" {
				continue
			}
			d[k] = v
		}
		b, _ = json.Marshal(d)
		parent = append(parent, e.Kind+" "+string(b))
	}
	if !subOK {
		t.Fatalf("the child did not finish ok; its entries:\n%s", strings.Join(sub, "\n"))
	}
	return parent
}

func TestSubagentOverlayResizeCancel(t *testing.T) {
	t.Parallel()
	var base, resized []string
	t.Run("no-resize", func(t *testing.T) { base = subagentOverlayResizeCancelRun(t, false) })
	t.Run("resize", func(t *testing.T) { resized = subagentOverlayResizeCancelRun(t, true) })
	if t.Failed() {
		return
	}
	if strings.Join(base, "\n") != strings.Join(resized, "\n") {
		t.Fatalf("parent transcript differs after resize\nno-resize:\n%s\nresize:\n%s",
			strings.Join(base, "\n"), strings.Join(resized, "\n"))
	}
}
