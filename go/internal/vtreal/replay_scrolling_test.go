package vtreal

// Transcript navigation on a real terminal: wheel, pgup/pgdown,
// home/end, the "scrolled" status cue, follow mode, and drag
// selection. The transcript is a 30-turn synthetic tape (generated
// below), long enough that every turn cannot fit on one screen.
//
// The contract, read off plugins/ui:
//   - blocks.go scrollCue: while the transcript is not at the bottom
//     the status bar reads "scrolled ↑ N lines", prefixed
//     "↓ new output · " when blocks landed since (model.go newBelow).
//   - model.go refresh: output pins to the bottom only when the view
//     already was there — new output never yanks a reader back.
//   - model.go submit: sending a line does GotoBottom, so typing
//     resumes follow mode.
//   - composer.go: home/end scroll only on an empty draft; inside a
//     draft they are line start/end.
//   - select.go: press-drag-release selects and copies; it must not
//     move the viewport.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// scrollingTape writes a tape of n turns; turn i answers with a marker
// line and enough filler that the transcript is many screens tall.
func scrollingTape(t *testing.T, dir string, n int, words int) string {
	t.Helper()
	path := filepath.Join(dir, "scrolling.jsonl")
	f, err := os.Create(path)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	enc := json.NewEncoder(f)
	seq := 0
	write := func(kind, text string) {
		seq++
		e := map[string]any{
			"seq":  seq,
			"at":   time.Date(2026, 9, 10, 10, 0, 0, 0, time.UTC).Add(time.Duration(seq) * time.Second).Format(time.RFC3339),
			"kind": kind,
			"data": map[string]any{"text": text},
		}
		if err := enc.Encode(e); err != nil {
			t.Fatal(err)
		}
	}
	write("meta", "")
	for i := 1; i <= n; i++ {
		write("input", fmt.Sprintf("ask %d", i))
		body := fmt.Sprintf("%s\n%s", scrollingMark(i), strings.TrimSpace(strings.Repeat("filler ", words)))
		write("assistant", "```stop\n"+body+"\n```")
		write("done", "")
	}
	return path
}

func scrollingMark(i int) string { return fmt.Sprintf("MARKER-%02d", i) }

// scrollingApp boots the replay tape and replays every turn, leaving
// the transcript long and parked at the bottom.
func scrollingApp(t *testing.T, cols, rows, turns int) *app {
	t.Helper()
	tape := scrollingTape(t, t.TempDir(), turns, 40)
	a := startCfg(t, cols, rows, replayConfig(tape))
	for i := 1; i <= turns; i++ {
		a.typeText(fmt.Sprintf("ask %d", i))
		a.key(uv.KeyEnter, 0)
		a.waitFor(scrollingMark(i))
		scrollingIdle(t, a, i)
	}
	a.settled()
	return a
}

// scrollingSpinner is the status bar's running chip ("⠏ 3s").
var scrollingSpinner = regexp.MustCompile(`[\x{2800}-\x{28FF}] \d+s`)

// scrollingIdle waits until turn n is recorded done and the UI itself
// is idle. The done entry lands before the UI stops running, and an
// enter typed in that gap steers into the finished turn: the loop asks
// the model again and that eats the next turn's tape reply.
func scrollingIdle(t *testing.T, a *app, n int) {
	t.Helper()
	if !a.waitDone(n, 30*time.Second) {
		t.Fatalf("turn %d never finished:\n%s", n, a.text())
	}
	a.waitUntil(func(s string) bool { return !scrollingSpinner.MatchString(s) }, fmt.Sprintf("the UI to go idle after turn %d", n))
}

// scrollingWheel sends n wheel events over the transcript.
func scrollingWheel(a *app, n int, up bool) {
	b := uv.MouseWheelDown
	if up {
		b = uv.MouseWheelUp
	}
	for range n {
		a.term.SendMouse(uv.MouseWheelEvent{X: 5, Y: 3, Button: b})
	}
}

// scrollingAtBottom is the bottom state: the status bar carries no
// scroll cue.
func scrollingAtBottom(a *app) bool { return !strings.Contains(a.text(), "scrolled ↑") }

// scrollingHome clears the composer and parks the view at the bottom,
// so each subtest starts from the same place.
func scrollingHome(t *testing.T, a *app) {
	t.Helper()
	a.key('l', uv.ModCtrl) // clear_input: end scrolls only on an empty draft
	a.key(uv.KeyEnd, 0)
	a.waitUntil(func(string) bool { return scrollingAtBottom(a) }, "transcript back at the bottom")
	a.settled()
}

