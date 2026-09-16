package ui

// Session resume: transcript replay from the "history" service, and
// the session picker — pre-chat at launch, driven by the launcher's
// session seam ("sessions" + "session-picker" + "session-choose", see
// uiCfg), and mid-session from /sessions or a status-bar click, where
// the list is re-read from the history directory.

import (
	"cmp"
	"github.com/charmbracelet/x/ansi"
	"time"

	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"

	"github.com/andreylukin/bough/plugins/history"
)

// pickerTitleWidth caps a session title in the picker, Claude-style.
const pickerTitleWidth = 60

// sessList is the mid-session picker's own list (nil = the launch
// picker, which reads cfg.sessions).
type sessList = []history.SessionInfo

// sessionID is the id of a history file path (base name sans .jsonl).
func sessionID(path string) string {
	return strings.TrimSuffix(filepath.Base(path), ".jsonl")
}

// replay synthesizes the transcript blocks for the history service's
// existing entries, exactly as the live session that wrote them did:
// input entries become the ❯ user line, everything else goes through
// addEvent so collapse defaults (and code de-dup) apply. A fresh
// session (no history, or no entries beyond the meta one) shows the
// welcome text instead; a model that already has blocks never replays
// (no double-render). A resumed transcript ends with a system row
// naming the session, its size and the last prompt, and lands on it.
func (m *model) replay() {
	cfg := m.cfg.Load()
	m.sessID = m.currentID(cfg)
	if len(m.blocks) > 0 {
		return
	}
	if cfg.hist == nil {
		m.welcome = true // fresh (no history service at all)
		m.noteLaunch(cfg)
		m.refresh()
		return
	}
	entries := cfg.hist.Entries()
	open := false // an input with no done/cancelled after it yet
	for _, e := range entries {
		text, _ := e.Data["text"].(string)
		switch e.Kind {
		case "input":
			open = true
		case "done", "cancelled":
			open = false
		}
		if e.Kind == "cancelled" && e.Data["interrupted"] == true {
			open = true // closed on resume after a crash: mark it, not "stopped by you"
			continue
		}
		// Typed job entries are serve's bookkeeping; the text job note
		// for the same event renders.
		if _, typed := e.Data["event"].(string); typed && e.Kind == "job" {
			continue
		}
		switch e.Kind {
		case "meta", "origin", "undo", "hook", "turn-summary", "notice", "notice-delivered":
			// session bookkeeping (cwd, a /undo's revert record —
			// its system row follows; a hook fire, the control room's
			// ledger — any notice it carried was recorded as its own
			// system row), nothing to render. A live session never
			// draws hook entries, so a resumed one must not either.
		case "input":
			// What was TYPED, not the message that was sent: an
			// injected skill's whole SKILL.md is appended to the
			// latter, and replaying showed it as the prompt — which
			// then went into the composer on Up.
			steer, _ := e.Data["steer"].(bool)
			m.blocks = append(m.blocks, block{id: m.nextID, kind: "user", text: history.Prompt(e), steer: steer})
			m.nextID++
		case "ask":
			q, _ := e.Data["question"].(string)
			id, _ := e.Data["id"].(string)
			secret, _ := e.Data["secret"].(bool)
			m.addEvent(Event{Kind: "ask", Text: q, ID: id, Options: strList(e.Data["options"]), Secret: secret})
		case "ask/answer":
			id, _ := e.Data["id"].(string)
			for i := range m.blocks {
				if b := &m.blocks[i]; b.kind == "ask" && b.askID == id {
					b.answered, b.answer = true, text
				}
			}
			if m.pendingAsk == id {
				m.clearPendingAsk()
			}
		default:
			m.addEvent(Event{Kind: e.Kind, Text: text, Data: e.Data})
		}
	}
	// The cache window counts from the last turn on file, not from the
	// replay (whose done events stamped "now" on the way through).
	m.lastRequest = time.Time{}
	for _, e := range entries {
		if e.Kind == "done" {
			m.lastRequest = e.At
		}
	}
	// The pinned list comes from the todo service: its events only
	// fire on a change, and the todo/* entries replay as no block. An
	// empty list clears it: a /sessions switch must not keep the last
	// session's panel.
	if cfg.todo != nil {
		m.todoText = ""
		if len(cfg.todo.List()) > 0 {
			m.todoText = cfg.todo.Render()
		}
	}
	// A turn the process died in (SIGKILL, crash) left its input on
	// disk with no done/cancelled entry: say so, or it reads as a
	// prompt still waiting for its answer.
	if open {
		m.blocks = append(m.blocks, block{id: m.nextID, kind: "system", text: "■ interrupted — bough exited before this turn finished"})
		m.nextID++
	}
	// Background jobs live in the process that started them: one with
	// no finished notice on file died with that process (or outlives
	// it as an untracked orphan). Say so, or it reads as still running.
	for _, n := range deadJobs(entries) {
		m.blocks = append(m.blocks, block{id: m.nextID, kind: "system", text: fmt.Sprintf("■ job %d ended with the previous bough process — no longer running", n)})
		m.nextID++
	}
	// A subagent card with no sub:done on file died with the process
	// that ran it: close it as cancelled, or it spins forever.
	for i := range m.blocks {
		if b := &m.blocks[i]; b.kind == "spawn" && b.sub != nil && b.sub.status == "running" {
			b.sub.status = "cancelled"
		}
	}
	m.expireAsks()                 // an ask with no answer entry replays as expired
	m.running = false              // a replayed transcript is never mid-turn
	m.welcome = len(m.blocks) == 0 // fresh session (0 entries): orient
	if len(m.blocks) > 0 {
		m.blocks = append(m.blocks, block{id: m.nextID, kind: "system", text: resumedLine(cfg.hist)})
		m.nextID++
	}
	m.noteLaunch(cfg)
	m.refresh()
	m.vp.GotoBottom()
}

