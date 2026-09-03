package ui

// UI actions by name: the keymap's bindable actions plus the effects
// commands trigger (the session picker, /clear, /keys, ...). One
// table feeds the "/" palette's action rows, /keys, chord validation
// and runAction — keys, chords and the palette all run an action
// through the same switch.

import (
	"slices"
	"time"

	tea "charm.land/bubbletea/v2"
)

// uiAction is one named action and its /keys description.
type uiAction struct {
	name, desc string
}

// uiActions lists every action, in /keys order. The first group is
// the keymap's (bindable) actions; the rest are reached by chord, the
// palette, or a command.
var uiActions = []uiAction{
	{"quit", "quit"},
	{"clear_input", "clear the composer"},
	{"history_inspect", "inspect history (toggle)"},
	{"block_next", "focus next block"},
	{"block_prev", "focus previous block"},
	{"collapse_toggle", "toggle the focused block"},
	{"collapse_all", "collapse all blocks"},
	{"expand_all", "expand all blocks"},
	{"todo_toggle", "pin/unpin the todo list"},
	{"external_editor", "edit the draft in $VISUAL / $EDITOR"},
	{"scroll_up", "scroll up"},
	{"scroll_down", "scroll down"},
	{"page_up", "page up"},
	{"page_down", "page down"},
	{"sessions", "pick a session to resume"},
	{"clear", "clear the visible transcript"},
	{"keys", "show the keybindings"},
	{"palette", "open the action palette"},
	{"undo", "run /undo (when a command provides it)"},
}

func knownAction(name string) bool {
	return slices.ContainsFunc(uiActions, func(a uiAction) bool { return a.name == name })
}

func actionNames() []string {
	out := make([]string, len(uiActions))
	for i, a := range uiActions {
		out[i] = a.name
	}
	return out
}

func actionDesc(name string) string {
	for _, a := range uiActions {
		if a.name == name {
			return a.desc
		}
	}
	return name
}

// actionKey is how an action is reached: its bound key, else its
// chord ("ctrl+x l"), else "" (palette only).
func actionKey(cfg *uiCfg, name string) string {
	if k := cfg.keys[name]; k != "" {
		return k
	}
	for _, k := range sortedKeys(cfg.chords) {
		if cfg.chords[k] == name {
			return cfg.keys["leader"] + " " + k
		}
	}
	return ""
}

// runAction performs one action by name — the one switch behind the
// keymap, the chords and the palette's action rows. via is the key
// label that triggered it, named in the quit hint; "" is the palette,
// whose quit row is an explicit pick and quits outright like /quit
// (a key's press only arms — the enter that accepted the row would
// otherwise disarm it first).
func (m *model) runAction(name, via string, cfg *uiCfg) tea.Cmd {
	switch name {
	case "quit":
		if via == "" {
			return tea.Quit
		}
		return m.quitPress(via)
	case "scroll_up":
		m.pane().ScrollUp(1)
	case "scroll_down":
		m.pane().ScrollDown(1)
	case "page_up":
		m.pane().PageUp()
	case "page_down":
		m.pane().PageDown()
	case "clear_input":
		m.input.Reset()
		m.syncPalette()
		m.layoutComposer()
	case "block_next":
		if !m.inspecting {
			m.moveFocus(1)
		}
	case "block_prev":
		if !m.inspecting {
			m.moveFocus(-1)
		}
	case "collapse_all":
		m.flash = collapseNote(true, m.setAllCollapsed(true))
	case "expand_all":
		m.flash = collapseNote(false, m.setAllCollapsed(false))
	case "collapse_toggle":
		// The focused block, else the newest collapsible one.
		if !m.inspecting && !m.toggleFocused() {
			if f := m.focusables(); len(f) > 0 {
				m.toggleBlock(f[len(f)-1])
			}
		}
	case "todo_toggle":
		if m.todoText == "" {
			m.flash = "no todo list yet (/todo add <text>)"
			return nil
		}
		m.todoPinned = !m.todoPinned
	case "external_editor":
		if m.inspecting {
			return nil
		}
		return m.openEditor()
	case "history_inspect":
		if m.inspecting {
			m.inspecting, m.diving = false, 0
			m.syncPalette()
			return nil
		}
		// On a focused subagent card: dive into the child's transcript.
		if i := m.focusedSpawn(); i >= 0 {
			m.inspecting, m.diving = true, m.blocks[i].id
			m.refreshOverlay()
			m.overlay.GotoTop()
			m.syncPalette()
			return nil
		}
		if cfg.hist == nil {
			m.flash = "no history service mounted"
			return nil
		}
		m.inspecting = true
		m.refreshOverlay()
		m.overlay.GotoBottom()
		m.syncPalette() // the palette is inert under the inspector
	case "sessions":
		m.openPicker()
	case "clear":
		// The visible transcript only; history is untouched.
		m.blocks = nil
		m.focusID = -1
		m.welcome = false
		m.refresh()
	case "keys":
		m.showKeys()
	case "palette":
		// Actions mode: the "/" palette over the action rows alone.
		// The palette is inert under the inspector, so say so rather
		// than leave a stray "/" behind; the draft it displaces is
		// stashed and comes back when the mode ends.
		if m.inspecting {
			m.flash = "no palette under the inspector (" + cfg.keys["history_inspect"] + " closes it)"
			return nil
		}
		if !m.pal.actionsOnly {
			m.pal.stash = m.input.Value()
		}
		m.pal.actionsOnly = true
		m.pal.escaped = false // an earlier esc on "/" must not keep this one shut
		m.setDraft("/")
		m.input.CursorEnd()
	case "undo":
		if cfg.cmds != nil && hasCommand(cfg.cmds, "undo") {
			return m.dispatch("/undo")
		}
		m.flash = "no /undo command"
	default:
		m.flash = "unknown action " + name
	}
	return nil
}

func hasCommand(cmds commandsView, name string) bool {
	for _, in := range cmds.List() {
		if in.Name == name {
			return true
		}
	}
	return false
}

// chordKey resolves the key after the leader: a bound chord runs its
// action, esc backs out quietly, anything else clears the pending
// leader with a flash naming it. Only the quit chord keeps the quit
// armed (it arms like ctrl+c); every other key disarms.
func (m *model) chordKey(key string, cfg *uiCfg) tea.Cmd {
	leader := cfg.keys["leader"]
	action, ok := cfg.chords[key]
	if !ok {
		if key != "esc" {
			m.flash = leader + " " + key + ": no such chord"
		}
		m.stop.armedAt = time.Time{}
		return nil
	}
	if action != "quit" {
		m.stop.armedAt = time.Time{}
	}
	return m.runAction(action, leader+" "+key, cfg)
}

// chordRows renders the chords for /keys as "leader key  desc".
func chordRows(cfg *uiCfg) [][2]string {
	var rows [][2]string
	for _, k := range sortedKeys(cfg.chords) {
		rows = append(rows, [2]string{cfg.keys["leader"] + " " + k, actionDesc(cfg.chords[k])})
	}
	return rows
}

func sortedKeys(m map[string]string) []string {
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	slices.Sort(out)
	return out
}
