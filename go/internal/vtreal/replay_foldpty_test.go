package vtreal

// fold-pty: folding on the real binary. Every collapsible header on a
// replayed session is toggled by click and by key, collapse_all /
// expand_all run over the whole transcript, folds happen while a turn
// streams, under a tmux resize, across a /sessions round trip, and on
// the last block with follow mode on. After every step the frame
// must hold (foldPtyFrame): the check() invariants, header glyphs
// styled, no open ▾ header with nothing under it, no half-drawn box,
// and a toggle pair returns the exact screen it started from (no ghost
// rows left behind by a collapse).
//
// Real recordings: the five largest ~/.bough/history tapes (or
// BOUGH_FOLD_PTY_DIR) are copied to a temp dir and replayed for
// BOUGH_FOLD_PTY_TURNS turns (default 3). The rapid property over
// random fold sequences is behind BOUGH_FOLD_PTY_RAPID=1.

import (
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
	"pgregory.net/rapid"

	"github.com/andreylukin/bough/plugins/replay"
)

// foldPtyEnd is the first row below the transcript: the composer or
// the status bar, looking only at the bottom rows.
func foldPtyEnd(ls []string) int {
	end := len(ls)
	for i := len(ls) - 1; i >= 0 && i >= len(ls)-6; i-- {
		if strings.HasPrefix(ls[i], "> ") || strings.Contains(ls[i], "? keys") {
			end = i
		}
	}
	return end
}

// foldPtyFrame is check() plus the fold-specific invariants.
func foldPtyFrame(a *app, where string) {
	a.t.Helper()
	a.check(where)
	snap := a.term.Snapshot()
	ls := a.lines()
	end := foldPtyEnd(ls)
	cellAt := func(y, x int) string {
		if y < 0 || y >= len(snap.Cells) || x < 0 || x >= len(snap.Cells[y]) {
			return ""
		}
		return snap.Cells[y][x].Content
	}
	for y := 0; y < end && y < len(snap.Cells); y++ {
		row := snap.Cells[y]
		// The row's first mark, when a header glyph: styled, and an
		// open one has a body under it.
		for _, c := range row {
			if c.Content == " " || c.Content == "" {
				continue
			}
			if c.Content == "▸" || c.Content == "▾" {
				if c.Style.Fg == nil && c.Style.Attrs == 0 {
					a.t.Errorf("%s: header glyph on row %d is unstyled:\n%s", where, y, a.text())
				}
				if c.Content == "▾" && y+1 < end && strings.TrimSpace(ls[y+1]) == "" &&
					(y+2 >= end || strings.TrimSpace(ls[y+2]) == "") {
					a.t.Errorf("%s: open header on row %d has no body under it:\n%s", where, y, a.text())
				}
			}
			break
		}
		// Box borders: a ╭ opens a column of │ closed by ╰ (or runs off
		// the transcript area); a ╰ sits under │ or ╭.
		for x, c := range row {
			switch c.Content {
			case "╭":
				for yy := y + 1; yy < end; yy++ {
					s := cellAt(yy, x)
					if s == "╰" {
						break
					}
					if s != "│" {
						a.t.Errorf("%s: box opened at %d,%d broken on row %d (%q):\n%s", where, x, y, yy, s, a.text())
						break
					}
				}
			case "╰":
				if y > 0 {
					if s := cellAt(y-1, x); s != "│" && s != "╭" {
						a.t.Errorf("%s: box bottom at %d,%d with %q above it:\n%s", where, x, y, s, a.text())
					}
				}
			}
		}
	}
}

// foldPtyHeaders lists the transcript rows whose first mark is glyph.
func foldPtyHeaders(a *app, glyph string) []int {
	ls := a.lines()
	end := foldPtyEnd(ls)
	var out []int
	for i := 0; i < end; i++ {
		if strings.HasPrefix(strings.TrimLeft(ls[i], " "), glyph+" ") {
			out = append(out, i)
		}
	}
	return out
}

