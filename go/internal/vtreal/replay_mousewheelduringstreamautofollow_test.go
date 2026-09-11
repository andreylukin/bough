package vtreal

// Wheel up while a reply is still streaming, let the deltas keep
// landing, then wheel back down. While scrolled up the viewport must
// not move: the top visible line stays put as the scroll cue's
// "lines below" count grows (proof the deltas did land). Wheeling back
// to the bottom mid-stream resumes follow mode, and after the turn the
// final line is on screen at the bottom.
//
// The replay seam has no step-by-step gate, so the tape is paced with
// delay_ms (as the theme and cancel tests do): each word is one delta.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// mouseWheelDuringStreamAutofollowLines is how many lines turn 2
// streams; each line is two words, so two deltas.
const mouseWheelDuringStreamAutofollowLines = 150

func mouseWheelDuringStreamAutofollowLine(i int) string { return fmt.Sprintf("STREAM-%03d", i) }

// mouseWheelDuringStreamAutofollowTape: turn 1 is a tall reply to have
// something to scroll into, turn 2 the long streaming one.
func mouseWheelDuringStreamAutofollowTape(t *testing.T) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "wheelstream.jsonl")
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
			"at":   time.Date(2026, 9, 11, 10, 0, 0, 0, time.UTC).Add(time.Duration(seq) * time.Second).Format(time.RFC3339),
			"kind": kind,
			"data": map[string]any{"text": text},
		}
		if err := enc.Encode(e); err != nil {
			t.Fatal(err)
		}
	}
	var one, two []string
	for i := 1; i <= 40; i++ {
		one = append(one, fmt.Sprintf("EARLY-%02d x", i))
	}
	for i := 1; i <= mouseWheelDuringStreamAutofollowLines; i++ {
		two = append(two, mouseWheelDuringStreamAutofollowLine(i)+" y")
	}
	write("meta", "")
	write("input", "ask 1")
	write("assistant", "```stop\n"+strings.Join(one, "\n")+"\n```")
	write("done", "")
	write("input", "ask 2")
	write("assistant", "```stop\n"+strings.Join(two, "\n")+"\n```")
	write("done", "")
	return path
}

// mouseWheelDuringStreamAutofollowTop is the first non-blank row.
func mouseWheelDuringStreamAutofollowTop(a *app) string {
	for _, l := range a.lines() {
		if strings.TrimSpace(l) != "" {
			return l
		}
	}
	return ""
}

// mouseWheelDuringStreamAutofollowNewest is the highest STREAM-NNN on
// screen, 0 when none is.
func mouseWheelDuringStreamAutofollowNewest(s string) int {
	for i := mouseWheelDuringStreamAutofollowLines; i > 0; i-- {
		if strings.Contains(s, mouseWheelDuringStreamAutofollowLine(i)) {
			return i
		}
	}
	return 0
}

func TestMouseWheelDuringStreamAutofollow(t *testing.T) {
	t.Parallel()
	a := startCfg(t, 100, 24, cancelConfig(mouseWheelDuringStreamAutofollowTape(t), 30))

	a.typeText("ask 1")
	a.key(uv.KeyEnter, 0)
	a.waitFor("EARLY-40")
	scrollingIdle(t, a, 1)
	a.settled()

	a.typeText("ask 2")
	a.key(uv.KeyEnter, 0)
	a.waitFor(mouseWheelDuringStreamAutofollowLine(5))

	t.Run("ScrolledUpViewDoesNotJump", func(t *testing.T) {
		scrollingWheel(a, 8, true)
		a.waitUntil(func(s string) bool { return strings.Contains(s, "scrolled ↑") }, "the scrolled cue mid-stream")
		time.Sleep(150 * time.Millisecond) // let queued wheel events drain
		top := mouseWheelDuringStreamAutofollowTop(a)
		below := scrollingCueLines(t, a)
		// Watch ~1.5s of deltas (about 50 words at 30ms).
		deadline := time.Now().Add(1500 * time.Millisecond)
		for time.Now().Before(deadline) {
			if got := mouseWheelDuringStreamAutofollowTop(a); got != top {
				t.Fatalf("top visible line moved while scrolled up: %q -> %q\n%s", top, got, a.text())
			}
			time.Sleep(25 * time.Millisecond)
		}
		if a.doneCount() >= 2 {
			t.Fatalf("the stream ended during the watch window; the scenario is vacuous:\n%s", a.text())
		}
		if got := scrollingCueLines(t, a); got <= below {
			t.Fatalf("no deltas landed while scrolled up (lines below %d -> %d):\n%s", below, got, a.text())
		}
		if s := a.text(); !strings.Contains(s, "↓ new output") {
			t.Fatalf("no new-output cue while scrolled up mid-stream:\n%s", s)
		}
	})

	t.Run("WheelToBottomResumesFollow", func(t *testing.T) {
		a.waitUntil(func(s string) bool {
			scrollingWheel(a, 3, false)
			return !strings.Contains(s, "scrolled ↑")
		}, "the bottom after wheel down")
		if a.doneCount() >= 2 {
			t.Skip("the stream ended before the wheel reached the bottom; follow mid-stream not observable")
		}
		at := mouseWheelDuringStreamAutofollowNewest(a.text())
		// Later deltas must keep the view at the bottom, newest on screen.
		a.waitUntil(func(s string) bool {
			return mouseWheelDuringStreamAutofollowNewest(s) >= at+10
		}, fmt.Sprintf("following output past %s", mouseWheelDuringStreamAutofollowLine(at+10)))
		if s := a.text(); strings.Contains(s, "scrolled ↑") {
			t.Fatalf("follow did not resume: the view fell behind the stream:\n%s", s)
		}
	})

	t.Run("FinalLineVisibleAfterEnd", func(t *testing.T) {
		if !a.waitDone(2, 60*time.Second) {
			t.Fatalf("the streaming turn never finished:\n%s", a.text())
		}
		a.settled()
		scrollingWheel(a, 8, true)
		a.waitUntil(func(s string) bool { return strings.Contains(s, "scrolled ↑") }, "the scrolled cue after the turn")
		a.key(uv.KeyEnd, 0)
		a.waitUntil(func(string) bool { return scrollingAtBottom(a) }, "the bottom after end")
		last := mouseWheelDuringStreamAutofollowLine(mouseWheelDuringStreamAutofollowLines)
		if s := a.settled(); !strings.Contains(s, last) {
			t.Fatalf("final line %s not visible after end:\n%s", last, s)
		}
		a.check("after end")
	})
}
