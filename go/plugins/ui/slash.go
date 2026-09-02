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
	open := m.cfg.Load().cmds != nil && !m.inspecting && !m.picking &&
		slashStart(draft) >= 0 && !m.pal.escaped
	if open && !m.pal.open {
		m.pal.selected = 0
	}
	m.pal.open = open
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
// palette's "/".
func (m *model) paletteQuery() string {
	draft := m.input.Value()
	if i := slashStart(draft); i >= 0 {
		return draft[i+1:]
	}
	return ""
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
		items[i] = paletteItem{name: in.Name, usage: in.Usage, summary: in.Summary}
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
		m.pal.escaped = true
		m.pal.escAt = m.input.Value()
		return true, nil
	case palComplete:
		// Tab: rewrite the word to "/name " (stays open at line start).
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
// half-typed "/cl" accepts as the bare "/clear").
func (m *model) acceptPalette(name string) tea.Cmd {
	line := "/" + name
	if draft := strings.TrimSpace(m.input.Value()); draft == line ||
		strings.HasPrefix(draft, line+" ") {
		line = draft
	}
	return m.dispatch(line)
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
func (m *model) dispatch(line string) tea.Cmd {
	cfg := m.cfg.Load()
	m.input.Reset()
	m.syncPalette()
	name, args, _ := strings.Cut(strings.TrimPrefix(line, "/"), " ")
	args = strings.TrimSpace(args)
	m.log(cfg, "command", line)
	m.blocks = append(m.blocks, block{id: m.nextID, kind: "command", text: line})
	m.nextID++
	out, err := cfg.cmds.Run(name, args)
	var act commands.UIAction
	if errors.As(err, &act) {
		if text, ok := commands.SubmitText(act); ok {
			// A skill command: the line goes to the loop as input.
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
	switch act {
	case commands.ActionClear:
		// The visible transcript only; history is untouched. The
		// welcome text does not come back either.
		m.blocks = nil
		m.focusID = -1
		m.welcome = false
		m.refresh()
	case commands.ActionCollapse:
		m.setAllCollapsed(true)
	case commands.ActionExpand:
		m.setAllCollapsed(false)
	case commands.ActionQuit:
		return tea.Quit
	case commands.ActionOpenPicker:
		m.openPicker()
	default:
		m.blocks = append(m.blocks, block{id: m.nextID, kind: "error",
			text: fmt.Sprintf("unknown ui action %q", string(act))})
		m.nextID++
		m.refresh()
		m.vp.GotoBottom()
	}
	return nil
}

// log records one dispatch half to history when a writable history
// service is mounted.
func (m *model) log(cfg *uiCfg, kind, text string) {
	if cfg.hlog != nil {
		cfg.hlog.Append(kind, map[string]any{"text": text})
	}
}
