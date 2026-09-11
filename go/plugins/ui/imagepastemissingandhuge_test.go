package ui

// Surface "image-paste-missing-and-huge": an image path pasted into the
// composer whose file is deleted before submit, and one naming a 50MB
// image. A missing file must surface a visible error without losing
// the draft; a huge one must be rejected (or downscaled) with a
// message. Neither may hang. Files are created and removed in-test.

import (
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
)

var (
	imagePasteMissingAndHugeMissingRe = regexp.MustCompile(`(?i)missing|not found|no such|can't read|cannot read|unreadable`)
	imagePasteMissingAndHugeHugeRe    = regexp.MustCompile(`(?i)too (large|big)|exceeds|downscal|resiz|limit`)
)

// imagePasteMissingAndHugeGate skips a subtest pinned on a known bug.
func imagePasteMissingAndHugeGate(t *testing.T, bug string) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_IMAGE_PASTE_MISSING_AND_HUGE") == "" {
		t.Skip("known bug (set BOUGH_KNOWN_IMAGE_PASTE_MISSING_AND_HUGE=1 to run): " + bug)
	}
}

// imagePasteMissingAndHugeFile writes a sparse png of size bytes.
func imagePasteMissingAndHugeFile(t *testing.T, name string, size int64) string {
	t.Helper()
	p := filepath.Join(t.TempDir(), name)
	if err := os.WriteFile(p, []byte("\x89PNG\r\n\x1a\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Truncate(p, size); err != nil {
		t.Fatal(err)
	}
	return p
}

// imagePasteMissingAndHugeTimed fails when fn takes longer than 5 s.
func imagePasteMissingAndHugeTimed(t *testing.T, what string, fn func()) {
	t.Helper()
	done := make(chan struct{})
	go func() { fn(); close(done) }()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatalf("%s hung", what)
	}
}

func TestImagePasteMissingAndHugeDriver(t *testing.T) {
	t.Run("missing file attaches then submit does not hang", func(t *testing.T) {
		img := imagePasteMissingAndHugeFile(t, "shot.png", 64)
		d := defaultDrv(t)
		d.typeStr("look ")
		d.feed(tea.PasteMsg{Content: img})
		if got := d.m.input.Value(); got != "look [Image #1] " {
			t.Fatalf("draft after paste = %q", got)
		}
		if err := os.Remove(img); err != nil {
			t.Fatal(err)
		}
		imagePasteMissingAndHugeTimed(t, "enter on a missing image", func() { d.press(keyEnter()) })
	})

	t.Run("missing file surfaces an error and keeps the draft", func(t *testing.T) {
		img := imagePasteMissingAndHugeFile(t, "shot.png", 64)
		d := defaultDrv(t)
		d.typeStr("look ")
		d.feed(tea.PasteMsg{Content: img})
		if err := os.Remove(img); err != nil {
			t.Fatal(err)
		}
		d.press(keyEnter())
		if len(d.sent) != 0 {
			t.Errorf("a draft naming a deleted image was sent: %q", d.sent)
		}
		if got := d.m.input.Value(); !strings.Contains(got, "look") {
			t.Errorf("draft lost: %q", got)
		}
		// The temp path carries the test name ("...MissingAndHuge..."): not a message.
		if s := strings.ReplaceAll(d.plain(), "MissingAndHuge", ""); !imagePasteMissingAndHugeMissingRe.MatchString(s) {
			t.Errorf("no visible missing-image error:\n%s", s)
		}
	})

	t.Run("huge file paste does not hang", func(t *testing.T) {
		img := imagePasteMissingAndHugeFile(t, "huge.png", 50<<20)
		d := defaultDrv(t)
		imagePasteMissingAndHugeTimed(t, "pasting a 50MB image", func() { d.feed(tea.PasteMsg{Content: img}) })
		imagePasteMissingAndHugeTimed(t, "enter after a 50MB image", func() { d.press(keyEnter()) })
	})

	t.Run("huge file is rejected or downscaled with a message", func(t *testing.T) {
		img := imagePasteMissingAndHugeFile(t, "huge.png", 50<<20)
		d := defaultDrv(t)
		d.feed(tea.PasteMsg{Content: img})
		if s := d.plain(); !imagePasteMissingAndHugeHugeRe.MatchString(s) {
			t.Errorf("no size message after pasting a 50MB image (draft %q, flash %q):\n%s", d.m.input.Value(), d.m.flash, s)
		}
	})
}
