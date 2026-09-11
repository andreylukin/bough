package ui

// Writing the clipboard. OSC 52 alone is not enough: iTerm2 blocks it
// until "Applications in terminal may access clipboard" is on, VS Code
// and screen mangle it, and the write is fire-and-forget so a blocked
// terminal fails silently. Like Claude Code and opencode, a copy also
// runs the platform's clipboard tool (pbcopy; wl-copy, xclip or xsel;
// PowerShell Set-Clipboard) and loads the tmux paste buffer when
// running under tmux. The flash names which paths took the text.

import (
	"context"
	"os"
	"os/exec"
	"runtime"
	"strings"

	tea "charm.land/bubbletea/v2"
)

// copiedMsg reports a finished clipboard write: what the flash should
// say (n things) and which paths took the text.
type copiedMsg struct {
	note string
	via  []string
}

// writeClipboardNative pipes text to the platform clipboard tool and
// the tmux buffer. It returns the names of the tools that succeeded.
// A package var so tests can stub the external commands.
var writeClipboardNative = clipboardNative

func clipboardNative(text string) []string {
	ctx, cancel := context.WithTimeout(context.Background(), clipboardTimeout)
	defer cancel()
	var via []string
	run := func(name string, args ...string) bool {
		if _, err := exec.LookPath(name); err != nil {
			return false
		}
		cmd := exec.CommandContext(ctx, name, args...)
		cmd.Stdin = strings.NewReader(text)
		return cmd.Run() == nil
	}
	switch runtime.GOOS {
	case "darwin":
		if run("pbcopy") {
			via = append(via, "pbcopy")
		}
	case "linux":
		switch {
		case os.Getenv("WAYLAND_DISPLAY") != "" && run("wl-copy"):
			via = append(via, "wl-copy")
		case run("xclip", "-selection", "clipboard"):
			via = append(via, "xclip")
		case run("xsel", "--clipboard", "--input"):
			via = append(via, "xsel")
		}
	case "windows":
		if run("powershell.exe", "-NonInteractive", "-NoProfile", "-Command",
			"[Console]::InputEncoding = [System.Text.Encoding]::UTF8; Set-Clipboard -Value ([Console]::In.ReadToEnd())") {
			via = append(via, "powershell")
		}
	}
	if os.Getenv("TMUX") != "" && run("tmux", "load-buffer", "-") {
		via = append(via, "tmux buffer")
	}
	return via
}

// copyText writes text to the clipboard by every path we have and
// reports back with note as the flash's lead ("copied 3 lines").
func copyText(text, note string) tea.Cmd {
	native := func() tea.Msg {
		return copiedMsg{note: note, via: writeClipboardNative(text)}
	}
	return tea.Batch(tea.SetClipboard(text), native)
}

// copyNote is the flash lead for a copy of text: its size in lines or
// characters.
func copyNote(text string) string {
	if n := strings.Count(text, "\n") + 1; n > 1 {
		return "copied " + plural(n, "line")
	}
	return "copied " + plural(len([]rune(text)), "char")
}

// finishCopy names the paths that took the text. OSC 52 is always
// attempted, so it is listed last as the one we cannot confirm — unless
// TERM names a terminal that has no OSC 52 at all. With no path left,
// the copy failed and the flash says so.
func (m *model) finishCopy(msg copiedMsg) {
	via := msg.via
	if osc52Capable() {
		via = append(via, "OSC 52")
	}
	if len(via) == 0 {
		m.flash = "copy failed · no clipboard tool and the terminal has no OSC 52"
		return
	}
	m.flash = msg.note + " · " + strings.Join(via, " + ")
}

// osc52Capable reports whether the terminal may take an OSC 52 write:
// false for the Linux console and dumb terminals, which have none.
func osc52Capable() bool {
	switch os.Getenv("TERM") {
	case "linux", "dumb":
		return false
	}
	return true
}

// copyFocused copies the raw text of the focused block, else the most
// recent assistant reply. Raw, not rendered: a mouse selection takes
// the hard-wrapped rendering, this takes the markdown the model wrote.
func (m *model) copyFocused() tea.Cmd {
	var text string
	for i := range m.blocks {
		if m.blocks[i].id == m.focusID {
			text = m.blocks[i].text
			break
		}
	}
	if text == "" {
		for i := len(m.blocks) - 1; i >= 0; i-- {
			if m.blocks[i].kind == "assistant" && m.blocks[i].text != "" {
				text = m.blocks[i].text
				break
			}
		}
	}
	if text == "" {
		m.flash = "nothing to copy yet"
		return nil
	}
	return copyText(text, copyNote(text))
}
