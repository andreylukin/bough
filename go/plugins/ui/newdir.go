package ui

// The new-session dialog: "/new <query>" opens a centered, bordered
// box over the transcript listing this directory's subdirectories,
// fuzzy-filtered fzf-style by the query (see paletteFilter). Up/Down
// move, Tab completes the draft to "/new <dir>", Enter starts the new
// session there ("/new <dir>" dispatches), Esc closes it until the
// draft changes.

import (
	"os"
	"path/filepath"
	"slices"
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

// newDirPrefix is the draft that opens the dialog.
const newDirPrefix = "/new "

// newDirMaxDepth bounds the directory walk below cwd.
const newDirMaxDepth = 3

// newDirQuery is the dialog's query when the draft is "/new <query>".
func newDirQuery(draft string) (string, bool) {
	if !strings.HasPrefix(draft, newDirPrefix) || strings.Contains(draft, "\n") {
		return "", false
	}
	return draft[len(newDirPrefix):], true
}

// listDirs walks root for directories (relative, sorted, depth
// bounded), skipping dot directories, node_modules and vendor.
func listDirs(root string) []string {
	var out []string
	_ = filepath.WalkDir(root, func(p string, d os.DirEntry, err error) error {
		if err != nil || p == root || !d.IsDir() {
			return nil
		}
		name := d.Name()
		if strings.HasPrefix(name, ".") || name == "node_modules" || name == "vendor" {
			return filepath.SkipDir
		}
		rel, err := filepath.Rel(root, p)
		if err != nil {
			return nil
		}
		out = append(out, filepath.ToSlash(rel))
		if len(out) >= atMaxFiles {
			return filepath.SkipAll
		}
		if strings.Count(rel, string(filepath.Separator)) >= newDirMaxDepth-1 {
			return filepath.SkipDir
		}
		return nil
	})
	slices.Sort(out)
	return out
}

// syncNewDir derives the dialog from the draft, like syncAt.
func (m *model) syncNewDir() {
	draft := m.input.Value()
	if m.nd.escaped && draft != m.nd.escAt {
		m.nd.escaped = false
	}
	_, ok := newDirQuery(draft)
	open := ok && m.cfg.Load().cmds != nil && !m.inspecting && !m.picking && !m.mp.open && !m.nd.escaped
	if open && !m.nd.open {
		m.nd.selected = 0
		m.ndDirs = listDirs(".")
	}
	m.nd.open = open
}

func (m *model) newDirItems() []paletteItem {
	q, _ := newDirQuery(m.input.Value())
	items := make([]paletteItem, len(m.ndDirs))
	for i, d := range m.ndDirs {
		items[i] = paletteItem{name: d}
	}
	return paletteFilter(items, q)
}

// newDirKey routes one key into the open dialog.
func (m *model) newDirKey(key string) (bool, tea.Cmd) {
	items := m.newDirItems()
	act, name := m.nd.onKey(key, items)
	switch act {
	case palMoved:
		return true, nil
	case palClose:
		m.nd.escaped = true
		m.nd.escAt = m.input.Value()
		return true, nil
	case palComplete:
		m.input.SetValue(newDirPrefix + name)
		m.input.CursorEnd()
		m.syncPalette()
		return true, nil
	case palAccept:
		m.input.Reset()
		m.syncPalette()
		return true, m.dispatch(newDirPrefix + name)
	}
	return false, nil
}

// newDirBox renders the dialog's bordered rows, nil while closed or
// on a pane too small to hold a border and one row.
func (m *model) newDirBox() []string {
	if !m.nd.open {
		return nil
	}
	w := min(m.width, 64)
	maxRows := min(palMaxRows, m.vp.Height()-2)
	if w < 8 || maxRows < 1 {
		return nil
	}
	th := m.cfg.Load().theme
	inner := w - 4
	pad := func(s string) string {
		s = ansi.Truncate(s, inner, "…")
		return "│ " + s + strings.Repeat(" ", max(inner-ansi.StringWidth(s), 0)) + " │"
	}
	out := []string{"╭" + ansi.Truncate("─ new session in "+strings.Repeat("─", w), w-2, "") + "╮"}
	items := m.newDirItems()
	if len(items) == 0 {
		out = append(out, pad(th["dim"].Render("no directory matches")))
	} else {
		sel := min(m.nd.selected, len(items)-1)
		first, rows := paletteWindow(len(items), sel, maxRows)
		for i := first; i < first+rows; i++ {
			if i == sel {
				out = append(out, pad(th["focus"].Render("▸ "+items[i].name)))
			} else {
				out = append(out, pad("  "+items[i].name))
			}
		}
	}
	return append(out, "╰"+strings.Repeat("─", w-2)+"╯")
}

// overlayCenter paints box over the middle of body, both ways.
func overlayCenter(body string, box []string, width int) string {
	bl := strings.Split(body, "\n")
	if len(box) > len(bl) {
		return body
	}
	top := (len(bl) - len(box)) / 2
	left := max((width-ansi.StringWidth(box[0]))/2, 0)
	for i, row := range box {
		l := bl[top+i]
		pre := ansi.Truncate(l, left, "")
		pre += strings.Repeat(" ", max(left-ansi.StringWidth(pre), 0))
		rest := ansi.TruncateLeft(l, left+ansi.StringWidth(row), "")
		bl[top+i] = pre + row + rest
	}
	return strings.Join(bl, "\n")
}
