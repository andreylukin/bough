package ui

// The rewind tree: double esc on an empty composer opens the session's
// WHOLE tree — this session's turns, the sessions it was forked from,
// and every fork taken off any of them — and Enter continues from any
// point in it. pi's /tree does the same over one file with parent ids;
// bough forks into a new session file, so the tree is assembled from
// the family of files: a fork's "meta" entry names the session and
// turn it came from (history.Fork), and a session's own turns are the
// ones after that point. Before this the menu only listed the current
// file, so a branch you had left was unreachable except through the
// session picker, where it looked like any other session.
//
// Picking a turn goes back to the point BEFORE that prompt, which is
// what Claude Code's menu offers and the only reading that makes the
// labels mean anything: to undo "update the readme" you pick "update
// the readme". history.Fork keeps the turn you name, so the row for a
// turn forks at the turn before it in its own file (a fork's first turn
// follows the turn it was forked at, which its file also holds); the
// root's first turn has no earlier turn, and the point before it is a
// fresh session (/new). Each branch ends in a tip row: "(current)" for
// this session does nothing, and "(continue)" on another branch
// resumes that session where it left off — that is how you switch
// branches rather than fork them.
//
// It moves the CONVERSATION only. Putting files back is /undo, one
// turn at a time, so each row says what its turn wrote rather than
// implying the code travels with it.

import (
	"cmp"
	"fmt"
	"slices"
	"strconv"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/andreylukin/bough/plugins/history"
)

// rewindRow is one row of the tree: a turn, or a branch's tip.
type rewindRow struct {
	sess   string   // the session (file id) the turn lives in
	seq    int64    // the input entry's seq; 0 on a tip row
	prev   int64    // the turn before it in the same file; 0 = none
	text   string   // the typed prompt, first line, for the row
	full   string   // the whole typed prompt, for the composer
	files  []string // what the turn wrote
	tip    bool     // the end of a branch
	active bool     // on the current session's path
	prefix string   // tree connectors, plain
}

// rewindPicker is the open menu; pick indexes rows, which end in this
// session's "(current)" tip.
type rewindPicker struct {
	open bool
	pick int
	rows []rewindRow
}

// rewindTurns folds history entries into turn rows: an "input" entry
// opens a turn and the next "done" closes it, carrying the files that
// turn wrote. prev is filled in from the turn before.
//
// A narrower fold than the one /tree uses, because this only has to
// DISPLAY turns — no checkpoints, no undo bookkeeping.
func rewindTurns(entries []history.Entry) []rewindRow {
	var rows []rewindRow
	for _, e := range entries {
		switch e.Kind {
		case "input":
			text := strings.TrimSpace(history.Prompt(e))
			if text == "" || strings.HasPrefix(text, "[background job] ") {
				continue // nobody typed that one
			}
			r := rewindRow{seq: e.Seq, text: firstLine(text), full: text}
			if n := len(rows); n > 0 {
				r.prev = rows[n-1].seq
			}
			rows = append(rows, r)
		case "done":
			if len(rows) > 0 {
				rows[len(rows)-1].files = strList(e.Data["files"])
			}
		}
	}
	return rows
}

// firstLine is text up to its first newline.
func firstLine(text string) string {
	if i := strings.IndexByte(text, '\n'); i >= 0 {
		return strings.TrimSpace(text[:i])
	}
	return text
}

// branch is one session's contribution to the tree: its own turns (the
// ones after the point it was forked at) and the forks taken off it.
type branch struct {
	id     string
	turns  []rewindRow
	forks  []*branch // children, in the order they were taken
	parent *branch
	at     int64 // the parent's turn this branch forked at
	cur    bool  // the mounted session
}

// treeNode is a turn with its children: the same branch's next turn,
// plus the forks taken at it.
type treeNode struct {
	row  rewindRow
	kids []*treeNode
}