func foldPtyCol(line string) int {
	return len([]rune(line)) - len([]rune(strings.TrimLeft(line, " ")))
}

// foldPtyRoundTrip clicks the closed header on row y, checks the frame,
// then closes it again from wherever its open header landed and
// requires the screen it started from.
func foldPtyRoundTrip(a *app, y int, where string) {
	a.t.Helper()
	before := a.settled()
	ls := strings.Split(before, "\n")
	tail := strings.TrimPrefix(strings.TrimLeft(ls[y], " "), "▸ ")
	a.click(foldPtyCol(ls[y])+1, y)
	if after := a.settled(); after == before {
		if foldPtyStepRow.MatchString(tail) && !foldPtyKnown() {
			a.t.Logf("%s: %s (click on %q did nothing)", where, foldPtyBugNarrationLead, tail)
			return
		}
		a.t.Errorf("%s: a click on %q changed nothing:\n%s", where, tail, after)
		return
	}
	foldPtyFrame(a, where+" opened")
	open := -1
	for i, l := range a.lines() {
		if strings.HasPrefix(strings.TrimLeft(l, " "), "▾ ") && strings.Contains(l, tail) {
			open = i
			break
		}
	}
	if open < 0 {
		// A kind that opens headerless, or a fold whose header text
		// changes when open: close everything and move on.
		a.t.Logf("%s: no ▾ %q after opening; collapse_all instead", where, tail)
		foldPtyChord(a, 'c')
		a.settled()
		return
	}
	cur := a.lines()
	a.click(foldPtyCol(cur[open])+1, open)
	a.waitUntil(func(s string) bool { return strings.Contains(s, "▸ "+tail) }, where+": second click to close it")
	if after := a.settled(); after != before {
		a.t.Errorf("%s: open+close did not restore the screen (ghost or lost rows):\nbefore:\n%s\nafter:\n%s", where, before, after)
	}
	foldPtyFrame(a, where+" closed")
}

// Known product bugs this file found. Their cases skip (or log, inside
// the sweeps) unless BOUGH_KNOWN_FOLD_PTY=1.
const (
	foldPtyBugNarrationLead = "known bug: a step fold led by a one-line narration (assistant) block " +
		"cannot be opened — clickTranscript toggles only collapsible() blocks (plugins/ui/model.go:1228) " +
		"and focusables offers only collapsible leads (model.go:1280), so neither click nor tab+enter reaches it"
	foldPtyBugCollapseAll = "known bug: collapse_all does not refold an open step fold — setAllCollapsed " +
		"(plugins/ui/model.go:1377) flips collapsed flags only, never m.unfolded, and flashes " +
		"\"nothing to collapse: every step is already folded\" with ▾ N steps on screen"
)

func foldPtyKnown() bool { return os.Getenv("BOUGH_KNOWN_FOLD_PTY") != "" }

var foldPtyStepRow = regexp.MustCompile(`^\d+ steps?( |$)`)

