package ui

// ctrl+v with an image on the clipboard saves it to
// ~/.bough/attachments/<timestamp>.png and inserts "@<path> " so the
// "@" attachment path carries it. A text clipboard falls through to
// the textarea's own ctrl+v paste; bracketed paste never comes this
// way. The clipboard probe is an external command, so it runs as a
// tea.Cmd under a short timeout and never blocks the UI.

import (
	"bytes"
	"context"
	"encoding/hex"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
)

const clipboardTimeout = 3 * time.Second

// readClipboardImage returns the clipboard's PNG bytes, nil when it
// holds no image. A package var so tests can stub the clipboard.
var readClipboardImage = clipboardImage

func clipboardImage() []byte {
	ctx, cancel := context.WithTimeout(context.Background(), clipboardTimeout)
	defer cancel()
	var out []byte
	switch runtime.GOOS {
	case "darwin":
		if _, err := exec.LookPath("pngpaste"); err == nil {
			out, _ = exec.CommandContext(ctx, "pngpaste", "-").Output()
		} else {
			raw, _ := exec.CommandContext(ctx, "osascript", "-e", "the clipboard as «class PNGf»").Output()
			out = decodeOsaData(raw)
		}
	case "linux":
		out, _ = exec.CommandContext(ctx, "xclip", "-selection", "clipboard", "-t", "image/png", "-o").Output()
	}
	if !bytes.HasPrefix(out, []byte("\x89PNG")) {
		return nil
	}
	return out
}

// decodeOsaData unwraps osascript's «data PNGf<hex>» rendering.
func decodeOsaData(raw []byte) []byte {
	s := strings.TrimSpace(string(raw))
	s = strings.TrimPrefix(s, "«data PNGf")
	s = strings.TrimSuffix(s, "»")
	b, err := hex.DecodeString(s)
	if err != nil {
		return nil
	}
	return b
}

// saveAttachment writes png under ~/.bough/attachments and returns
// its path.
func saveAttachment(png []byte) (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	dir := filepath.Join(home, ".bough", "attachments")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return "", err
	}
	path := filepath.Join(dir, time.Now().Format("20060102-150405.000")+".png")
	return path, os.WriteFile(path, png, 0o644)
}

// imagePasteMsg delivers the clipboard probe to Update: path is the
// saved image ("" when the clipboard held none), key the ctrl+v to
// replay into the textarea in that case.
type imagePasteMsg struct {
	path string
	err  error
	key  tea.KeyPressMsg
}

// pasteKey probes the clipboard for an image off the UI goroutine.
func (m *model) pasteKey(msg tea.KeyPressMsg) tea.Cmd {
	return func() tea.Msg {
		png := readClipboardImage()
		if png == nil {
			return imagePasteMsg{key: msg}
		}
		path, err := saveAttachment(png)
		return imagePasteMsg{path: path, err: err, key: msg}
	}
}

// finishPaste inserts the "@path " reference, or hands a text
// clipboard's ctrl+v to the textarea.
func (m *model) finishPaste(msg imagePasteMsg) tea.Cmd {
	if msg.err != nil {
		m.flash = "image paste: " + msg.err.Error()
		return nil
	}
	if msg.path == "" {
		_, cmd := m.editKey(msg.key)
		return cmd
	}
	m.input.InsertString("@" + msg.path + " ")
	m.syncPalette()
	m.layoutComposer()
	m.flash = "attached " + msg.path
	return nil
}