// noteLaunch appends the launcher's notice (a stale dev binary) as an
// error row: it must be read, and a fresh session keeps its welcome
// text above it.
func (m *model) noteLaunch(cfg *uiCfg) {
	if cfg.info != "" {
		m.blocks = append(m.blocks, block{id: m.nextID, kind: "system", text: "ℹ " + cfg.info})
		m.nextID++
		m.welcome = false
	}
	if cfg.notice == "" {
		return
	}
	m.blocks = append(m.blocks, block{id: m.nextID, kind: "error", text: cfg.notice})
	m.nextID++
	m.welcome = false
}

// resumedLine is the one-line system row a resumed transcript ends
// with: "resumed <id> · <n> entries · last: <first line of last prompt>".
func resumedLine(h historyView) string {
	entries := h.Entries()
	s := fmt.Sprintf("resumed %s · %d entries", sessionID(h.Path()), len(entries))
	if last := history.LastPrompt(entries); last != "" {
		s += " · last: " + truncateCols(last, pickerTitleWidth)
	}
	return s
}

// openPicker shows the picker mid-session: the list is re-read from
// the history directory, the cursor on the current session.
func (m *model) openPicker() {
	m.picking = true
	m.pickQuery = ""
	m.pickAll = false
	m.sessRows = m.listSessions()
	m.pick = m.currentRow(m.cfg.Load())
	m.syncPalette()
}

// currentRow is the mounted session's index in the picker rows, 0 when
// it is not listed.
func (m *model) currentRow(cfg *uiCfg) int {
	cur := m.currentID(cfg)
	for i, r := range m.pickerRows(cfg) {
		if r.ID == cur {
			return i
		}
	}
	return 0
}

// listSessions reads the session directory next to the current
// history file, this directory's sessions first. Never nil: a non-nil
// list is what marks the picker as mid-session (see leavePicker).
func (m *model) listSessions() sessList {
	rows := sessList{}
	h := m.cfg.Load().hist
	if h == nil {
		return rows
	}
	infos, err := history.List(filepath.Dir(h.Path()))
	if err != nil {
		return rows
	}
	cwd, _ := os.Getwd()
	return append(rows, history.PreferCwd(infos, cwd)...)
}