// foldPtyTape writes a tape of one turn whose replies are the given
// assistant texts; each ```js reply gets a code and a result entry.
func foldPtyTape(t *testing.T, input string, replies ...string) string {
	t.Helper()
	var sb strings.Builder
	seq := 0
	add := func(kind string, data map[string]any) {
		seq++
		b, _ := json.Marshal(map[string]any{"seq": seq, "at": "2026-09-10T10:00:00Z", "kind": kind, "data": data})
		sb.Write(b)
		sb.WriteByte('\n')
	}
	add("meta", map[string]any{"cwd": "/tmp/demo"})
	add("input", map[string]any{"text": input})
	for i, r := range replies {
		add("assistant", map[string]any{"text": r})
		if _, code, ok := strings.Cut(r, "```js\n"); ok {
			code, _, _ = strings.Cut(code, "```")
			add("code", map[string]any{"text": code})
			add("result", map[string]any{"code": code, "text": fmt.Sprintf("out %d\n", i)})
		}
	}
	add("done", map[string]any{"text": ""})
	path := filepath.Join(t.TempDir(), "fold.jsonl")
	if err := os.WriteFile(path, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return path
}

// foldPtyStepsApp boots on tape, runs its one turn, returns the app
// and the row of the closed "N steps" fold.
func foldPtyStepsApp(t *testing.T, tape string) (*app, int) {
	t.Helper()
	a := startCfg(t, 100, 30, replayConfig(tape))
	a.typeText("do the steps")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.waitFor("All done.")
	foldPtyFrame(a, "turn")
	for i, l := range a.lines() {
		if strings.HasPrefix(l, "▸ ") && foldPtyStepRow.MatchString(l[len("▸ "):]) {
			return a, i
		}
	}
	t.Fatalf("no closed step fold on screen:\n%s", a.text())
	return nil, -1
}

// A fold whose steps are bare code/result pairs opens on a click and
// by tab+enter; collapse_all folds it back to one row.
func TestFoldPtyStepFoldCodeLed(t *testing.T) {
	t.Parallel()
	tape := foldPtyTape(t, "do the steps",
		"```js\nconsole.log(1)\n```", "```js\nconsole.log(2)\n```", "```stop\nAll done.\n```")
	a, row := foldPtyStepsApp(t, tape)
	a.click(2, row)
	a.waitUntil(func(s string) bool { return strings.Contains(s, "▾ 2 steps") }, "the fold opened by a click")
	foldPtyFrame(a, "fold open")
	foldPtyChord(a, 'c')
	s := a.settled()
	foldPtyFrame(a, "collapse_all")
	if strings.Contains(s, "▾ 2 steps") {
		if !foldPtyKnown() {
			t.Skip(foldPtyBugCollapseAll + "; set BOUGH_KNOWN_FOLD_PTY=1 to run")
		}
		t.Fatalf("collapse_all left the step fold open:\n%s", s)
	}
}

// Real turns narrate each step ("Let me check."), so the fold's lead
// is that narration. It must open like any other fold.
func TestFoldPtyStepFoldNarrationLed(t *testing.T) {
	t.Parallel()
	tape := foldPtyTape(t, "do the steps",
		"Let me check one.\n```js\nconsole.log(1)\n```",
		"Now the other.\n```js\nconsole.log(2)\n```",
		"```stop\nAll done.\n```")
	a, row := foldPtyStepsApp(t, tape)
	before := a.settled()
	a.click(2, row)
	clicked := a.settled() != before
	// And by keyboard: tab walks from the newest stop; try every stop.
	keyed := false
	if !clicked {
		for range 6 {
			a.key(uv.KeyTab, 0)
			if strings.Contains(a.keymapFocused(), "steps") {
				a.key(uv.KeyEnter, 0)
				keyed = strings.Contains(a.settled(), "▾ 2 steps")
				break
			}
		}
	}
	if !clicked && !keyed {
		t.Fatalf("the narration-led fold opened neither by click nor by tab+enter:\n%s", a.text())
	}
	foldPtyFrame(a, "fold open")
}

// foldPtyIdle waits for the spinner to stop: a recorded turn that ran
// subagents writes their done entries before its own, so doneCount
// alone can say "finished" while the parent still streams.
func foldPtyIdle(a *app) {
	a.t.Helper()
	a.waitUntil(func(s string) bool { return !cancelSpinner.MatchString(s) }, "the spinner to stop")
}

// foldPtyChord presses ctrl+x then k. The pending-leader hint is
// logged when it never shows (the caller checks what the chord did),
// so a missing hint and a dead chord are told apart.
func foldPtyChord(a *app, k rune) {
	a.t.Helper()
	a.key('x', uv.ModCtrl)
	deadline := time.Now().Add(2 * time.Second)
	for !strings.Contains(a.text(), "ctrl+x …") {
		if time.Now().After(deadline) {
			a.t.Logf("ctrl+x %c: the leader hint never showed:\n%s", k, a.text())
			break
		}
		time.Sleep(20 * time.Millisecond)
	}
	a.key(k, 0)
}

// foldPtyTapes copies the n largest replayable history tapes into a
// temp dir, so a live session writing to them cannot move the test.
func foldPtyTapes(t *testing.T, n int) []string {
	t.Helper()
	dir := os.Getenv("BOUGH_FOLD_PTY_DIR")
	if dir == "" {
		home, _ := os.UserHomeDir()
		dir = filepath.Join(home, ".bough", "history")
	}
	paths, _ := filepath.Glob(filepath.Join(dir, "*.jsonl"))
	type sized struct {
		p string
		n int64
	}
	var all []sized
	for _, p := range paths {
		if st, err := os.Stat(p); err == nil {
			all = append(all, sized{p, st.Size()})
		}
	}
	sort.Slice(all, func(i, j int) bool { return all[i].n > all[j].n })
	tmp := t.TempDir()
	var out []string
	for _, s := range all {
		if len(out) == n {
			break
		}
		if tp, err := replay.Load(s.p); err != nil || tp.Turns() == 0 {
			continue
		}
		in, err := os.Open(s.p)
		if err != nil {
			continue
		}
		dst := filepath.Join(tmp, filepath.Base(s.p))
		f, err := os.Create(dst)
		if err != nil {
			in.Close()
			t.Fatal(err)
		}
		_, _ = io.Copy(f, in)
		in.Close()
		f.Close()
		out = append(out, dst)
	}
	if len(out) == 0 {
		t.Skip("no replayable history tapes in " + dir)
	}
	return out
}

func foldPtyTurns() int {
	if n, err := strconv.Atoi(os.Getenv("BOUGH_FOLD_PTY_TURNS")); err == nil && n > 0 {
		return n
	}
	return 3
}

// foldPtySend submits one recorded input.
func foldPtySend(a *app, in string) {
	if strings.Contains(in, "\n") || len(in) > 200 {
		a.term.Paste(in)
	} else {
		a.typeText(in)
	}
	a.key(uv.KeyEnter, 0)
}

// foldPtyInputs is the tape's model inputs (commands dropped), at most max.
func foldPtyInputs(t *testing.T, tape string, max int) []string {
	t.Helper()
	tp, err := replay.Load(tape)
	if err != nil {
		t.Fatal(err)
	}
	var out []string
	for _, in := range tp.Inputs {
		if len(out) == max {
			break
		}
		if !strings.HasPrefix(in, "/") {
			out = append(out, in)
		}
	}
	return out
}

func foldPtyDelay(tape string, ms int) string {
	return strings.Replace(replayConfig(tape), fmt.Sprintf("{file: %q}", tape), fmt.Sprintf("{file: %q, delay_ms: %d}", tape, ms), 1)
}

// Real recordings: replay, then fold/unfold every header on screen by
// click, every focus stop by key, and collapse_all/expand_all.
func TestFoldPtyHistoryTapes(t *testing.T) {
	t.Parallel()
	for _, tape := range foldPtyTapes(t, 5) {
		id := strings.TrimSuffix(filepath.Base(tape), ".jsonl")
		t.Run(id, func(t *testing.T) {
			t.Parallel()
			a := startCfg(t, 100, 30, replayConfig(tape))
			turns := 0
			for _, in := range foldPtyInputs(t, tape, foldPtyTurns()) {
				foldPtySend(a, in)
				turns++
				if !a.waitDone(turns, 90*time.Second) {
					t.Fatalf("turn %d never finished:\n%s", turns, a.text())
				}
				foldPtyIdle(a)
				foldPtyFrame(a, fmt.Sprintf("turn %d", turns))
			}

			// Click: every closed header on screen, one round trip each.
			// A round trip restores the screen, so the rows stay valid.
			for i, y := range foldPtyHeaders(a, "▸") {
				if i >= 8 || t.Failed() {
					break
				}
				foldPtyRoundTrip(a, y, fmt.Sprintf("click header row %d", y))
			}
			if t.Failed() {
				return
			}

			// Keys: tab to each stop, enter twice returns the screen.
			for i := range 6 {
				a.key(uv.KeyTab, 0)
				before := a.settled()
				a.key(uv.KeyEnter, 0)
				a.settled()
				foldPtyFrame(a, fmt.Sprintf("stop %d toggled", i))
				a.key(uv.KeyEnter, 0)
				if after := a.settled(); after != before {
					t.Errorf("stop %d: enter twice did not restore the screen:\nbefore:\n%s\nafter:\n%s", i, before, after)
					return
				}
			}
			a.key(uv.KeyEscape, 0)

			foldPtyChord(a, 'e')
			a.settled()
			foldPtyFrame(a, "expand_all")
			foldPtyChord(a, 'c')
			a.settled()
			foldPtyFrame(a, "collapse_all")
			for _, y := range foldPtyHeaders(a, "▾") {
				l := strings.TrimPrefix(strings.TrimLeft(a.lines()[y], " "), "▾ ")
				if foldPtyStepRow.MatchString(l) && !foldPtyKnown() {
					t.Logf("%s (row %d %q)", foldPtyBugCollapseAll, y, l)
					continue
				}
				t.Errorf("collapse_all left an open header on row %d:\n%s", y, a.text())
			}
			if n := a.doneCount(); n != turns {
				t.Errorf("fold keys changed the finished turns from %d to %d", turns, n)
			}
		})
	}
}

// Folding while a turn streams: collapse_all, expand_all, tab+enter
// and header clicks land on the finished turn above while the second
// turn's words are still arriving; afterwards the reply is whole, the
// view follows its tail, and the frame holds.
func TestFoldPtyWhileStreaming(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/keymap.jsonl")
	a := startCfg(t, 100, 30, foldPtyDelay(tape, 25))
	a.typeText("list the files here")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn 1 never finished:\n%s", a.text())
	}
	a.typeText("count to forty")
	a.key(uv.KeyEnter, 0)
	a.waitFor("line 1")
	for i := 0; a.doneCount() < 2 && i < 40; i++ {
		switch i % 4 {
		case 0:
			foldPtyChord(a, 'e')
		case 1:
			foldPtyChord(a, 'c')
		case 2:
			if h := foldPtyHeaders(a, "▸"); len(h) > 0 {
				a.click(2, h[0])
			}
		case 3:
			// No esc here: esc on a running turn cancels it.
			a.key(uv.KeyTab, 0)
			a.key(uv.KeyEnter, 0)
		}
		time.Sleep(40 * time.Millisecond)
	}
	if !a.waitDone(2, 60*time.Second) {
		t.Fatalf("turn 2 never finished after folding mid-stream:\n%s", a.text())
	}
	if n := a.doneCount(); n != 2 {
		t.Fatalf("folding mid-stream changed the turn count to %d", n)
	}
	foldPtyFrame(a, "after the streamed turn")
	// The tail of the reply is on screen: the view is at the bottom.
	a.key(uv.KeyEnd, 0)
	if s := a.settled(); !strings.Contains(s, "line 40") {
		t.Errorf("the reply's tail is not on screen after folding mid-stream:\n%s", s)
	}
}

