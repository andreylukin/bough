package serve

import (
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/orb"
)

// A failed orb is readable: its error and the tail of resume.log, which the
// web opens from the "failed" chip instead of leaving a dead label.
func TestSessionOrbLog(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	seedModeSession(t, f, "broken", map[string]any{"cwd": "/w", "mode": "project", "project": "app"})
	writeState(t, f.home, orb.State{Session: "broken", Project: "app", Status: orb.StatusFailed, Error: "resume.sh: exit status 1", PID: 1 << 30, UpdatedAt: time.Now()})
	log := strings.Repeat("noise\n", maxLogChunk/6+10) + "error: failed to parse manifest: relative URL without a base\n"
	if err := os.WriteFile(filepath.Join(orb.Dir(f.home, "broken"), "resume.log"), []byte(log), 0o644); err != nil {
		t.Fatal(err)
	}
	code, body := f.do(t, "GET", "/api/sessions/broken/orb/log", "")
	if code != http.StatusOK || body["status"] != "failed" || body["error"] != "resume.sh: exit status 1" {
		t.Fatalf("orb log = %d %v", code, body)
	}
	text, _ := body["text"].(string)
	if len(text) > maxLogChunk || !strings.HasSuffix(text, "relative URL without a base\n") {
		t.Errorf("text is %d bytes, want the capped tail ending in the error", len(text))
	}
	if code, _ := f.do(t, "GET", "/api/sessions/nope/orb/log", ""); code != http.StatusNotFound {
		t.Errorf("unknown session = %d, want 404", code)
	}
}