// rewindFamily assembles the tree around the current session: its
// fork ancestors as far as their files exist, then every session
// forked from any of those, to any depth. current is the mounted
// session's entries (the freshest copy; the file may lag), infos the
// session directory. Returns the root branch.
//
// A fork whose origin file is gone is a root of its own with all its
// turns, since nothing else shows them. A fork with no turns of its
// own is left out unless it is the current session: a branch nobody
// walked down is noise.
func rewindFamily(curID string, current []history.Entry, infos []history.SessionInfo) *branch {
	byID := make(map[string]history.SessionInfo, len(infos))
	for _, s := range infos {
		byID[s.ID] = s
	}
	// Up: the chain of origins, stopping at a session that is not a
	// fork or whose origin is missing.
	rootID := curID
	seen := map[string]bool{curID: true}
	for {
		s, ok := byID[rootID]
		if !ok || s.ForkedFrom == "" || seen[s.ForkedFrom] {
			break
		}
		if _, ok := byID[s.ForkedFrom]; !ok {
			break
		}
		rootID = s.ForkedFrom
		seen[rootID] = true
	}
	// Down: forks of members, to a fixpoint. Sorted by id so a
	// UUIDv7 fork lands in the order it was taken.
	sorted := slices.Clone(infos)
	slices.SortFunc(sorted, func(a, b history.SessionInfo) int { return cmp.Compare(a.ID, b.ID) })
	branches := map[string]*branch{}
	get := func(id string) *branch {
		if b, ok := branches[id]; ok {
			return b
		}
		b := &branch{id: id, cur: id == curID}
		if id == curID {
			b.turns = rewindTurns(current)
		} else if s, ok := byID[id]; ok {
			entries, err := history.Read(s.Path)
			if err == nil {
				b.turns = rewindTurns(entries)
			}
		}
		for i := range b.turns {
			b.turns[i].sess = id
		}
		branches[id] = b
		return b
	}
	root := get(rootID)
	// A fork copies its ancestors, so the turns up to the fork point
	// belong to the parent's rows: keep the ones after it.
	own := func(b *branch, at int64) {
		b.at = at
		b.turns = slices.DeleteFunc(b.turns, func(r rewindRow) bool { return r.seq <= at })
	}
	members := map[string]*branch{rootID: root}
	for changed := true; changed; {
		changed = false
		for _, s := range sorted {
			if _, in := members[s.ID]; in || s.ForkedFrom == "" {
				continue
			}
			parent, ok := members[s.ForkedFrom]
			if !ok {
				continue
			}
			b := get(s.ID)
			own(b, s.AtSeq)
			members[s.ID] = b
			// A fork taken at a turn the parent itself copied from ITS
			// origin (a rewind past the fork point) hangs off the
			// branch that owns that turn.
			for parent.parent != nil && s.AtSeq <= parent.at {
				parent = parent.parent
			}
			b.parent = parent
			if len(b.turns) > 0 || b.cur {
				parent.forks = append(parent.forks, b)
			}
			changed = true
		}
	}
	// The current session might be a fork whose origin is not in the
	// listing (a fake, or a file just written): then it is the root.
	if _, in := members[curID]; !in {
		root = get(curID)
	}
	return root
}

// rewindTree lays a branch out as nodes: each turn's children are the
// forks taken at it (first) and the branch's own next turn (last, so
// the branch reads straight down past its side branches); the last
// turn's children are the forks at it and then the tip.
func rewindTree(b *branch) *treeNode {
	tip := &treeNode{row: rewindRow{sess: b.id, tip: true, active: b.cur}}
	if b.cur {
		tip.row.text = "(current)"
	} else {
		tip.row.text = "(continue)"
	}
	forksAt := func(seq int64) []*treeNode {
		var kids []*treeNode
		for _, f := range b.forks {
			if f.at == seq {
				kids = append(kids, rewindTree(f))
			}
		}
		return kids
	}
	next := tip
	for i := len(b.turns) - 1; i >= 0; i-- {
		n := &treeNode{row: b.turns[i]}
		n.kids = append(forksAt(b.turns[i].seq), next)
		next = n
	}
	return next
}

// markActive flags the path from the root to the current session's
// tip: every turn of the current branch and of the branches it was
// forked from, up to the fork points.
func markActive(n *treeNode) bool {
	on := n.row.tip && n.row.active
	for _, k := range n.kids {
		if markActive(k) {
			on = true
		}
	}
	n.row.active = on
	return on
}

// flatten walks the tree into rows with pi-style connectors: n's row
// gets prefix, and its descendants line up under gutter. A child among
// siblings gets ├─ (└─ for the last) and its descendants a gutter that
// keeps │ open while siblings are still to come; an only child
// continues straight down in its parent's column.
func flatten(n *treeNode, prefix, gutter string, out *[]rewindRow) {
	r := n.row
	r.prefix = prefix
	*out = append(*out, r)
	if len(n.kids) == 1 {
		flatten(n.kids[0], gutter, gutter, out)
		return
	}
	for i, k := range n.kids {
		conn, down := "├─ ", "│  "
		if i == len(n.kids)-1 {
			conn, down = "└─ ", "   "
		}
		flatten(k, gutter+conn, gutter+down, out)
	}
}

// rewindRows is the menu's rows for the current session.
func (m *model) rewindRows(cfg *uiCfg) []rewindRow {
	root := rewindFamily(m.currentID(cfg), cfg.hist.Entries(), m.listSessions())
	tree := rewindTree(root)
	markActive(tree)
	var rows []rewindRow
	flatten(tree, "", "", &rows)
	return rows
}