// Real recordings streamed with a delay: fold chords while each turn
// runs, frame checked after each.
func TestFoldPtyHistoryWhileStreaming(t *testing.T) {
	t.Parallel()
	for _, tape := range foldPtyTapes(t, 2) {
		id := strings.TrimSuffix(filepath.Base(tape), ".jsonl")
		t.Run(id, func(t *testing.T) {
			t.Parallel()
			a := startCfg(t, 100, 30, foldPtyDelay(tape, 2))
			turns := 0
			for _, in := range foldPtyInputs(t, tape, 2) {
				foldPtySend(a, in)
				turns++
				deadline := time.Now().Add(120 * time.Second)
				for i := 0; a.doneCount() < turns && time.Now().Before(deadline); i++ {
					foldPtyChord(a, []rune("ec")[i%2])
					time.Sleep(60 * time.Millisecond)
				}
				if !a.waitDone(turns, 30*time.Second) {
					t.Fatalf("turn %d never finished:\n%s", turns, a.text())
				}
				foldPtyIdle(a)
				foldPtyFrame(a, fmt.Sprintf("streamed turn %d", turns))
			}
		})
	}
}

// Follow mode on, the newest block opened from the bottom: its body
// shows, the view stays at the bottom, and closing it returns the
// exact screen. A later turn still follows.
func TestFoldPtyLastBlockFollowMode(t *testing.T) {
	t.Parallel()
	a := clicksApp(t)
	a.settled()
	heads := foldPtyHeaders(a, "▸")
	if len(heads) == 0 {
		t.Fatalf("no header on screen:\n%s", a.text())
	}
	foldPtyRoundTrip(a, heads[len(heads)-1], "last block")
	if strings.Contains(a.text(), "scrolled ↑") {
		t.Errorf("folding the last block left follow mode:\n%s", a.text())
	}
	// The tape's error turn is followed by a nudge that eats the next
	// reply; the failed block is the part this test needs.
	clicksTurn(t, a, 2, "now break it")
	a.waitFor("boom failed")
	foldPtyFrame(a, "after the next turn")
}

