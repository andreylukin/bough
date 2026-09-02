package ui

// The "/" command palette, ported from old bough (plugins/commands/
// src/palette.rs + plugins/tui-shell/src/palette.rs). Invariants:
// the palette is STATE plus a PURE filter and never dispatches by
// itself — the composer owns open/close (a "/" at line start, see
// syncPalette in model.go); filtering is three tiers keyed on the
// NAME alone, so a growing query only ever removes rows and the
// selection cannot be swapped out under the typist; the overlay is
// drawn directly above the composer and never reserves rows it has
// no content for.

import (
	"sort"
	"strings"

	"github.com/andreylukin/bough/plugins/commands"
)

// palMaxRows caps the overlay height (old bough's 10).
const palMaxRows = 10

// palette is the "/" palette. State only.
type palette struct {
	open     bool
	selected int
	escaped  bool   // Esc pressed: stay closed until the draft changes
	escAt    string // the draft when Esc closed the palette

	// Tab cycling: the first Tab remembers the query it completed
	// from and the draft it wrote; while the draft is still that
	// completion, the palette keeps filtering on the remembered
	// query and the next Tab moves to the next match.
	cycling    bool
	cycleQuery string
	cycleDraft string
}

// paletteItem is one row: a command name with its usage and summary.
// skill rows rank below built-ins and wear a dim name.
type paletteItem struct {
	name    string
	usage   string
	summary string
	skill   bool
}

// paletteFilter is PURE: prefix matches first, then substring, then
// fzf-style SUBSEQUENCE (the query's characters in order, anywhere in
// the name — "mnr" finds "monarch"); within a tier built-ins rank
// above skills, each group alphabetical, and with an empty query
// "/help" pins to the top so Enter on a bare "/" is always harmless.
// The order depends on the NAME (and kind) alone, never on
// registration order or the query length, so a growing query only
// ever REMOVES rows: a row may DEMOTE a tier as the query grows ("dr"
// is a prefix of "drift" but "drf" only a subsequence), which keeps
// it on screen rather than dropping it, and the row under the cursor
// is never swapped out.
func paletteFilter(all []paletteItem, query string) []paletteItem {
	q := strings.ToLower(strings.TrimLeft(strings.TrimSpace(query), "/"))
	var prefix, substr, subseq []paletteItem
	for _, it := range all {
		name := strings.ToLower(it.name)
		switch {
		case q == "" || strings.HasPrefix(name, q):
			prefix = append(prefix, it)
		case strings.Contains(name, q):
			substr = append(substr, it)
		case subsequence(name, q):
			subseq = append(subseq, it)
		}
	}
	rank := func(it paletteItem) int {
		switch {
		case it.skill:
			return 2
		case q == "" && it.name == "help":
			return 0
		}
		return 1
	}
	for _, tier := range [][]paletteItem{prefix, substr, subseq} {
		sort.Slice(tier, func(i, j int) bool {
			if a, b := rank(tier[i]), rank(tier[j]); a != b {
				return a < b
			}
			return tier[i].name < tier[j].name
		})
	}
	return append(append(prefix, substr...), subseq...)
}

// subsequence reports whether hay contains needle's runes in order
// (both already lowercase).
func subsequence(hay, needle string) bool {
	h := []rune(hay)
	i := 0
	for _, n := range needle {
		for i < len(h) && h[i] != n {
			i++
		}
		if i == len(h) {
			return false
		}
		i++
	}
	return true
}

// paletteAction is what a key did to the palette.
type paletteAction int

const (
	palPass     paletteAction = iota // not the palette's key: falls through to the composer
	palMoved                         // Up/Down: selection moved
	palComplete                      // Tab: rewrite the composer to "/name " and stay open
	palAccept                        // Enter: dispatch this name
	palClose                         // Esc
)