// sessRow is one picker row: a session, the tree connectors that
// place it under the session it was forked from or spawned by, and
// the transcript line a query matched (content hits only).
type sessRow struct {
	history.SessionInfo
	prefix  string
	snippet string
}

// pickerRows is the list the picker shows — its own mid-session list,
// else the launcher-provided one — most recently active first, scoped
// to this project unless tab widened it (pickerScope), laid out as a
// tree (sessionTree), narrowed to titles or transcripts containing the
// typed query (case-insensitive); a transcript hit carries its line.
func (m *model) pickerRows(cfg *uiCfg) []sessRow {
	infos := cfg.sessions
	if m.sessRows != nil {
		infos = m.sessRows
	}
	infos = slices.Clone(infos)
	slices.SortStableFunc(infos, func(a, b history.SessionInfo) int { return b.ModTime.Compare(a.ModTime) })
	m.pickWide = m.pickAll
	if !m.pickAll {
		infos, m.pickWide = pickerScope(infos, m.currentID(cfg))
	}
	rows := sessionTree(infos)
	if m.pickQuery == "" {
		return rows
	}
	q := strings.ToLower(m.pickQuery)
	corpus, raw := m.pickerCorpus(infos)
	rows = slices.DeleteFunc(rows, func(r sessRow) bool {
		if strings.Contains(strings.ToLower(r.Title), q) {
			return false
		}
		return !strings.Contains(corpus[r.ID], q)
	})
	for i := range rows {
		if !strings.Contains(strings.ToLower(rows[i].Title), q) {
			rows[i].snippet = matchLine(raw[rows[i].ID], q)
		}
	}
	return rows
}

// pickerScope keeps the sessions of this project (the git repository
// holding the working directory, else the directory itself) and drops
// background runs nobody sat in front of (wiki ingests and the like;
// a spawned agent stays, nested under its parent). The current session
// and rows with no recorded directory always stay. A scope that would
// leave nothing but the current session shows everything instead, and
// reports it widened.
func pickerScope(infos []history.SessionInfo, cur string) ([]history.SessionInfo, bool) {
	cwd, _ := os.Getwd()
	repo := repoRoot(cwd)
	kept := slices.DeleteFunc(slices.Clone(infos), func(s history.SessionInfo) bool {
		if s.ID == cur {
			return false
		}
		if s.Background && s.SpawnedBy == "" {
			return true
		}
		switch {
		case s.Cwd == "" && s.Repo == "":
			return false
		case s.Cwd == cwd:
			return false
		case repo != "" && (s.Repo == repo || s.Cwd == repo || strings.HasPrefix(s.Cwd, repo+string(filepath.Separator))):
			return false
		}
		return true
	})
	if !slices.ContainsFunc(kept, func(s history.SessionInfo) bool { return s.ID != cur }) {
		return infos, true
	}
	return kept, false
}

// repoRoot is the nearest directory at or above dir holding .git, ""
// outside a repository.
func repoRoot(dir string) string {
	for d := dir; d != ""; {
		if _, err := os.Stat(filepath.Join(d, ".git")); err == nil {
			return d
		}
		up := filepath.Dir(d)
		if up == d {
			break
		}
		d = up
	}
	return ""
}

// matchLine is the first line of the original-case transcript holding
// the lowercase q, cut to start near the match. The cut counts runes
// (a rune-for-rune lowercasing keeps counts aligned), so it never
// splits a multi-byte character.
func matchLine(corpus, q string) string {
	for line := range strings.Lines(corpus) {
		low := strings.Map(unicode.ToLower, line)
		i := strings.Index(low, q)
		if i < 0 {
			continue
		}
		if lead := utf8.RuneCountInString(low[:i]); lead > 30 {
			line = "…" + strings.TrimLeft(string([]rune(line)[lead-20:]), " ")
		}
		return strings.TrimSpace(line)
	}
	return ""
}