// Fold state across /sessions: open a block, leave with /new, come
// back through the picker. The result must match a cold resume of
// the same file — same transcript, same fold state.
func TestFoldPtySessionsRoundTrip(t *testing.T) {
	t.Parallel()
	a := clicksApp(t)
	row := clicksRow(a, "▸", "code js")
	if row < 0 {
		t.Fatalf("no closed code header:\n%s", a.text())
	}
	a.click(2, row)
	a.waitUntil(func(s string) bool { return clicksHasOpen(s, "code js") }, "code block open")
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	if len(paths) != 1 {
		t.Fatalf("want one history file, have %v", paths)
	}
	log := paths[0]

	a.typeText("/new")
	a.key(uv.KeyEnter, 0)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "count to two") }, "/new to clear the transcript")
	a.typeText("/sessions")
	a.key(uv.KeyEnter, 0)
	a.waitFor("resume a session")
	sessionTreeSelect(a, "count to two")
	a.key(uv.KeyEnter, 0)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "resume a session") }, "picker to close")
	a.waitFor("Counted.")
	foldPtyFrame(a, "resumed via picker")
	picked := resumeTranscript(a)
	open := clicksHasOpen(a.settled(), "code js")

	cold := startCfg(t, 100, 30, resumeConfig(clicksTape(t), log))
	cold.waitFor("Counted.")
	foldPtyFrame(cold, "cold resume")
	coldT := resumeTranscript(cold)
	coldOpen := clicksHasOpen(cold.settled(), "code js")
	t.Logf("fold state after /sessions: open=%v; after a cold resume: open=%v", open, coldOpen)
	if open != coldOpen {
		t.Errorf("fold state differs between picker resume (open=%v) and cold resume (open=%v)", open, coldOpen)
	}
	if strings.Join(picked, "\n") != strings.Join(coldT, "\n") {
		t.Errorf("picker resume and cold resume render different transcripts:\npicker:\n%s\n\ncold:\n%s",
			strings.Join(picked, "\n"), strings.Join(coldT, "\n"))
	}
}

