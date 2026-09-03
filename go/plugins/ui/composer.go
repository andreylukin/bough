package ui

// The composer: a multi-line textarea. Enter submits (model.go);
// shift+enter / alt+enter / ctrl+j insert a newline; a paste keeps its
// newlines. Up/Down on the first/last line (or an empty composer)
// recall this session's prompts, newest first; Home/End on an empty
// composer jump the transcript to top/bottom. Emacs-style ctrl+a/e/w/u/k
// are the textarea's own bindings.

import (
	"fmt"

	"charm.land/bubbles/v2/key"
	"charm.land/bubbles/v2/textarea"
	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
)

// composerMaxLines caps the composer's height; longer drafts scroll
// inside it.
const composerMaxLines = 8

// composerState is the prompt-recall cursor: recall is the index into
// prompts() being shown, -1 while not browsing; draft is what was
// typed before browsing began.
type composerState struct {
	recall int
	draft  string
}

func newComposer() textarea.Model {
	ta := textarea.New()
	ta.Prompt = "> "
	ta.Placeholder = "say something"
	ta.ShowLineNumbers = false
	ta.MaxHeight = composerMaxLines
	// Grow with the draft's VISUAL rows: a long single line soft-wraps,
	// and sizing by logical LineCount left the top row scrolled out of
	// view. The textarea counts wrapped rows itself under DynamicHeight.
	ta.DynamicHeight = true
	ta.MinHeight = 1
	// MaxHeight alone caps the draft at 8 logical lines; the content cap
	// is what lets a longer paste in and scroll.
	ta.MaxContentHeight = 100000
	ta.SetVirtualCursor(true)
	ta.KeyMap.InsertNewline = key.NewBinding(key.WithKeys("shift+enter", "alt+enter", "ctrl+j"))
	// The transcript's ground shows through: no cursor-line bar, no
	// end-of-buffer tint.
	st := ta.Styles()
	st.Focused.CursorLine = lipgloss.NewStyle()
	st.Focused.EndOfBuffer = lipgloss.NewStyle()
	st.Blurred.CursorLine = lipgloss.NewStyle()
	st.Blurred.EndOfBuffer = lipgloss.NewStyle()
	ta.SetStyles(st)
	ta.SetHeight(1)
	ta.Focus()
	return ta
}

// layoutComposer takes the composer's height (the textarea sizes itself
// to the draft's wrapped rows, 1..composerMaxLines) and gives the
// transcript the rest of the screen: status bar plus composer rows come
// off the height.
func (m *model) layoutComposer() {
	n := min(max(m.input.Height(), 1), composerMaxLines)
	if h := m.height - 1 - n; h > 0 {
		atBottom := m.vp.AtBottom()
		m.vp.SetHeight(h)
		m.overlay.SetHeight(h)
		if atBottom {
			m.vp.GotoBottom()
		}
	}
}

// prompts returns this session's user prompts in transcript order
// (replayed history included), consecutive duplicates squeezed.
func (m *model) prompts() []string {
	var out []string
	for i := range m.blocks {
		if b := &m.blocks[i]; b.kind == "user" && (len(out) == 0 || out[len(out)-1] != b.text) {
			out = append(out, b.text)
		}
	}
	return out
}

// composerKey handles the composer's navigation keys before the keymap
// waterfall: up/down inside a multi-line draft move the cursor, on its
// edge lines they browse prompt history; home/end on an empty draft
// jump the transcript; tab on a path-like word completes it (see
// pathcomplete.go). Anything else ends a recall browse. Reports
// whether the key was consumed.
func (m *model) composerKey(key string, msg tea.KeyPressMsg) (bool, tea.Cmd) {
	if m.inspecting || m.pal.open {
		return false, nil
	}
	switch key {
	case "up", "down":
	default:
		m.comp.recall = -1
	}
	switch key {
	case "up":
		if m.input.Line() > 0 {
			return m.editKey(msg)
		}
		ps := m.prompts()
		switch {
		case len(ps) == 0:
			return false, nil // nothing to recall: the keymap scrolls
		case m.comp.recall < 0:
			m.comp.draft = m.input.Value()
			m.comp.recall = len(ps) - 1
		case m.comp.recall > 0:
			m.comp.recall--
		default:
			return true, nil // oldest already shown
		}
		m.setDraft(ps[m.comp.recall])
		m.flash = recallNote(m.comp.recall, len(ps))
		return true, nil
	case "down":
		if m.input.Line() < m.input.LineCount()-1 {
			return m.editKey(msg)
		}
		if m.comp.recall < 0 {
			return false, nil
		}
		ps := m.prompts()
		if m.comp.recall++; m.comp.recall >= len(ps) {
			m.comp.recall = -1
			m.setDraft(m.comp.draft)
			m.flash = "draft restored"
		} else {
			m.setDraft(ps[m.comp.recall])
			m.flash = recallNote(m.comp.recall, len(ps))
		}
		return true, nil
	case "tab":
		if m.tabComplete() {
			return true, nil
		}
	case "home", "end":
		if m.input.Value() != "" {
			return m.editKey(msg) // line start/end inside the draft
		}
		if key == "home" {
			m.vp.GotoTop()
		} else {
			m.vp.GotoBottom()
		}
		return true, nil
	}
	return false, nil
}

// recallNote is the status-bar cue while a past prompt sits in the
// composer: Up put it there silently, and a user who meant to scroll
// would otherwise type onto its end and send the two glued together.
func recallNote(i, n int) string {
	return fmt.Sprintf("↑ recalled prompt %d/%d · esc clears · ↓ back", n-i, n)
}

// editKey feeds one key straight to the textarea.
func (m *model) editKey(msg tea.KeyPressMsg) (bool, tea.Cmd) {
	var cmd tea.Cmd
	m.input, cmd = m.input.Update(msg)
	m.syncPalette()
	m.layoutComposer()
	return true, cmd
}

func (m *model) setDraft(s string) {
	m.input.SetValue(s)
	m.syncPalette()
	m.layoutComposer()
}