// pickerCorpus is every listed session's transcript, lowercased (and
// as written, for snippets) and keyed by id, read once per row set and reused for the whole typing
// run.
//
// A title is the session's first prompt, so filtering on titles alone
// answers only "how did this session open" — and what you remember is
// usually something said in the middle of it. Reading the files is
// affordable (a year of sessions is tens of megabytes of JSONL) and
// happens on the first keystroke rather than when the picker opens, so
// opening it stays instant.
func (m *model) pickerCorpus(infos []history.SessionInfo) (low, raw map[string]string) {
	key := corpusKey(infos)
	if m.pickCorpus != nil && m.pickCorpusFor == key {
		return m.pickCorpus, m.pickCorpusRaw
	}
	corpus := make(map[string]string, len(infos))
	orig := make(map[string]string, len(infos))
	for _, in := range infos {
		if in.Path == "" {
			continue // no file to read (a synthesized row)
		}
		entries, err := history.Read(in.Path)
		if err != nil {
			continue // unreadable is not matchable, and never fatal
		}
		var b strings.Builder
		for _, e := range entries {
			b.WriteString(history.EntryText(e))
			b.WriteByte('\n')
		}
		orig[in.ID] = b.String()
		corpus[in.ID] = strings.ToLower(orig[in.ID])
	}
	m.pickCorpus, m.pickCorpusRaw, m.pickCorpusFor = corpus, orig, key
	return corpus, orig
}

// corpusKey identifies a row set cheaply, so a mid-session re-read (or
// a fork landing) rebuilds instead of filtering stale text.
func corpusKey(infos []history.SessionInfo) string {
	var b strings.Builder
	for _, in := range infos {
		b.WriteString(in.ID)
		b.WriteByte(' ')
		b.WriteString(in.ModTime.Format(time.RFC3339Nano))
		b.WriteByte(';')
	}
	return b.String()
}

// sessionTree nests each fork under the session it was forked from
// (SessionInfo.ForkedFrom), and each background agent under the
// session that spawned it (SpawnedBy), pi's /tree over bough's one-file-per-
// branch sessions: a session's forks hang under it with ├─ └─ │
// connectors, in the order they were taken (ids are UUIDv7s, so id
// order), to any depth. A family lands where its first member did in
// the incoming order, so a recently active branch keeps its old root
// near the top and this directory's sessions still come first. A fork
// whose origin is not listed is a root like any other.
func sessionTree(infos []history.SessionInfo) []sessRow {
	kids := map[string][]history.SessionInfo{}
	byID := map[string]bool{}
	for _, s := range infos {
		byID[s.ID] = true
	}
	for _, s := range infos {
		if p := parentOf(s); p != "" && byID[p] && p != s.ID {
			kids[p] = append(kids[p], s)
		}
	}
	for id := range kids {
		slices.SortFunc(kids[id], func(a, b history.SessionInfo) int { return cmp.Compare(a.ID, b.ID) })
	}
	rootOf := func(s history.SessionInfo) string {
		seen := map[string]bool{}
		for p := parentOf(s); p != "" && byID[p] && !seen[s.ID]; p = parentOf(s) {
			seen[s.ID] = true
			for _, t := range infos {
				if t.ID == p {
					s = t
					break
				}
			}
		}
		return s.ID
	}
	var rows []sessRow
	var walk func(s history.SessionInfo, prefix, gutter string)
	walk = func(s history.SessionInfo, prefix, gutter string) {
		rows = append(rows, sessRow{SessionInfo: s, prefix: prefix})
		for i, k := range kids[s.ID] {
			conn, down := "├─ ", "│  "
			if i == len(kids[s.ID])-1 {
				conn, down = "└─ ", "   "
			}
			walk(k, gutter+conn, gutter+down)
		}
	}
	placed := map[string]bool{}
	for _, s := range infos {
		root := rootOf(s)
		if placed[root] {
			continue
		}
		placed[root] = true
		for _, r := range infos {
			if r.ID == root {
				walk(r, "", "")
				break
			}
		}
	}
	return rows
}

