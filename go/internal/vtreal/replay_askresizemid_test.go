package vtreal

// A pending tools.ask survives a resize: the tape asks with four
// options too long for a 40-column pane, and while the ask is pending
// the pane goes 120 -> 40 -> 120 under tmux. Each size must re-wrap
// the options inside the pane with the composer still on the last
// rows, and afterwards "3" must still pick option 3 (the answered
// one-liner names it and the turn resumes on the tape's next reply).

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// askResizeMidEnds are the last words of the four options: each must
// survive a re-wrap whole.
var askResizeMidEnds = []string{"ALPHAEND", "BRAVOEND", "CHARLIEEND", "DELTAEND"}

func TestAskResizeMid(t *testing.T) {
	t.Parallel()
	tape, err := filepath.Abs("testdata/replay/ask_resize_mid.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	tm, home := resizeTmuxStart(t, 120, 30, askConfig(tape))
	resizeTmuxSend(tm, "pick a route")
	tm.waitFor("DELTAEND")
	for _, cols := range []int{120, 40, 120} {
		tm.resize(cols, 30)
		where := fmt.Sprintf("ask pending @ %d cols", cols)
		resizeTmuxCheck(t, tm, where, cols)
		s := resizeTmuxSettled(tm, cols)
		for _, want := range []string{"Pick a route", "1.", "2.", "3.", "4.", "type a number"} {
			if !strings.Contains(s, want) {
				t.Errorf("%s: pending ask missing %q after resize:\n%s", where, want, s)
			}
		}
		// Options must wrap at word boundaries, not be sliced mid-word
		// by the pane-width safety net.
		t.Run(fmt.Sprintf("word_wrap_%d", cols), func(t *testing.T) {
			if os.Getenv("BOUGH_KNOWN_ASK_RESIZE_MID") == "" {
				t.Skip("known bug: renderAsk (plugins/ui/ask.go) emits unwrapped option rows and fit() Hardwraps them mid-word at narrow widths; set BOUGH_KNOWN_ASK_RESIZE_MID=1 to run")
			}
			for _, w := range askResizeMidEnds {
				if !strings.Contains(s, w) {
					t.Errorf("%s: option word %q sliced by the wrap:\n%s", where, w, s)
				}
			}
		})
		if t.Failed() {
			return
		}
	}
	resizeTmuxSend(tm, "3")
	resizeTmuxWaitDone(t, tm, home, 1)
	tm.waitFor("Route locked in.")
	s := resizeTmuxSettled(tm, 120)
	if !strings.Contains(s, "→ charlie option") {
		t.Errorf("3 did not pick option 3 (charlie):\n%s", s)
	}
}