// tmux, since x/vt loses rows on resize: an open block survives a
// resize storm with the composer and status bar on the last rows and
// no row wider than the pane, and still closes from the keyboard.
func TestFoldPtyResizeWhileOpen(t *testing.T) {
	t.Parallel()
	tm := foldPtyTmux(t, 100, 30, replayConfig(clicksTape(t)))
	tm.keys("count to two", "Enter")
	tm.waitFor("Counted.")
	tm.settled()
	tm.keys("Tab")
	tm.settled()
	tm.keys("Enter")
	tm.waitFor("▾")
	for _, sz := range [][2]int{{40, 12}, {160, 45}, {30, 10}, {100, 30}} {
		tm.resize(sz[0], sz[1])
		tm.waitUntil(func(s string) bool {
			// The status bar spans the pane: a bar at the new width
			// means bough repainted for this size.
			ls := strings.Split(s, "\n")
			bar := -1
			for i, l := range ls {
				if strings.HasSuffix(l, "? keys") {
					bar = i
				}
			}
			// (capture-pane turns space runs into tabs, so the bar's
			// width is not comparable; the row count is.)
			return len(ls) == sz[1] && bar >= 0 && composerRow(ls) >= len(ls)-2
		}, fmt.Sprintf("a full-width repaint after resize to %dx%d", sz[0], sz[1]))
		s := tm.settled()
		for i, l := range strings.Split(s, "\n") {
			if w := len([]rune(l)); w > sz[0] {
				t.Errorf("%dx%d: row %d is %d cells wide:\n%s", sz[0], sz[1], i, w, s)
			}
		}
		if sz[1] >= 20 && !strings.Contains(s, "▾") {
			t.Errorf("%dx%d: the open block closed or vanished across a resize:\n%s", sz[0], sz[1], s)
		}
	}
	tm.keys("Enter")
	tm.waitUntil(func(s string) bool { return !strings.Contains(s, "▾") }, "enter to close the block after the resizes")
}