// parentOf is the session a row hangs under: its fork origin, else the
// session that spawned it.
func parentOf(s history.SessionInfo) string {
	return cmp.Or(s.ForkedFrom, s.SpawnedBy)
}

// currentID is the mounted session's id ("" without history).
func (m *model) currentID(cfg *uiCfg) string {
	if cfg.hist == nil {
		return ""
	}
	return sessionID(cfg.hist.Path())
}

// handlePickerKey drives the session picker: typed text filters the
// rows by title (backspace deletes), up/down move, enter
// resumes the selected session, esc starts a fresh one (at launch) or
// goes back to the chat (mid-session). The quit binding still works.
// Without a "session-choose" callback the list is read-only (the view
// says so loudly): enter does nothing and esc falls through to a
// fresh chat without choosing.
func (m model) handlePickerKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	cfg := m.cfg.Load()
	rows := m.pickerRows(cfg)
	key := msg.String()
	if cfg.action[key] == "quit" {
		if m.sessRows == nil {
			return m, tea.Quit // the launch chooser: nothing to go back to
		}
		return m.leavePicker(""), nil // in-session: back, like esc — never a one-press quit
	}
	switch key {
	case "up":
		if m.pick > 0 {
			m.pick--
		}
	case "down":
		if m.pick < len(rows)-1 {
			m.pick++
		}
	case "pgup":
		m.pick = max(m.pick-pickerFit(rows, m.pick, m.pickerPage(cfg)), 0)
	case "pgdown":
		m.pick = max(min(m.pick+pickerFitFrom(rows, m.pick, m.pickerPage(cfg)), len(rows)-1), 0)
	case "enter":
		if cfg.choose == nil || len(rows) == 0 {
			return m, nil
		}
		if m.pick >= len(rows) { // sessions swapped under us (hot reload)
			m.pick = 0
		}
		next := m.leavePicker(rows[m.pick].ID)
		return next, next.cacheTick() // the resumed session's cache window
	case "esc":
		return m.leavePicker(""), nil
	case "tab":
		m.pickAll = !m.pickAll
		m.pick = m.currentRow(cfg)
	case "backspace":
		if r := []rune(m.pickQuery); len(r) > 0 {
			m.pickQuery = string(r[:len(r)-1])
			m.pick = 0
		}
	default:
		if msg.Text != "" {
			m.pickQuery += msg.Text
			m.pick = 0
		}
	}
	return m, nil
}

// leavePicker enters the chat view. At launch (no own list) the choose
// seam is invoked once (id "" = fresh session) and the now-current
// history replays. Mid-session, id "" is "back" (nothing changes) and
// a different session swaps history through the seam and replays from
// scratch; the current session's id is a no-op resume.
func (m model) leavePicker(id string) model {
	cfg := m.cfg.Load()
	launch := m.sessRows == nil
	m.picking = false
	m.pickQuery = ""
	m.sessRows = nil
	if !launch && (id == "" || id == m.currentID(cfg)) {
		return m
	}
	if !launch && m.running {
		// Swapping history under a live turn would render its deltas
		// into the resumed session and lose its own cancelled entry
		// (cancel is async, so it cannot be cancelled-then-swapped).
		m.flash = "a turn is running — esc cancels it, then resume"
		return m
	}
	if cfg.choose != nil {
		cfg.choose(id)
	}
	if id != "" {
		m.blocks = nil
		m.focusID = -1
		m.welcome = false
		m.title = "" // the replay sets the new session's, if it has one
	}
	m.replay()
	return m
}

