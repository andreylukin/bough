package ui

// Composer integration for the "/" palette and command dispatch: the
// model owns opening and closing (the palette itself never dispatches,
// see palette.go), and a submitted "/" line goes through the
// "commands" service — never to the LLM.

import (
	"errors"
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/andreylukin/bough/plugins/commands"
)

// syncPalette derives the palette from the DRAFT, never from the key
// (old bough M17): backspacing back to "/" reopens it, backspacing
// past it closes it, and the list can never disagree with the text it
// is filtering. Esc keeps it closed until the draft changes. With no
// commands service "/" is plain text and the palette never opens; it
// is inert under the inspector and the session picker. A "/" opens
// it at line start (dispatch) or at the start of the last word
// ("look at /he" — accept completes the word in place).
func (m *model) syncPalette() {
	draft := m.input.Value()
	if m.pal.escaped && draft != m.pal.escAt {
		m.pal.escaped = false
	}
	if m.pal.cycling && draft != m.pal.cycleDraft {
		m.pal.cycling = false // any edit ends a Tab cycle
	}
	open := m.cfg.Load().cmds != nil && !m.inspecting && !m.picking && !m.mp.open &&
		slashStart(draft) >= 0 && !m.pal.escaped
	if open && !m.pal.open {
		m.pal.selected = 0
	}
	m.pal.open = open
	m.syncAt() // the "@" picker follows the same draft
}

// overlayRows is whichever composer picker is open: "/" or "@".
func (m *model) overlayRows() []string {
	if lines := m.paletteRows(); len(lines) > 0 {
		return lines
	}
	return m.atRows()
}

// slashStart is the index of the "/" the palette is filtering on: 0
// when the draft starts with "/", else the "/" opening the last
// whitespace-separated word, else -1 (a path like "src/x" never opens).
func slashStart(draft string) int {
	if strings.HasPrefix(draft, "/") {
		return 0
	}
	i := strings.LastIndexAny(draft, " \t\n") + 1
	if i > 0 && i < len(draft) && draft[i] == '/' {
		return i
	}
	return -1
}

// paletteQuery is the text the palette filters: the draft after the
// palette's "/" — or, mid Tab-cycle, the query the cycle started
// from, so the list keeps every match while Tab walks them.
func (m *model) paletteQuery() string {
	draft := m.input.Value()
	if m.pal.cycling && draft == m.pal.cycleDraft {
		return m.pal.cycleQuery
	}
	if i := slashStart(draft); i >= 0 {
		return draft[i+1:]
	}
	return ""
}

// draftWord is the "/word" the composer holds (without args): what
// the user actually typed, for the fuzzy-accept echo.
func (m *model) draftWord() string {
	draft := strings.TrimSpace(m.input.Value())
	if i := slashStart(m.input.Value()); i >= 0 {
		draft = m.input.Value()[i:]
	}
	word, _, _ := strings.Cut(strings.TrimSpace(draft), " ")
	return word
}

// completePalette rewrites the palette's word to "/name " in place.
func (m *model) completePalette(name string) {
	draft := m.input.Value()
	i := slashStart(draft)
	if i < 0 {
		i = 0
		draft = ""
	}
	m.input.SetValue(draft[:i] + "/" + name + " ")
	m.input.CursorEnd()
	m.pal.cycleDraft = m.input.Value()
	m.syncPalette()
}

// paletteItems adapts the commands service's list to palette rows.
func (m *model) paletteItems() []paletteItem {
	cmds := m.cfg.Load().cmds
	if cmds == nil {
		return nil
	}
	infos := cmds.List()
	items := make([]paletteItem, len(infos))
	for i, in := range infos {
		items[i] = paletteItem{name: in.Name, usage: in.Usage, summary: in.Summary,
			skill: in.IsSkill() || in.IsTemplate()}
	}
	return items
}

// paletteRows renders the open palette's overlay lines for View.
func (m *model) paletteRows() []string {
	if !m.pal.open {
		return nil
	}
	items := paletteFilter(m.paletteItems(), m.paletteQuery())
	maxRows := palMaxRows
	if h := m.vp.Height(); h < maxRows {
		maxRows = h
	}
	return paletteLines(items, m.pal.selected, m.width, maxRows, m.cfg.Load().theme)
}

