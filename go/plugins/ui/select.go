package ui

// Mouse selection in the transcript: press-drag-release over the
// viewport highlights the swept text (reverse video) and copies its
// plain form to the clipboard via OSC 52. A plain click (no drag)
// keeps its old meaning (toggle a block, pick an ask option); the next
// press clears the highlight. Coordinates are content rows (viewport
// offset applied) and cells, so the selection survives scrolling.

import (
	"slices"
	"strconv"
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

type selection struct {
	pressed  bool // left button down inside the transcript
	active   bool // moved since the press: a real selection
	ax, ay   int  // anchor cell/row (content coordinates)
	cx, cy   int  // current cell/row
	pressRow int  // viewport row of the press, for the click fallback
}

// pressSelect records a left press over the transcript and clears any
// previous highlight. It does not act: the click meaning is decided on
// release, once we know whether the mouse moved.
func (m *model) pressSelect(mouse tea.Mouse) {
	was := m.sel.active
	m.sel = selection{pressed: true, ax: mouse.X, ay: mouse.Y + m.vp.YOffset(),
		cx: mouse.X, cy: mouse.Y + m.vp.YOffset(), pressRow: mouse.Y}
	if was {
		m.refresh()
	}
}

// dragSelect extends the selection to the current cell while the left
// button is held.
func (m *model) dragSelect(mouse tea.Mouse) {
	if !m.sel.pressed {
		return // a press is the only proof a drag needs; terminals differ
		// on whether motion reports carry the held button
	}
	y := mouse.Y
	if y >= m.vp.Height() {
		y = m.vp.Height() - 1
	}
	if y < 0 {
		y = 0
	}
	cx, cy := mouse.X, y+m.vp.YOffset()
	if cx == m.sel.cx && cy == m.sel.cy {
		return
	}
	m.sel.cx, m.sel.cy, m.sel.active = cx, cy, true
	m.refresh()
}

// releaseSelect ends a drag: a real selection is copied and stays
// highlighted; a plain click falls through to the click handler.
func (m *model) releaseSelect(mouse tea.Mouse) tea.Cmd {
	if !m.sel.pressed {
		return nil
	}
	m.sel.pressed = false
	if !m.sel.active {
		return m.clickTranscript(mouse.Y)
	}
	text := m.selectedText()
	if text == "" {
		m.sel.active = false
		m.refresh()
		return nil
	}
	n := strings.Count(text, "\n") + 1
	if n == 1 {
		m.flash = "copied " + plural(len([]rune(text)), "char")
	} else {
		m.flash = "copied " + plural(n, "line")
	}
	return tea.SetClipboard(text)
}

func plural(n int, unit string) string {
	if n == 1 {
		return "1 " + unit
	}
	return strconv.Itoa(n) + " " + unit + "s"
}

// bounds returns the selection in reading order: (row, cell) start and
// end, end exclusive on the cell.
func (s selection) bounds() (r0, c0, r1, c1 int) {
	r0, c0, r1, c1 = s.ay, s.ax, s.cy, s.cx
	if r1 < r0 || (r1 == r0 && c1 < c0) {
		r0, c0, r1, c1 = r1, c1, r0, c0
	}
	return r0, c0, r1, c1 + 1
}

// selectedText is the plain text under the highlight, lines joined
// with newlines and trailing blanks trimmed.
func (m *model) selectedText() string {
	if !m.sel.active {
		return ""
	}
	r0, c0, r1, c1 := m.sel.bounds()
	var out []string
	for r := r0; r <= r1 && r < len(m.lines); r++ {
		plain := ansi.Strip(m.lines[r])
		left, right := 0, ansi.StringWidth(plain)
		if r == r0 {
			left = c0
		}
		if r == r1 {
			right = min(right, c1)
		}
		if left >= right {
			out = append(out, "")
			continue
		}
		out = append(out, strings.TrimRight(ansi.Cut(plain, left, right), " "))
	}
	return strings.TrimRight(strings.Join(out, "\n"), "\n")
}

// highlight applies reverse video to the selected span of each content
// line, keeping the other styling intact.
func (m *model) highlight(lines []string, cfg *uiCfg) []string {
	if !m.sel.active {
		return lines
	}
	r0, c0, r1, c1 := m.sel.bounds()
	hl := cfg.theme["focus"].Reverse(true)
	out := slices.Clone(lines)
	for r := r0; r <= r1 && r < len(out); r++ {
		line := out[r]
		w := ansi.StringWidth(line)
		left, right := 0, w
		if r == r0 {
			left = c0
		}
		if r == r1 {
			right = min(w, c1)
		}
		if left >= w {
			continue
		}
		mid := ansi.Cut(line, left, right)
		out[r] = ansi.Cut(line, 0, left) + hl.Render(ansi.Strip(mid)) + ansi.Cut(line, right, w)
	}
	return out
}