// resumeID is "/sessions <id>": resume that session directly, with an
// error block for an unknown id.
func (m *model) resumeID(id string) {
	found := false
	for _, s := range m.listSessions() {
		if s.ID == id {
			found = true
		}
	}
	if !found {
		m.blocks = append(m.blocks, block{id: m.nextID, kind: "error",
			text: fmt.Sprintf("no session %q (/sessions lists them)", id)})
		m.nextID++
		m.refresh()
		m.vp.GotoBottom()
		return
	}
	m.sessRows = sessList{} // mid-session semantics for leavePicker
	*m = m.leavePicker(id)
}

// pickerPage is how many lines fit between the picker's header and its
// hint row (at least one).
func (m *model) pickerPage(cfg *uiCfg) int {
	header := 2 + 2 // title and blank, then the "N sessions" count and blank
	if m.pickQuery != "" {
		header += 2
	}
	if cfg.choose == nil {
		header += 2
	}
	return max(m.height-1-header, 1)
}

// pickerFit is how many rows ending at end fit in budget lines, a row
// with a snippet taking two (at least one).
func pickerFit(rows []sessRow, end, budget int) int {
	n := 0
	for i := min(end, len(rows)-1); i >= 0; i-- {
		budget--
		if rows[i].snippet != "" {
			budget--
		}
		if budget < 0 {
			break
		}
		n++
	}
	return max(n, 1)
}

// pickerFitFrom is how many rows starting at start fit in budget
// lines, counted like pickerFit.
func pickerFitFrom(rows []sessRow, start, budget int) int {
	n := 0
	for i := max(start, 0); i < len(rows); i++ {
		budget--
		if rows[i].snippet != "" {
			budget--
		}
		if budget < 0 {
			break
		}
		n++
	}
	return max(n, 1)
}

// pickerView renders the full-screen session list: this directory's
// sessions first, one row per session (local time, entry count,
// first-input title, working directory), the current session marked,
// the selected row in the focus style, key hints pinned to the bottom.
func (m *model) pickerView(cfg *uiCfg) string {
	th := cfg.theme
	rows := m.pickerRows(cfg)
	if m.pick >= len(rows) { // sessions swapped under us
		m.pick = 0
	}
	lines := []string{
		th["accent"].Render("bough") + " " + th["dim"].Render("· resume a session"),
		"",
	}
	if m.pickQuery != "" {
		lines = append(lines, th["accent"].Render("search: ")+m.pickQuery, "")
	}
	if cfg.choose == nil {
		lines = append(lines, th["error"].Render("✗ session-choose service missing — list is read-only"), "")
	}
	if len(rows) == 0 && m.pickQuery != "" {
		lines = append(lines, th["dim"].Render("  (no matching sessions)"))
	} else if len(rows) == 0 {
		lines = append(lines, th["dim"].Render("  (no sessions)"))
	}
	if len(rows) > 0 {
		scope := "this project"
		if m.pickWide {
			scope = "all projects"
		}
		lines = append(lines, th["dim"].Render("  "+plural(len(rows), "session")+" · "+scope), "")
	}
	cur := m.currentID(cfg)
	cwd, _ := os.Getwd()
	home, _ := os.UserHomeDir()
	// Scroll so the selected row stays on screen above the hint row.
	start := max(m.pick-pickerFit(rows, m.pick, m.pickerPage(cfg))+1, 0)
	for i, s := range rows[start:] {
		i += start
		marker, st := "  ", th["result"]
		if i == m.pick {
			marker, st = "▸ ", th["focus"]
		}
		title := strings.TrimSpace(s.Title)
		if title == "" {
			title = "Untitled session · " + shortID(s.ID)
		}
		row := fmt.Sprintf("%s%s%s  %3d %-7s  %s",
			marker, s.prefix, s.ModTime.Local().Format("2006-01-02 15:04"), s.Entries, pluralWord(s.Entries, "entry", "entries"), truncateCols(title, pickerTitleWidth))
		if s.SpawnedBy != "" && s.ForkedFrom == "" {
			row += " [subagent]"
		}
		if s.ID == cur {
			row += " (current)"
		}
		row += "  " + elideMiddle(shortDir(s.Cwd, cwd, home), 40)
		if m.width > 2 {
			row = ansi.Truncate(row, m.width-1, "…")
		}
		lines = append(lines, st.Render(row))
		if s.snippet != "" {
			lines = append(lines, th["dim"].Render(ansi.Truncate("      "+s.prefix+"“"+s.snippet+"”", max(m.width-1, 1), "…")))
		}
	}
	hint := "type to search · ↑/↓ select · tab all/project · enter resume · esc new session"
	if m.sessRows != nil {
		hint = "type to search · ↑/↓ select · tab all/project · enter resume · esc back"
	}
	hints := th["dim"].Render(hint)
	for len(lines) < m.height-1 {
		lines = append(lines, "")
	}
	if m.height > 1 && len(lines) > m.height-1 {
		lines = lines[:m.height-1] // a short pane: the hint row still fits
	}
	lines = append(lines, hints)
	for i := range lines {
		lines[i] = ansi.Truncate(lines[i], m.width, "…")
	}
	return strings.Join(lines, "\n")
}

