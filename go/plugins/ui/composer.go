package ui

// The composer: a multi-line textarea. Enter submits (model.go;
// alt+enter is the follow_up keymap action there); shift+enter /
// ctrl+j insert a newline; a paste keeps its newlines. Up/Down (or
// ctrl+p/ctrl+n) move the cursor while the draft spans more than one
// visual row, and recall prompts once it is on the first/last row —
// this directory's earlier sessions included, newest first. Home/End
// on an empty composer jump the transcript to top/bottom. Emacs-style
// ctrl+a/e/w/u/k are the textarea's own bindings.

import (
	"fmt"
	"slices"
	"strings"

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
// typed before browsing began. past caches this directory's earlier
// sessions' prompts, read once on the first recall (loaded marks that
// read, since the answer can legitimately be empty).
type composerState struct {
	recall int
	draft  string
	past   []string
	loaded bool
	// dropped are drafts esc cleared. Claude Code does the same on
	// double-esc ("saves the draft to history so Up recalls it"): a
	// draft is cheap to clear only if clearing it is undoable.
	dropped []string
}

// dropDraft remembers a draft esc is about to clear, so Up brings it
// back. Consecutive duplicates are squeezed, like the rest of history.
func (m *model) dropDraft(text string) {
	if strings.TrimSpace(text) == "" {
		return
	}
	if n := len(m.comp.dropped); n > 0 && m.comp.dropped[n-1] == text {
		return
	}
	m.comp.dropped = append(m.comp.dropped, text)
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
	ta.KeyMap.InsertNewline = key.NewBinding(key.WithKeys("shift+enter", "ctrl+j"))
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
	// The background-job strip sits under the composer and takes its
	// rows from the transcript, like the status bar does.
	cfg := m.cfg.Load()
	n += len(m.jobRows(cfg)) + len(m.todoRows(cfg)) + len(m.boardRows(cfg))
	if h := m.height - 1 - n; h > 0 {
		atBottom := m.vp.AtBottom()
		m.vp.SetHeight(h)
		m.overlay.SetHeight(h)
		if atBottom {
			m.vp.GotoBottom()
		}
	}
}

// prompts is what Up walks: this directory's prompts from earlier
// sessions first, then this session's, in transcript order with
// consecutive duplicates squeezed. Index len-1 is the most recent, so
// the first Up lands on the last thing typed.
//
// History that ends with the session is not history. Every launch in a
// directory now starts a new session, so a composer that only knew its
// own transcript opened with an empty Up arrow every time.
func (m *model) prompts() []string {
	if !m.comp.loaded {
		m.comp.loaded = true
		if p := m.cfg.Load().past; p != nil {
			// RecentPrompts is newest-first; the composer walks
			// oldest-first, and this session's go on the end.
			past := p()
			m.comp.past = make([]string, 0, len(past))
			for i := len(past) - 1; i >= 0; i-- {
				m.comp.past = append(m.comp.past, past[i])
			}
		}
	}
	out := slices.Clone(m.comp.past)
	for i := range m.blocks {
		if b := &m.blocks[i]; b.kind == "user" && (len(out) == 0 || out[len(out)-1] != b.text) {
			out = append(out, b.text)
		}
	}
	// A draft cleared by esc is the most recent thing typed, so it is
	// the first thing Up offers.
	for _, d := range m.comp.dropped {
		if len(out) == 0 || out[len(out)-1] != d {
			out = append(out, d)
		}
	}
	return out
}

// onFirstRow and onLastRow are the edges Up and Down browse history
// from. They count VISUAL rows, not logical lines: a single long line
// soft-wraps to several rows, and Claude Code's rule — "when the input
// spans more than one visual row, whether wrapped or multiline, first
// moves the cursor within the prompt; once the cursor is on the first
// or last visual row, pressing again navigates command history" — is
// the one that matches what the eye sees. Judging by logical line sent
// a wrapped paragraph straight to history with the cursor three rows
// down.
func (m *model) onFirstRow() bool {
	return m.input.Line() == 0 && m.input.LineInfo().RowOffset == 0
}

func (m *model) onLastRow() bool {
	li := m.input.LineInfo()
	return m.input.Line() >= m.input.LineCount()-1 && li.RowOffset >= li.Height-1
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
	// readline's history keys, which Claude Code also binds: ctrl+p and
	// ctrl+n are up and down for every purpose below.
	switch key {
	case "ctrl+p":
		key = "up"
	case "ctrl+n":
		key = "down"
	}
	switch key {
	case "up", "down":
	default:
		m.comp.recall = -1
	}
	switch key {
	case "up":
		if !m.onFirstRow() {
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
		if !m.onLastRow() {
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
		// A path under the cursor is the certain completion, so it
		// wins; otherwise tab takes the small model's guess at the
		// rest of the sentence.
		if m.tabComplete() || m.acceptSuggestion() {
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

// editKey feeds one key straight to the textarea. Every edit re-arms
// the autocomplete pause (predict.go); with no llm-small row that is a
// nil command.
func (m *model) editKey(msg tea.KeyPressMsg) (bool, tea.Cmd) {
	var cmd tea.Cmd
	m.input, cmd = m.input.Update(msg)
	m.syncPalette()
	m.layoutComposer()
	return true, tea.Batch(cmd, m.schedulePredict(m.cfg.Load()))
}

func (m *model) setDraft(s string) {
	m.input.SetValue(s)
	m.syncPalette()
	m.layoutComposer()
}