func TestScrolling(t *testing.T) {
	t.Parallel()
	const turns = 30
	a := scrollingApp(t, 100, 24, turns)
	a.check("after 30 turns")
	if !scrollingAtBottom(a) || !strings.Contains(a.text(), scrollingMark(turns)) {
		t.Fatalf("a finished turn should leave the view at the bottom:\n%s", a.text())
	}

	// Subtests share one boot and run in order; each parks the view at
	// the bottom first, so a failure cannot leak into the next.
	t.Run("WheelUpShowsCueAndKeepsComposer", func(t *testing.T) {
		scrollingHome(t, a)
		scrollingWheel(a, 4, true)
		a.waitUntil(func(s string) bool { return strings.Contains(s, "scrolled ↑") }, "the scrolled cue after wheel up")
		s := a.settled()
		if n := scrollingCueLines(t, a); n <= 0 {
			t.Fatalf("wheel up left %d lines below the view:\n%s", n, s)
		}
		ls := strings.Split(s, "\n")
		if r := composerRow(ls); r < 0 || r < len(ls)-3 {
			t.Fatalf("composer left the bottom while scrolled (row %d of %d):\n%s", r, len(ls), s)
		}
		a.check("wheel up")
	})

	t.Run("WheelDownClearsCue", func(t *testing.T) {
		scrollingHome(t, a)
		scrollingWheel(a, 6, true)
		a.waitUntil(func(s string) bool { return strings.Contains(s, "scrolled ↑") }, "the scrolled cue")
		scrollingWheel(a, 12, false)
		a.waitUntil(func(string) bool { return scrollingAtBottom(a) }, "the cue to clear at the bottom")
		if !strings.Contains(a.text(), scrollingMark(turns)) {
			t.Fatalf("the newest turn is not on screen at the bottom:\n%s", a.text())
		}
		a.check("wheel down")
	})

	t.Run("PageUpMovesFurtherThanWheel", func(t *testing.T) {
		scrollingHome(t, a)
		scrollingWheel(a, 1, true)
		a.settled()
		wheel := scrollingCueLines(t, a)
		scrollingHome(t, a)
		a.key(uv.KeyPgUp, 0)
		a.settled()
		page := scrollingCueLines(t, a)
		if page <= wheel {
			t.Fatalf("pgup moved %d lines, one wheel notch moved %d: a page should be further:\n%s", page, wheel, a.text())
		}
		a.check("pgup")
	})

	t.Run("PageDownReturnsToBottom", func(t *testing.T) {
		scrollingHome(t, a)
		a.key(uv.KeyPgUp, 0)
		a.waitUntil(func(s string) bool { return strings.Contains(s, "scrolled ↑") }, "the scrolled cue after pgup")
		for range 4 {
			a.key(uv.KeyPgDown, 0)
		}
		a.waitUntil(func(string) bool { return scrollingAtBottom(a) }, "the bottom after pgdown")
		a.check("pgdown")
	})

	t.Run("HomeGoesToTheTopEndComesBack", func(t *testing.T) {
		scrollingHome(t, a)
		a.key(uv.KeyHome, 0)
		a.waitFor(scrollingMark(1))
		s := a.settled()
		if !strings.Contains(s, "scrolled ↑") {
			t.Fatalf("no scrolled cue at the top of the transcript:\n%s", s)
		}
		if strings.Contains(s, scrollingMark(turns)) {
			t.Fatalf("the last turn is still on screen at the top:\n%s", s)
		}
		a.check("home")
		a.key(uv.KeyEnd, 0)
		a.waitUntil(func(string) bool { return scrollingAtBottom(a) }, "the bottom after end")
		if strings.Contains(a.text(), scrollingMark(1)) {
			t.Fatalf("the first turn is still on screen at the bottom:\n%s", a.text())
		}
		a.check("end")
	})

	// home/end are line start/end while a draft is in the composer:
	// they must not move the transcript (composer.go).
	t.Run("HomeWithADraftDoesNotScroll", func(t *testing.T) {
		scrollingHome(t, a)
		a.typeText("a draft")
		a.waitFor("> a draft")
		before := scrollingTranscript(a.settled())
		a.key(uv.KeyHome, 0)
		s := a.settled()
		if strings.Contains(s, "scrolled ↑") {
			t.Fatalf("home scrolled the transcript while a draft was in the composer:\n%s", s)
		}
		if got := scrollingTranscript(s); got != before {
			t.Fatalf("home with a draft moved the transcript:\nbefore:\n%s\nafter:\n%s", before, s)
		}
		a.key('l', uv.ModCtrl)
		a.waitUntil(func(s string) bool { return !strings.Contains(s, "a draft") }, "the draft to clear")
	})

	// Sending a line resumes follow mode: submit() pins to the bottom,
	// so the new turn is visible even though the view was scrolled up.
	t.Run("SubmitResumesFollowMode", func(t *testing.T) {
		scrollingHome(t, a)
		a.key(uv.KeyHome, 0)
		a.waitFor(scrollingMark(1))
		a.settled()
		a.typeText("one more")
		a.key(uv.KeyEnter, 0)
		a.waitUntil(func(s string) bool { return strings.Contains(s, "one more") && !strings.Contains(s, "scrolled ↑") },
			"the view to jump to the bottom on submit")
		// Past the end of the tape the replay plugin answers with a
		// stop block, so the turn still finishes at the bottom.
		if !a.waitDone(turns+1, 30*time.Second) {
			t.Fatalf("the extra turn never finished:\n%s", a.text())
		}
		s := a.settled()
		if strings.Contains(s, "scrolled ↑") {
			t.Fatalf("follow mode did not resume after submit:\n%s", s)
		}
		a.check("follow after submit")
	})

	// A drag over the transcript selects and copies; it must not move
	// the viewport (select.go works in content coordinates).
	t.Run("DragSelectDoesNotScroll", func(t *testing.T) {
		scrollingHome(t, a)
		a.key(uv.KeyPgUp, 0)
		a.waitUntil(func(s string) bool { return strings.Contains(s, "scrolled ↑") }, "the scrolled cue before the drag")
		before := scrollingTranscript(a.settled())
		row := 4
		a.term.SendMouse(uv.MouseClickEvent{X: 2, Y: row, Button: uv.MouseLeft})
		for x := 3; x <= 20; x++ {
			a.term.SendMouse(uv.MouseMotionEvent{X: x, Y: row, Button: uv.MouseLeft})
		}
		a.term.SendMouse(uv.MouseReleaseEvent{X: 20, Y: row, Button: uv.MouseLeft})
		a.waitUntil(func(s string) bool { return strings.Contains(s, "copied") }, "the copy flash after the drag")
		// The flash replaces the scroll cue on the status bar, so the
		// proof the view did not move is the transcript itself: the same
		// rows, modulo the reverse-video highlight (plain text here).
		if after := scrollingTranscript(a.settled()); after != before {
			t.Fatalf("the drag moved the transcript:\nbefore:\n%s\nafter:\n%s", before, after)
		}
		a.check("drag select")
	})
}