// openRewind shows the menu with the cursor on "(current)", the way
// Claude Code opens it: at the present, walking back from there.
func (m *model) openRewind() bool {
	cfg := m.cfg.Load()
	if cfg.hist == nil {
		m.flash = "no history in this session: nothing to rewind to"
		return false
	}
	rows := m.rewindRows(cfg)
	turns := 0
	for _, r := range rows {
		if !r.tip {
			turns++
		}
	}
	if turns == 0 {
		m.flash = "no turns yet: nothing to rewind to"
		return false
	}
	pick := len(rows) - 1
	for i, r := range rows {
		if r.tip && r.text == "(current)" {
			pick = i
		}
	}
	m.rw = rewindPicker{open: true, rows: rows, pick: pick}
	return true
}

// handleRewindKey drives the menu. The quit binding backs out rather
// than quitting, like the other pickers.
func (m model) handleRewindKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	key := msg.String()
	if m.cfg.Load().action[key] == "quit" {
		key = "esc"
	}
	switch key {
	case "up", "ctrl+p":
		if m.rw.pick > 0 {
			m.rw.pick--
		}
	case "down", "ctrl+n":
		if m.rw.pick < len(m.rw.rows)-1 {
			m.rw.pick++
		}
	case "enter":
		row := m.rw.rows[m.rw.pick]
		m.rw = rewindPicker{}
		cur := m.currentID(m.cfg.Load())
		if row.tip {
			if row.sess == cur {
				return m, nil // "(current)": nothing to do
			}
			// "(continue)": switch to that branch where it left off.
			return m, m.dispatch("/sessions " + row.sess)
		}
		var cmd tea.Cmd
		switch {
		case row.prev == 0:
			// Before the first prompt is a session with no turns.
			cmd = m.dispatch("/new")
		case row.sess == cur:
			cmd = m.dispatch("/tree " + strconv.FormatInt(row.prev, 10))
		default:
			cmd = m.dispatch("/tree " + strconv.FormatInt(row.prev, 10) + " " + row.sess)
		}
		// The prompt you rewound past goes back in the composer. Going
		// back to before a turn is how you re-ask it, and retyping it
		// from the transcript is the whole of the work you just undid.
		// After the dispatch, not before: it resumes the forked session
		// in place, and that path resets the composer.
		m.setDraft(row.full)
		return m, cmd
	case "esc":
		m.rw = rewindPicker{}
	}
	return m, nil
}

// rewindView renders the tree: oldest turn first, forks as side
// branches, the current path marked, the cursor row in the focus
// style, and a count of what is scrolled off either end.
func (m *model) rewindView(cfg *uiCfg) string {
	th := cfg.theme
	lines := []string{
		th["accent"].Render("bough") + " " + th["dim"].Render("· rewind"),
		"",
		th["dim"].Render("Go back to the conversation as it was before…"),
		"",
	}
	rows := m.rw.rows
	room := max(m.height-9, 3)
	first := 0
	if len(rows) > room && m.rw.pick > room-1 {
		first = min(m.rw.pick-room+1, len(rows)-room)
	}
	if first > 0 {
		lines = append(lines, th["dim"].Render(fmt.Sprintf("  ↑ %d more above", first)))
	}
	end := min(first+room, len(rows))
	for i := first; i < end; i++ {
		lines = append(lines, m.rewindRowLine(i, th))
	}
	if end < len(rows) {
		lines = append(lines, th["dim"].Render(fmt.Sprintf("  ↓ %d more below", len(rows)-end)))
	}
	lines = append(lines, "", th["dim"].Render("enter goes back to before this prompt · (continue) switches branch · esc cancels"))
	return strings.Join(lines, "\n")
}

// rewindRowLine renders one row: connectors, the path marker, the
// prompt, then what its turn wrote.
func (m *model) rewindRowLine(i int, th theme) string {
	r := m.rw.rows[i]
	style, marker := th["dim"], "  "
	if r.active && !r.tip {
		style = th["result"]
	}
	if i == m.rw.pick {
		style, marker = th["focus"], th["accent"].Render("❯ ")
	}
	// The path marker only earns its column once there is more than
	// one path.
	dot := ""
	if m.rewindBranched() {
		dot = "  "
		if r.active {
			dot = th["accent"].Render("• ")
		}
	}
	note := ""
	if !r.tip {
		note = "  no files written"
		switch n := len(r.files); {
		case n == 1:
			note = "  wrote " + r.files[0]
		case n > 1:
			note = fmt.Sprintf("  wrote %d files", n)
		}
	}
	text := r.text
	w := m.width - 4 - len([]rune(r.prefix)) - len([]rune(note)) - len([]rune(dot))
	if w > 10 && len([]rune(text)) > w {
		text = string([]rune(text)[:w-1]) + "…"
	}
	return marker + th["dim"].Render(r.prefix) + dot + style.Render(text) + th["dim"].Render(note)
}

// rewindBranched reports whether the tree has more than one path.
func (m *model) rewindBranched() bool {
	for _, r := range m.rw.rows {
		if !r.active {
			return true
		}
	}
	return false
}