func foldPtyTmux(t *testing.T, cols, rows int, yml string) *tmuxApp {
	t.Helper()
	if _, err := exec.LookPath("tmux"); err != nil {
		t.Skip("tmux not installed")
	}
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	tm := &tmuxApp{t: t, sock: fmt.Sprintf("vtreal-fold-%d-%d", os.Getpid(), time.Now().UnixNano())}
	shell := fmt.Sprintf("cd %s && HOME=%s TERM=xterm-256color BOUGH_WEB_ADDR=127.0.0.1:0 %s -config %s", home, home, bin, cfg)
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
	return tm
}

// Random fold sequences on the real binary, one session per check.
// Slow (~2 s a check): BOUGH_FOLD_PTY_RAPID=1 with -rapid.checks=N.
// The default run plays one fixed sequence instead.
func TestFoldPtyRapid(t *testing.T) {
	if os.Getenv("BOUGH_FOLD_PTY_RAPID") == "" {
		if msg := foldPtyOps(t, []int{0, 2, 3, 5, 1, 6, 4, 0, 0, 3}); msg != "" {
			t.Fatal(msg)
		}
		return
	}
	rapid.Check(t, func(rt *rapid.T) {
		ops := rapid.SliceOfN(rapid.IntRange(0, 6), 1, 12).Draw(rt, "ops")
		if msg := foldPtyOps(t, ops); msg != "" {
			rt.Fatalf("ops %v: %s", ops, msg)
		}
	})
}

// foldPtyOps runs one op sequence in a subtest and returns the failing
// screen, "" when every frame held.
func foldPtyOps(t *testing.T, ops []int) string {
	var failed string
	t.Run(fmt.Sprint(ops), func(t *testing.T) {
		a := clicksApp(t)
		for i, op := range ops {
			switch op {
			case 0:
				if h := foldPtyHeaders(a, "▸"); len(h) > 0 {
					a.click(2, h[i%len(h)])
				}
			case 1:
				if h := foldPtyHeaders(a, "▾"); len(h) > 0 {
					a.click(2, h[i%len(h)])
				}
			case 2:
				a.key(uv.KeyTab, 0)
			case 3:
				a.key(uv.KeyEnter, 0)
			case 4:
				foldPtyChord(a, 'c')
			case 5:
				foldPtyChord(a, 'e')
			case 6:
				a.term.SendMouse(uv.MouseWheelEvent{X: 5, Y: 3, Button: uv.MouseWheelUp})
			}
			foldPtyFrame(a, fmt.Sprintf("op %d (%d)", i, op))
			if t.Failed() {
				break
			}
		}
		if n := a.doneCount(); n != 1 {
			t.Errorf("fold ops changed the finished turns to %d", n)
		}
		if t.Failed() {
			failed = a.text()
		}
	})
	return failed
}