// scrollingTranscript is the screen without its last two rows (status
// bar and composer), which carry a spinner and the draft.
func scrollingTranscript(screen string) string {
	ls := strings.Split(screen, "\n")
	if len(ls) < 2 {
		return screen
	}
	return strings.Join(ls[:len(ls)-2], "\n")
}

// scrollingCueLines reads the "scrolled ↑ N lines" count off the
// status bar; 0 means the view is at the bottom (no cue).
func scrollingCueLines(t *testing.T, a *app) int {
	t.Helper()
	s := a.text()
	i := strings.Index(s, "scrolled ↑ ")
	if i < 0 {
		if strings.Contains(s, "scrolled ↑") {
			t.Fatalf("malformed scroll cue:\n%s", s)
		}
		return 0
	}
	rest := s[i+len("scrolled ↑ "):]
	var n int
	if _, err := fmt.Sscanf(rest, "%d lines", &n); err != nil {
		t.Fatalf("cannot read the scroll cue count from %q:\n%s", strings.SplitN(rest, "\n", 2)[0], s)
	}
	return n
}

// Output that lands while the reader is scrolled up must not yank the
// view down; the status bar says so instead ("↓ new output · scrolled
// ↑ N lines", blocks.go scrollCue + model.go newBelow). The tape
// streams word by word here, so there is a turn to scroll away from.
func TestScrollingNewOutputDoesNotFollow(t *testing.T) {
	t.Parallel()
	tape := scrollingTape(t, t.TempDir(), 2, 200)
	cfg := strings.Replace(replayConfig(tape),
		fmt.Sprintf("config: {file: %q}", tape),
		fmt.Sprintf("config: {file: %q, delay_ms: 25}", tape), 1)
	a := startCfg(t, 100, 24, cfg)

	a.typeText("ask 1")
	a.key(uv.KeyEnter, 0)
	a.waitFor(scrollingMark(1))
	scrollingIdle(t, a, 1)
	a.settled()

	// Second turn: scroll up while it is still streaming.
	a.typeText("ask 2")
	a.key(uv.KeyEnter, 0)
	a.waitFor(scrollingMark(2))
	// Turn 1 alone is taller than the pane, so a wheel notch always has
	// somewhere to go; keep nudging until the cue shows.
	a.waitUntil(func(s string) bool {
		scrollingWheel(a, 1, true)
		return strings.Contains(s, "scrolled ↑")
	}, "the scrolled cue mid-turn")
	a.waitUntil(func(s string) bool { return strings.Contains(s, "↓ new output") },
		"the new-output cue while scrolled up mid-turn")
	if !a.waitDone(2, 30*time.Second) {
		t.Fatalf("the second turn never finished:\n%s", a.text())
	}
	s := a.settled()
	if !strings.Contains(s, "scrolled ↑") {
		t.Fatalf("the finished turn pulled the scrolled-up view back to the bottom:\n%s", s)
	}
	a.check("new output while scrolled up")

	// End follows again.
	a.key(uv.KeyEnd, 0)
	a.waitUntil(func(string) bool { return scrollingAtBottom(a) }, "the bottom after end")
	if !strings.Contains(a.text(), scrollingMark(2)) {
		t.Fatalf("the newest turn is not on screen at the bottom:\n%s", a.text())
	}
}
