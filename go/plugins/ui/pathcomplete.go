package ui

// Tab path completion, shell-style: with no palette or "@" picker
// open, Tab on a path-like word before the cursor (it has a "/",
// starts with "./" or "~", or is the prefix of something in cwd)
// completes it against the filesystem — the common prefix first, then
// each Tab cycles the candidates; directories complete with a trailing
// "/". Anywhere else Tab keeps its keymap meaning (block focus).

import (
	"os"
	"path/filepath"
	"slices"
	"strings"
	"unicode"

	tea "charm.land/bubbletea/v2"
)

// tabState is the cycling cursor: cands are the last completion's
// candidates (as they sit in the draft, "@" prefix and escapes
// included), idx the one shown (-1 while the common prefix is
// showing), value/line/col the draft and cursor as that completion
// left them — any other draft or cursor starts afresh.
type tabState struct {
	cands     []string
	idx       int
	value     string
	line, col int
}

// wordBeforeCursor is the run of non-blank runes ending at the cursor
// on its line; a backslash-escaped blank ("my\ dir") is part of it.
func (m *model) wordBeforeCursor() string {
	line := []rune(strings.Split(m.input.Value(), "\n")[m.input.Line()])
	col := min(m.input.Column(), len(line))
	start := col
	for start > 0 && (!unicode.IsSpace(line[start-1]) || (start > 1 && line[start-2] == '\\')) {
		start--
	}
	return string(line[start:col])
}

// escapePath / unescapePath shell-escape blanks in a name so a
// completed "my dir/" stays one word for the next Tab.
func escapePath(s string) string   { return strings.ReplaceAll(s, " ", "\\ ") }
func unescapePath(s string) string { return strings.ReplaceAll(s, "\\ ", " ") }

// pathCandidates lists the filesystem entries word is a prefix of
// ("~" expanded, dotfiles only when the typed name starts with ".");
// directories carry a trailing "/". Each candidate keeps the user's
// spelling of the directory part.
func pathCandidates(word string) []string {
	if word == "~" {
		return []string{"~/"}
	}
	pat := word
	if strings.HasPrefix(pat, "~/") {
		home, _ := os.UserHomeDir()
		pat = home + pat[1:]
	}
	dir, base := filepath.Split(pat)
	if dir == "" {
		dir = "."
	}
	ents, err := os.ReadDir(dir)
	if err != nil {
		return nil
	}
	var out []string
	for _, e := range ents {
		name := e.Name()
		if !strings.HasPrefix(name, base) || (base == "" && strings.HasPrefix(name, ".")) {
			continue
		}
		isDir := e.IsDir()
		if e.Type()&os.ModeSymlink != 0 {
			st, err := os.Stat(filepath.Join(dir, name))
			isDir = err == nil && st.IsDir()
		}
		if isDir {
			name += "/"
		}
		out = append(out, strings.TrimSuffix(word, base)+name)
	}
	slices.Sort(out)
	return out
}

// pathLike: a word worth completing as a path — it has a "/", starts
// with "." or "~", or already names something in cwd. A URL is not.
func pathLike(word string, cands []string) bool {
	if strings.Contains(word, "://") {
		return false
	}
	return strings.Contains(word, "/") || strings.HasPrefix(word, ".") || strings.HasPrefix(word, "~") || len(cands) > 0
}

// commonPrefix of the candidates, in runes.
func commonPrefix(cands []string) string {
	p := []rune(cands[0])
	for _, c := range cands[1:] {
		r := []rune(c)
		n := 0
		for n < len(p) && n < len(r) && p[n] == r[n] {
			n++
		}
		p = p[:n]
	}
	return string(p)
}

// tabComplete handles Tab in the composer. Reports whether it took
// the key: false leaves Tab to the keymap (block focus).
func (m *model) tabComplete() bool {
	if len(m.tab.cands) > 0 && m.input.Value() == m.tab.value && m.input.Line() == m.tab.line && m.input.Column() == m.tab.col {
		old := commonPrefix(m.tab.cands)
		if m.tab.idx >= 0 {
			old = m.tab.cands[m.tab.idx]
		}
		m.tab.idx = (m.tab.idx + 1) % len(m.tab.cands)
		m.replaceWord(old, m.tab.cands[m.tab.idx])
		m.tab.value, m.tab.line, m.tab.col = m.input.Value(), m.input.Line(), m.input.Column()
		return true
	}
	word := m.wordBeforeCursor()
	if word == "" {
		return false
	}
	at := ""
	if word[0] == '@' {
		at, word = "@", word[1:]
	}
	cands := pathCandidates(unescapePath(word))
	if !pathLike(word, cands) {
		return false
	}
	if len(cands) == 0 {
		m.flash = "no completion for " + word
		return true
	}
	for i := range cands {
		cands[i] = at + escapePath(cands[i])
	}
	prefix := commonPrefix(cands)
	if len(cands) == 1 {
		// A lone match completes outright; the next Tab starts afresh
		// (a directory then descends into its contents).
		m.replaceWord(at+word, prefix)
		m.tab = tabState{}
		return true
	}
	m.tab = tabState{cands: cands, idx: -1}
	if len([]rune(prefix)) > len([]rune(at+word)) {
		m.replaceWord(at+word, prefix)
	} else {
		m.tab.idx = 0
		m.replaceWord(at+word, cands[0])
	}
	m.tab.value, m.tab.line, m.tab.col = m.input.Value(), m.input.Line(), m.input.Column()
	return true
}

// replaceWord swaps the word before the cursor (old) for with, leaving
// the cursor after it and the rest of the draft in place.
func (m *model) replaceWord(old, with string) {
	for range []rune(old) {
		m.input, _ = m.input.Update(tea.KeyPressMsg{Code: tea.KeyBackspace})
	}
	m.input.InsertString(with)
	m.syncPalette()
	m.layoutComposer()
}
