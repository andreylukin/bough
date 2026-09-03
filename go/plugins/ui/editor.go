package ui

// ctrl+g (external_editor) hands the draft to $VISUAL, else $EDITOR,
// else nano: the TUI suspends while the editor runs on a temp file
// (tea.ExecProcess) and the file's content replaces the draft when it
// exits. A non-zero exit keeps the draft as it was.

import (
	"os"
	"os/exec"
	"strings"

	tea "charm.land/bubbletea/v2"
)

// editorDoneMsg delivers the editor's exit to Update.
type editorDoneMsg struct {
	path string
	err  error
}

// editorCommand builds the editor invocation on path; the variable
// may carry flags ("code --wait").
func editorCommand(path string) *exec.Cmd {
	ed := os.Getenv("VISUAL")
	if ed == "" {
		ed = os.Getenv("EDITOR")
	}
	if ed == "" {
		ed = "nano"
	}
	args := strings.Fields(ed)
	return exec.Command(args[0], append(args[1:], path)...)
}

// saveDraft writes the draft to a temp file for the editor.
func saveDraft(draft string) (string, error) {
	f, err := os.CreateTemp("", "bough-draft-*.md")
	if err != nil {
		return "", err
	}
	_, werr := f.WriteString(draft)
	if cerr := f.Close(); werr == nil {
		werr = cerr
	}
	return f.Name(), werr
}

// openEditor saves the draft and returns the suspending editor cmd.
func (m *model) openEditor() tea.Cmd {
	path, err := saveDraft(m.input.Value())
	if err != nil {
		m.flash = "editor: " + err.Error()
		return nil
	}
	return tea.ExecProcess(editorCommand(path), func(err error) tea.Msg {
		return editorDoneMsg{path: path, err: err}
	})
}

// finishEditor replaces the draft with the edited file and removes it.
func (m *model) finishEditor(msg editorDoneMsg) {
	data, rerr := os.ReadFile(msg.path)
	os.Remove(msg.path)
	if msg.err != nil {
		m.flash = "editor: " + msg.err.Error() + " · draft kept"
		return
	}
	if rerr != nil {
		m.flash = "editor: " + rerr.Error()
		return
	}
	m.setDraft(strings.TrimRight(string(data), "\n"))
}
