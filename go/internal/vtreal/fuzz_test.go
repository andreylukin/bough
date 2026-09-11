package vtreal

// A model-based property test on the real PTY: rapid draws a random
// sequence of user actions (typing, sending, wheel, clicks, esc), a
// tiny model tracks what must be true (turns sent, draft in the
// composer), and the screen is checked after every step. rapid
// shrinks a failing sequence to the shortest one. A boot per check
// makes this slow, so it is opt-in:
//
//	BOUGH_FUZZ_TUI=1 go test ./internal/vtreal -run TestFuzzTUI -rapid.checks=50 -parallel 8

import (
	"fmt"
	"os"
	"strings"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"
	"pgregory.net/rapid"
)

type fuzzModel struct {
	a     *app
	draft string
	sent  int
}

func (m *fuzzModel) Check(t *rapid.T) {
	s := m.a.settled()
	ls := strings.Split(s, "\n")
	if panicky.MatchString(s) {
		t.Fatalf("crash text on screen:\n%s", s)
	}
	if composerRow(ls) < 0 {
		t.Fatalf("composer off screen (draft %q, %d turns):\n%s", m.draft, m.sent, s)
	}
	if m.draft != "" && !strings.Contains(s, "> "+m.draft) {
		t.Fatalf("draft %q not in the composer:\n%s", m.draft, s)
	}
	for i, l := range ls {
		if w := len([]rune(l)); w > m.a.cols {
			t.Fatalf("row %d is %d cells wide in %d columns:\n%s", i, w, m.a.cols, s)
		}
	}
}

func TestFuzzTUI(t *testing.T) {
	if os.Getenv("BOUGH_FUZZ_TUI") == "" {
		t.Skip("set BOUGH_FUZZ_TUI=1")
	}
	t.Parallel()
	rapid.Check(t, func(rt *rapid.T) {
		cols := rapid.SampledFrom([]int{40, 80, 100, 160}).Draw(rt, "cols")
		rows := rapid.SampledFrom([]int{10, 24, 40}).Draw(rt, "rows")
		m := &fuzzModel{a: start(t, cols, rows)}
		rt.Repeat(map[string]func(*rapid.T){
			"type": func(rt *rapid.T) {
				w := rapid.StringMatching(`[a-z0-9]{1,8}`).Draw(rt, "word")
				if len(m.draft)+len(w)+1 > 30 {
					return
				}
				if m.draft != "" {
					w = " " + w
				}
				m.a.typeText(w)
				m.draft += w
			},
			"send": func(rt *rapid.T) {
				if m.draft == "" {
					return
				}
				m.a.key(uv.KeyEnter, 0)
				m.sent++
				m.a.waitFor(fmt.Sprintf("echo: %s", m.draft))
				m.draft = ""
			},
			"wheel": func(rt *rapid.T) {
				b := uv.MouseWheelUp
				if rapid.Bool().Draw(rt, "down") {
					b = uv.MouseWheelDown
				}
				for range rapid.IntRange(1, 5).Draw(rt, "n") {
					m.a.term.SendMouse(uv.MouseWheelEvent{X: 3, Y: 2, Button: b})
				}
			},
			"click": func(rt *rapid.T) {
				// Transcript rows only: the status bar and composer
				// open pickers, which are their own flows.
				m.a.click(rapid.IntRange(0, cols-1).Draw(rt, "x"), rapid.IntRange(0, max(rows-3, 1)).Draw(rt, "y"))
				if s := m.a.settled(); strings.Contains(s, "esc back") || strings.Contains(s, "esc to close") {
					m.a.key(uv.KeyEscape, 0)
				}
			},
			"": m.Check,
		})
	})
}
