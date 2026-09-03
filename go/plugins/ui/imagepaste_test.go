package ui

// ctrl+v image paste (imagepaste.go) with a stubbed clipboard.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

func TestImagePasteAttaches(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	png := []byte("\x89PNG\r\n\x1a\nfake")
	readClipboardImage = func() []byte { return png }
	t.Cleanup(func() { readClipboardImage = clipboardImage })

	d := defaultDrv(t)
	d.typeStr("what is this ")
	d.press(keyCtrl('v'))
	got := d.m.input.Value()
	dir := filepath.Join(home, ".bough", "attachments")
	if !strings.HasPrefix(got, "what is this @"+dir+"/") || !strings.HasSuffix(got, ".png ") {
		t.Fatalf("draft = %q, want an @%s/<ts>.png reference", got, dir)
	}
	path := strings.TrimSuffix(strings.TrimPrefix(got, "what is this @"), " ")
	data, err := os.ReadFile(path)
	if err != nil || string(data) != string(png) {
		t.Fatalf("attachment file: %v %q", err, data)
	}
	// The model cannot take pixels yet, so the flash must not claim
	// the image was attached.
	if !strings.Contains(d.m.flash, "image saved") || strings.Contains(d.m.flash, "attached") {
		t.Errorf("flash = %q", d.m.flash)
	}
}

// A keymap binding on ctrl+v wins over the clipboard probe.
func TestImagePasteYieldsToKeymap(t *testing.T) {
	probed := false
	readClipboardImage = func() []byte { probed = true; return []byte("\x89PNG") }
	t.Cleanup(func() { readClipboardImage = clipboardImage })
	d := newDrv(t, 80, 24, cfgWith(t, nil, map[string]string{"scroll_down": "ctrl+v"}, nil))
	d.typeStr("plain ")
	d.press(keyCtrl('v'))
	if probed || d.m.input.Value() != "plain " {
		t.Errorf("ctrl+v bound to scroll_down: probed=%v draft=%q", probed, d.m.input.Value())
	}
}

func TestImagePasteNoImageFallsThrough(t *testing.T) {
	readClipboardImage = func() []byte { return nil }
	t.Cleanup(func() { readClipboardImage = clipboardImage })
	d := defaultDrv(t)
	d.typeStr("plain ")
	next, cmd := d.m.Update(keyCtrl('v'))
	d.m = next.(model)
	msg := cmd()
	ip, ok := msg.(imagePasteMsg)
	if !ok || ip.path != "" {
		t.Fatalf("probe should report no image, got %#v", msg)
	}
	// The key replays into the textarea, whose own ctrl+v is the text
	// paste (a clipboard read cmd); the draft is untouched.
	d.feed(ip)
	if d.m.input.Value() != "plain " {
		t.Errorf("draft = %q", d.m.input.Value())
	}
	// Bracketed paste never touches the probe.
	d.feed(tea.PasteMsg{Content: "x\ny"})
	if d.m.input.Value() != "plain x\ny" {
		t.Errorf("bracketed paste broke: %q", d.m.input.Value())
	}
}

func TestDecodeOsaData(t *testing.T) {
	if got := decodeOsaData([]byte("«data PNGf89504E47»\n")); string(got) != "\x89PNG" {
		t.Errorf("decode = %q", got)
	}
	if decodeOsaData([]byte("zz")) != nil {
		t.Error("garbage should decode to nil")
	}
}