// paletteKey routes one key into the open palette, reporting whether
// the palette consumed it.
func (m *model) paletteKey(key string) (bool, tea.Cmd) {
	items := paletteFilter(m.paletteItems(), m.paletteQuery())
	act, name := m.pal.onKey(key, items)
	switch act {
	case palMoved:
		return true, nil
	case palClose:
		// Esc on a lone "/query" drops it (the user backed out of a
		// command); text with anything more stays.
		if draft := m.input.Value(); strings.HasPrefix(draft, "/") &&
			!strings.ContainsAny(strings.TrimSpace(draft), " \t\n") {
			m.input.Reset()
			m.pal.cycling = false
		}
		m.pal.escaped = true
		m.pal.escAt = m.input.Value()
		return true, nil
	case palComplete:
		// Tab: rewrite the word to "/name " (stays open at line
		// start); a repeated Tab walks the matches of the query the
		// first one completed from.
		if m.pal.cycling {
			m.pal.selected = (m.pal.selected + 1) % len(items)
			name = items[m.pal.selected].name
		} else {
			m.pal.cycling = true
			m.pal.cycleQuery = m.paletteQuery()
		}
		m.completePalette(name)
		return true, nil
	case palAccept:
		if slashStart(m.input.Value()) > 0 {
			// Mid-text there is nothing to dispatch: complete instead.
			m.completePalette(name)
			return true, nil
		}
		return true, m.acceptPalette(name)
	}
	return false, nil
}

// acceptPalette dispatches the selected name — plus the typed args
// when the draft already names it ("/clear now" accepts as typed, a
// half-typed "/cl" accepts as the bare "/clear"). A fuzzy accept (the
// typed word is not a prefix of the name: "/sesion" → /sessions) says
// so in the echo, so the command that actually ran is never a secret.
func (m *model) acceptPalette(name string) tea.Cmd {
	line := "/" + name
	echo := line
	if draft := strings.TrimSpace(m.input.Value()); draft == line ||
		strings.HasPrefix(draft, line+" ") {
		line, echo = draft, draft
	} else if word := m.draftWord(); word != "/" &&
		!strings.HasPrefix(strings.ToLower(line), strings.ToLower(word)) {
		echo = line + " (from " + word + ")"
	}
	return m.dispatchAs(line, echo)
}

// clickPalette maps a left click on a palette row to select + accept.
func (m *model) clickPalette(mouse tea.Mouse) (bool, tea.Cmd) {
	if !m.pal.open {
		return false, nil
	}
	items := paletteFilter(m.paletteItems(), m.paletteQuery())
	if len(items) == 0 {
		return false, nil
	}
	sel := m.pal.selected
	if sel >= len(items) {
		sel = len(items) - 1
	}
	maxRows := palMaxRows
	if h := m.vp.Height(); h < maxRows {
		maxRows = h
	}
	first, rows := paletteWindow(len(items), sel, maxRows)
	top := m.vp.Height() - rows
	if rows == 0 || mouse.Y < top || mouse.Y >= m.vp.Height() {
		return false, nil
	}
	m.pal.selected = first + (mouse.Y - top)
	m.pal.open = false
	return true, m.acceptPalette(items[m.pal.selected].name)
}

// dispatch runs one submitted "/" line through the commands service —
// never the LLM. The line renders as a dim command echo and its output
// as a "system" block (errors included: an unknown name comes back as
// "unknown command: /x (try /help)"); a UIAction sentinel is an effect
// only the UI can perform, interpreted here. Every command renders
// output or a reason (M27): an empty output echoes the command name.
// Both halves are recorded to history as "command"/"system" entries —
// never "input", which DefaultProject would feed to the model.
func (m *model) dispatch(line string) tea.Cmd { return m.dispatchAs(line, line) }

// dispatchAs is dispatch with a distinct echo text (the fuzzy-accept
// "/sessions (from /sesion)").
func (m *model) dispatchAs(line, echo string) tea.Cmd {
	cfg := m.cfg.Load()
	m.input.Reset()
	m.syncPalette()
	name, args, _ := strings.Cut(strings.TrimPrefix(line, "/"), " ")
	args = strings.TrimSpace(args)
	m.log(cfg, "command", echo)
	m.blocks = append(m.blocks, block{id: m.nextID, kind: "command", text: echo})
	m.nextID++
	out, err := cfg.cmds.Run(name, args)
	if act, ok := errors.AsType[commands.UIAction](err); ok {
		if text, ok := commands.SubmitText(act); ok {
			// A skill or template command: the text goes to the loop
			// as input.
			m.log(cfg, "system", "/"+name)
			return m.submit(text)
		}
		m.refresh()
		m.vp.GotoBottom()
		return m.perform(act)
	}
	if err != nil {
		out = err.Error()
	}
	if out == "" {
		out = "/" + name
	}
	m.log(cfg, "system", out)
	m.blocks = append(m.blocks, block{id: m.nextID, kind: "system", text: out})
	m.nextID++
	m.refresh()
	m.vp.GotoBottom()
	return nil
}