// shortDir renders a session's working directory: "." for this
// directory, "?" for a file predating the meta entry, ~-abbreviated
// otherwise.
func shortDir(dir, cwd, home string) string {
	switch {
	case dir == "":
		return "?"
	case dir == cwd:
		return "."
	case home != "" && strings.HasPrefix(dir, home+"/"):
		return "~" + strings.TrimPrefix(dir, home)
	}
	return dir
}

func pluralWord(n int, one, many string) string {
	if n == 1 {
		return one
	}
	return many
}

// shortID is a session id's last eight characters: ids are time-ordered,
// so sessions started within minutes share a prefix but not a suffix.
func shortID(id string) string {
	if r := []rune(id); len(r) > 8 {
		return string(r[len(r)-8:])
	}
	return id
}

// elideMiddle caps a path at n runes by cutting whole segments out of
// its middle ("~/…/scratch/1a2b…"), never through a segment such as an
// opaque uuid; a path that cannot be cut keeps its last segment whole.
func elideMiddle(path string, n int) string {
	if len([]rune(path)) <= n {
		return path
	}
	parts := strings.Split(path, "/")
	if len(parts) < 3 {
		return path
	}
	head, tail := parts[0], parts[len(parts)-1]
	for i := len(parts) - 2; i > 0; i-- {
		next := parts[i] + "/" + tail
		if len([]rune(head+"/…/"+next)) > n {
			break
		}
		tail = next
	}
	return head + "/…/" + tail
}

// truncateCols caps s at n cells, first line only, with an ellipsis;
// it cuts between graphemes, never inside a ZWJ sequence.
func truncateCols(s string, n int) string {
	return ansi.Truncate(strings.SplitN(s, "\n", 2)[0], n, "…")
}

var (
	jobStartRe = regexp.MustCompile(`^job (\d+) started in the background`)
	jobDoneRe  = regexp.MustCompile(`job (\d+) \[([^\]]+)\]`)
)

// deadJobs lists, in start order, the background jobs a transcript
// started ("job N started in the background" results) with no later
// finished notice (a "job" entry or a "[background job]" wake input
// naming "job N [status]").
func deadJobs(entries []history.Entry) []int {
	var open []int
	for _, e := range entries {
		text, _ := e.Data["text"].(string)
		switch {
		case e.Kind == "result":
			if g := jobStartRe.FindStringSubmatch(text); g != nil {
				n, _ := strconv.Atoi(g[1])
				open = append(open, n)
			}
		case e.Kind == "job" || (e.Kind == "input" && strings.HasPrefix(text, "[background job]")):
			for _, g := range jobDoneRe.FindAllStringSubmatch(text, -1) {
				if g[2] == "running" {
					continue
				}
				n, _ := strconv.Atoi(g[1])
				open = slices.DeleteFunc(open, func(x int) bool { return x == n })
			}
		}
	}
	return open
}
