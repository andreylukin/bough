package vtreal

// Surface "image-paste-missing-and-huge" on a real PTY: a pasted image
// path deleted before submit, and a 50MB image path. The turn must
// still complete (nothing hangs); the error and size messages are
// gated on the known bugs.

import (
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"
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

func TestImagePasteMissingAndHugeReal(t *testing.T) {
	t.Parallel()
	t.Run("missing", func(t *testing.T) {
		t.Parallel()
		a := start(t, 100, 30)
		img := imagePasteMissingAndHugeFile(t, "gone.png", 64)
		a.typeText("see ")
		a.term.Paste(img)
		a.waitFor("[Image #1]")
		if err := os.Remove(img); err != nil {
			t.Fatal(err)
		}
		a.key(uv.KeyEnter, 0)
		if os.Getenv("BOUGH_KNOWN_IMAGE_PASTE_MISSING_AND_HUGE") == "" {
			// Today the draft is sent anyway; at least the turn completes.
			a.waitFor("echo: see")
		}
		t.Run("error visible and draft kept", func(t *testing.T) {
			imagePasteMissingAndHugeGate(t, "submit sends a deleted image's tag with no error (plugins/ui/model.go enter -> expandPastes)")
			a.waitUntil(func(s string) bool {
				// The temp path carries the test name ("...MissingAndHuge..."): not a message.
				s = strings.ReplaceAll(s, "MissingAndHuge", "")
				return regexp.MustCompile(`(?i)missing|not found|no such|can't read|cannot read|unreadable`).MatchString(s)
			}, "a missing-image error")
			ls := a.lines()
			if r := composerRow(ls); r < 0 || !strings.Contains(ls[r], "see") {
				t.Errorf("draft lost:\n%s", a.text())
			}
		})
	})
	t.Run("huge", func(t *testing.T) {
		t.Parallel()
		a := start(t, 100, 30)
		img := imagePasteMissingAndHugeFile(t, "huge.png", 50<<20)
		a.term.Paste(img)
		t.Run("size message", func(t *testing.T) {
			imagePasteMissingAndHugeGate(t, "a 50MB image attaches silently (plugins/ui/paste.go attachImage has no size check; plugins/llm/image.go drops it past maxImageBytes)")
			a.waitUntil(func(s string) bool {
				return regexp.MustCompile(`(?i)too (large|big)|exceeds|downscal|resiz|limit`).MatchString(s)
			}, "a size message")
		})
		a.typeText(" hi")
		a.key(uv.KeyEnter, 0)
		a.waitFor("echo:") // nothing hangs: the turn completes
	})
}