// onKey is PURE on the state: Up/Down move the selection WRAPPING at
// both ends, Tab completes, Enter accepts, Esc closes, and anything
// else falls through to the composer (which re-filters). An empty
// item list answers only Esc: there is nothing to move to, complete
// or accept, and a palette that swallowed Enter over no items would
// eat a message.
func (p *palette) onKey(key string, items []paletteItem) (paletteAction, string) {
	if key == "esc" {
		p.open = false
		p.selected = 0
		return palClose, ""
	}
	if len(items) == 0 {
		p.selected = 0
		return palPass, ""
	}
	if p.selected >= len(items) {
		p.selected = len(items) - 1
	}
	n := len(items)
	switch key {
	case "up", "shift+tab":
		p.selected = (p.selected + n - 1) % n
		return palMoved, ""
	case "down":
		p.selected = (p.selected + 1) % n
		return palMoved, ""
	case "tab":
		return palComplete, items[p.selected].name
	case "enter":
		p.open = false
		return palAccept, items[p.selected].name
	}
	return palPass, ""
}

// paletteWindow slides only far enough to keep the selection on
// screen, so a long list does not re-page under the cursor on every
// keystroke: (first, rows) of the visible slice.
func paletteWindow(nItems, selected, maxRows int) (first, rows int) {
	rows = maxRows
	if nItems < rows {
		rows = nItems
	}
	if rows <= 0 {
		return 0, 0
	}
	first = selected - (rows - 1)
	if first < 0 {
		first = 0
	}
	if first > nItems-rows {
		first = nItems - rows
	}
	return first, rows
}

// paletteLines renders the overlay: min(len(items), maxRows) lines —
// never a row it has no content for — the window slid to keep the
// selection visible. Each row is "/name usage" at body contrast
// padded to ONE shared usage column (the max on screen, capped at
// half the width so one long usage cannot push every summary off the
// right edge) plus the summary dimmed; the selected row is painted
// full-width on the "select" background so it reads as a bar. The
// overlay wears the composer's ground (the terminal default), so
// unselected rows carry no background.
func paletteLines(items []paletteItem, selected, width, maxRows int, th theme) []string {
	if len(items) == 0 || maxRows <= 0 || width <= 0 {
		return nil
	}
	if selected >= len(items) {
		selected = len(items) - 1
	}
	first, rows := paletteWindow(len(items), selected, maxRows)
	shown := items[first : first+rows]
	col := 0
	for _, it := range shown {
		if n := len([]rune(paletteLeft(it))); n > col {
			col = n
		}
	}
	if col > width/2 {
		col = width / 2
	}
	out := make([]string, rows)
	for i, it := range shown {
		out[i] = paletteRow(it, first+i == selected, width, col, th)
	}
	return out
}

// paletteLeft is a row's left cell: "/name", plus the usage hint when
// the command has one.
func paletteLeft(it paletteItem) string {
	if it.usage == "" {
		return "/" + it.name
	}
	return "/" + it.name + " " + it.usage
}

// paletteRow renders one row, clipped to width; a summary that does
// not fit ends in "…" at a word boundary rather than a cut word, and
// a skill row's name is dimmed so the group reads apart from the
// built-ins.
func paletteRow(it paletteItem, sel bool, width, col int, th theme) string {
	marker := "  "
	if sel {
		marker = "> "
	}
	head := clipRunes(marker+padRunes(paletteLeft(it), col), width)
	left := width - len([]rune(head)) - 2
	if sel {
		line := head
		if it.summary != "" && left > 0 {
			line += "  " + commands.Ellipsize(it.summary, left)
		}
		return th["select"].Render(padRunes(clipRunes(line, width), width))
	}
	if it.skill {
		head = th["dim"].Render(head)
	}
	if left <= 0 || it.summary == "" {
		return head
	}
	return head + "  " + th["dim"].Render(commands.Ellipsize(it.summary, left))
}

// padRunes pads s with spaces to n runes (never truncates).
func padRunes(s string, n int) string {
	if d := n - len([]rune(s)); d > 0 {
		return s + strings.Repeat(" ", d)
	}
	return s
}

// clipRunes hard-clips s to n runes.
func clipRunes(s string, n int) string {
	if r := []rune(s); len(r) > n {
		return string(r[:n])
	}
	return s
}
