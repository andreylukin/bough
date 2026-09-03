package ui

// "@" file references in the composer: a word starting with "@" opens
// a picker over the project's files (fuzzy, like the "/" palette);
// Tab or Enter completes the word to "@path " in place. On submit the
// loop expands each "@path" that names a real file into an attachment
// (see loop.ExpandAt), so the model sees the file, not just its name.

import (
	"os"
	"path/filepath"
	"slices"
	"strings"

	tea "charm.land/bubbletea/v2"
)

// atMaxFiles caps the walk so a huge tree cannot stall the composer.
const atMaxFiles = 5000

// atStart is the index of the "@" opening the last word of the draft
// (start of text or after whitespace), else -1. An "@" inside a word
// ("a@b.com") never opens the picker.
func atStart(draft string) int {
	i := strings.LastIndexAny(draft, " \t\n") + 1
	if i < len(draft) && draft[i] == '@' {
		return i
	}
	return -1
}

// listFiles walks root (relative paths, sorted), skipping dot
// directories, node_modules and vendor trees.
func listFiles(root string) []string {
	var out []string
	_ = filepath.WalkDir(root, func(p string, d os.DirEntry, err error) error {
		if err != nil {
			return nil
		}
		if p == root {
			return nil
		}
		name := d.Name()
		if d.IsDir() {
			if strings.HasPrefix(name, ".") || name == "node_modules" || name == "vendor" {
				return filepath.SkipDir
			}
			return nil
		}
		if strings.HasPrefix(name, ".") {
			return nil
		}
		rel, err := filepath.Rel(root, p)
		if err != nil {
			return nil
		}
		out = append(out, filepath.ToSlash(rel))
		if len(out) >= atMaxFiles {
			return filepath.SkipAll
		}
		return nil
	})
	slices.Sort(out)
	return out
}

// syncAt derives the picker from the draft, like syncPalette: it opens
// on a word-initial "@" (never while the "/" palette owns the line),
// closes when that word goes away, and stays closed after Esc until
// the draft changes. The file list is read when the picker opens.
func (m *model) syncAt() {
	draft := m.input.Value()
	if m.at.escaped && draft != m.at.escAt {
		m.at.escaped = false
	}
	open := !m.pal.open && !m.inspecting && !m.picking && !m.mp.open && atStart(draft) >= 0 && !m.at.escaped
	if open && !m.at.open {
		m.at.selected = 0
		m.atFiles = listFiles(".")
	}
	m.at.open = open
}

func (m *model) atQuery() string {
	draft := m.input.Value()
	if i := atStart(draft); i >= 0 {
		return draft[i+1:]
	}
	return ""
}

func (m *model) atItems() []paletteItem {
	items := make([]paletteItem, len(m.atFiles))
	for i, f := range m.atFiles {
		items[i] = paletteItem{name: f, prefix: "@"}
	}
	return items
}

// atRows renders the open picker's overlay lines.
func (m *model) atRows() []string {
	if !m.at.open {
		return nil
	}
	items := paletteFilter(m.atItems(), m.atQuery())
	if len(items) == 0 {
		return nil
	}
	maxRows := palMaxRows
	if h := m.vp.Height(); h < maxRows {
		maxRows = h
	}
	return paletteLines(items, m.at.selected, m.width, maxRows, m.cfg.Load().theme)
}

// atKey routes one key into the open picker. Tab and Enter both
// complete in place: an "@" word is text, there is nothing to run.
func (m *model) atKey(key string) (bool, tea.Cmd) {
	items := paletteFilter(m.atItems(), m.atQuery())
	act, name := m.at.onKey(key, items)
	switch act {
	case palMoved:
		return true, nil
	case palClose:
		m.at.escaped = true
		m.at.escAt = m.input.Value()
		return true, nil
	case palComplete, palAccept:
		draft := m.input.Value()
		i := atStart(draft)
		if i < 0 {
			return false, nil
		}
		m.input.SetValue(draft[:i] + "@" + name + " ")
		m.input.CursorEnd()
		m.syncPalette()
		m.layoutComposer() // the completion can add a wrapped row
		return true, nil
	}
	return false, nil
}