// perform applies a UIAction: the effect is the command's visible
// answer. Unknown actions fail loud as an error block.
func (m *model) perform(act commands.UIAction) tea.Cmd {
	if id, ok := commands.ResumeID(act); ok {
		m.resumeID(id)
		return nil
	}
	if cur, rows, ok := commands.ModelPickerChoices(act); ok {
		m.openModelPicker(cur, rows)
		return nil
	}
	switch act {
	case commands.ActionClear:
		// The visible transcript only; history is untouched. The
		// welcome text does not come back either.
		m.blocks = nil
		m.focusID = -1
		m.welcome = false
		m.refresh()
	case commands.ActionCollapse:
		m.noteSystem(collapseNote(true, m.setAllCollapsed(true)))
	case commands.ActionExpand:
		m.noteSystem(collapseNote(false, m.setAllCollapsed(false)))
	case commands.ActionQuit:
		return tea.Quit
	case commands.ActionOpenPicker:
		m.openPicker()
	case commands.ActionKeys:
		m.showKeys()
	default:
		m.blocks = append(m.blocks, block{id: m.nextID, kind: "error",
			text: fmt.Sprintf("unknown ui action %q", string(act))})
		m.nextID++
		m.refresh()
		m.vp.GotoBottom()
	}
	return nil
}

// keysText renders the live keymap (cfg.action, so rebinds show) plus
// the fixed keys the composer and palette own.
func keysText(cfg *uiCfg) string {
	rows := [][2]string{
		{"enter", "send the line (/cmd dispatches, !cmd runs a shell command)"},
	}
	order := []string{"quit", "clear_input", "history_inspect", "block_next", "block_prev",
		"collapse_toggle", "collapse_all", "expand_all", "todo_toggle", "external_editor", "scroll_up", "scroll_down", "page_up", "page_down"}
	desc := map[string]string{
		"quit": "quit", "clear_input": "clear the composer", "history_inspect": "inspect history (toggle)",
		"block_next": "focus next block", "block_prev": "focus previous block",
		"collapse_toggle": "toggle the focused block", "collapse_all": "collapse all blocks",
		"expand_all": "expand all blocks", "todo_toggle": "pin/unpin the todo list", "external_editor": "edit the draft in $VISUAL / $EDITOR", "scroll_up": "scroll up", "scroll_down": "scroll down",
		"page_up": "page up", "page_down": "page down",
	}
	for _, a := range order {
		if k := cfg.keys[a]; k != "" {
			rows = append(rows, [2]string{k, desc[a]})
		}
	}
	rows = append(rows,
		[2]string{"esc", "close the palette · decline a pending ask"},
		[2]string{"tab", "on a path: complete it, again to cycle · in the palette: complete"},
		[2]string{"ctrl+v", "an image on the clipboard: save it under ~/.bough/attachments as @path"},
		[2]string{"?", "on an empty composer: this list (/keys)"},
	)
	w := 0
	for _, r := range rows {
		if len(r[0]) > w {
			w = len(r[0])
		}
	}
	var b strings.Builder
	b.WriteString("keys\n")
	for _, r := range rows {
		fmt.Fprintf(&b, "  %-*s  %s\n", w, r[0], r[1])
	}
	return strings.TrimRight(b.String(), "\n")
}

// showKeys appends the keymap as a system block (the /keys answer,
// and "?" on an empty composer).
func (m *model) showKeys() {
	cfg := m.cfg.Load()
	text := keysText(cfg)
	m.log(cfg, "system", text)
	m.blocks = append(m.blocks, block{id: m.nextID, kind: "system", text: text})
	m.nextID++
	m.refresh()
	m.vp.GotoBottom()
}

// log records one dispatch half to history when a writable history
// service is mounted.
func (m *model) log(cfg *uiCfg, kind, text string) {
	if cfg.hlog != nil {
		cfg.hlog.Append(kind, map[string]any{"text": text})
	}
}
