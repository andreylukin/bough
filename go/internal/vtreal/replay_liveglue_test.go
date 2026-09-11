package vtreal

// Live streaming block hygiene, replayed with a delay so the reply
// actually streams on the PTY: while a turn runs the live block grows
// under a cursor glyph and the composer stays pinned; when the turn
// ends the cursor, the spinner and the elapsed chip are all gone and
// the next user prompt starts its own row instead of being glued to
// the tail of the assistant's text.

import (
	"fmt"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// liveGlueConfig is the replay overlay with a per-word stream delay,
// so the live block is observable between two screen samples.
func liveGlueConfig(tape string, delayMS int) string {
	return strings.Replace(replayConfig(tape),
		fmt.Sprintf("config: {file: %q}", tape),
		fmt.Sprintf("config: {file: %q, delay_ms: %d}", tape, delayMS), 1)
}

const liveGlueCursor = "▌"

// bubbles' MiniDot frames: the status bar spinner while a turn runs.
const liveGlueSpinner = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"

// The in-flight elapsed chip: "12s · " or "2m05s · ".
var liveGlueElapsed = regexp.MustCompile(`(\d+s|\d+m\d\ds) · `)

func liveGlueHasSpinner(s string) bool { return strings.ContainsAny(s, liveGlueSpinner) }

// liveGlueLive is the live region: the screen from the last assistant
// header to the streaming cursor. It only grows while words arrive.
func liveGlueLive(s string) (string, bool) {
	i := strings.LastIndex(s, "● bough")
	j := strings.Index(s, liveGlueCursor)
	if i < 0 || j < i {
		return "", false
	}
	return s[i : j+len(liveGlueCursor)], true
}

// liveGlueTape is the fixture: two turns, the second with a code block.
func liveGlueTape(t *testing.T) string {
	t.Helper()
	p, err := filepath.Abs("testdata/replay/liveglue.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	return p
}

// liveGlueSettledChecks is everything a finished turn must hold.
func liveGlueSettledChecks(a *app, where string) {
	a.t.Helper()
	s := a.settled()
	if strings.Contains(s, liveGlueCursor) {
		a.t.Errorf("%s: streaming cursor %q still on screen after the turn ended:\n%s", where, liveGlueCursor, s)
	}
	if liveGlueHasSpinner(s) {
		a.t.Errorf("%s: spinner still in the status bar after the turn ended:\n%s", where, s)
	}
	if m := liveGlueElapsed.FindString(s); m != "" {
		a.t.Errorf("%s: elapsed chip %q still in the status bar after the turn ended:\n%s", where, m, s)
	}
	for i, l := range strings.Split(s, "\n") {
		if !strings.Contains(l, "❯") {
			continue
		}
		if !strings.HasPrefix(strings.TrimLeft(l, " "), "❯") {
			a.t.Errorf("%s: row %d glues text to the user marker (%q):\n%s", where, i, l, s)
		}
	}
	a.check(where)
}

func TestLiveGlue(t *testing.T) {
	t.Parallel()
	tape := liveGlueTape(t)

	// While the first reply streams: a cursor glyph is on screen, the
	// live region grows, and the composer never leaves the last rows.
	t.Run("streaming_grows_with_composer_pinned", func(t *testing.T) {
		t.Parallel()
		a := startCfg(t, 100, 30, liveGlueConfig(tape, 30))
		a.typeText("explain the layout")
		a.key(uv.KeyEnter, 0)

		var widths []int
		sawCursor, sawSpinner := false, false
		deadline := time.Now().Add(30 * time.Second)
		for time.Now().Before(deadline) && a.doneCount() < 1 {
			s := a.text()
			if liveGlueHasSpinner(s) {
				sawSpinner = true
			}
			if live, ok := liveGlueLive(s); ok {
				sawCursor = true
				if n := len([]rune(live)); len(widths) == 0 || widths[len(widths)-1] != n {
					widths = append(widths, n)
				}
				ls := strings.Split(s, "\n")
				if r := composerRow(ls); r < 0 || r < len(ls)-3 {
					t.Fatalf("composer not pinned while streaming (row %d of %d):\n%s", r, len(ls), s)
				}
			}
			time.Sleep(15 * time.Millisecond)
		}
		if !sawCursor {
			t.Fatalf("never saw the streaming cursor %q while the reply streamed:\n%s", liveGlueCursor, a.text())
		}
		if !sawSpinner {
			t.Fatalf("never saw the status-bar spinner while the turn ran:\n%s", a.text())
		}
		grew := 0
		for i := 1; i < len(widths); i++ {
			if widths[i] > widths[i-1] {
				grew++
			}
		}
		if grew < 2 {
			t.Fatalf("live block did not grow while streaming (sizes %v):\n%s", widths, a.text())
		}
		if !a.waitDone(1, 30*time.Second) {
			t.Fatalf("turn never finished:\n%s", a.text())
		}
		liveGlueSettledChecks(a, "after streamed turn")
	})

	// Both turns end clean, and the second user prompt lands on its own
	// row right under the first reply.
	t.Run("turn_end_leaves_no_live_chrome", func(t *testing.T) {
		t.Parallel()
		a := startCfg(t, 100, 30, liveGlueConfig(tape, 10))
		for i, in := range []string{"explain the layout", "count the go files"} {
			a.typeText(in)
			a.key(uv.KeyEnter, 0)
			if !a.waitDone(i+1, 60*time.Second) {
				t.Fatalf("turn %d never finished:\n%s", i+1, a.text())
			}
			liveGlueSettledChecks(a, fmt.Sprintf("turn %d", i+1))
			if t.Failed() {
				return
			}
		}
		s := a.settled()
		if !strings.Contains(s, "❯ count the go files") {
			t.Fatalf("second user line missing from the transcript:\n%s", s)
		}
	})
}
