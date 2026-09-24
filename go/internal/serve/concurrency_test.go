package serve

import (
	"context"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// A session whose git read hangs must not hold up the list or any other
// session's reads: nothing on the request path is serialized.
func TestSlowSessionReadDoesNotBlockOthers(t *testing.T) {
	f := newAPI(t)
	// Directories that exist: a cwd that is gone is answered 410 up front.
	slow, fast := t.TempDir(), t.TempDir()
	f.seed(t, "slow", history.Entry{Seq: 1, Kind: "meta", Data: map[string]any{"cwd": slow}})
	f.seed(t, "fast", history.Entry{Seq: 1, Kind: "meta", Data: map[string]any{"cwd": fast}})

	release := make(chan struct{})
	started := make(chan struct{}, 2)
	oldE, oldC := sessionEdits, changesOf
	t.Cleanup(func() { sessionEdits, changesOf = oldE, oldC })
	sessionEdits = func(ctx context.Context, dir string, es []history.Entry) ([]Edit, bool, error) {
		if dir == slow {
			started <- struct{}{}
			<-release
		}
		return nil, true, nil
	}
	changesOf = func(ctx context.Context, dir string) ([]Change, bool) {
		if dir == slow {
			started <- struct{}{}
			<-release
		}
		return nil, true
	}
	defer close(release)

	for _, p := range []string{"/api/sessions/slow/edits", "/api/sessions/slow/changes"} {
		go func() {
			resp, err := f.srv.Client().Get(f.srv.URL + p)
			if err == nil {
				resp.Body.Close()
			}
		}()
	}
	<-started
	<-started

	for _, p := range []string{"/api/sessions", "/api/sessions/fast/edits", "/api/sessions/fast/changes"} {
		done := make(chan int, 1)
		go func() {
			code, _ := f.do(t, "GET", p, "")
			done <- code
		}()
		select {
		case code := <-done:
			if code != http.StatusOK {
				t.Fatalf("GET %s = %d", p, code)
			}
		case <-time.After(3 * time.Second):
			t.Fatalf("GET %s waited on the slow session", p)
		}
	}
}

// The hooks page lists rule files (.md); opening one reads it, while
// anything else outside the pools is still refused.
func TestHookFileReadsListedRules(t *testing.T) {
	f := newAPI(t)
	f.api.home = f.home
	dir := filepath.Join(f.home, ".claude", "rules")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	rule := filepath.Join(dir, "style.md")
	if err := os.WriteFile(rule, []byte("Use tabs.\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	code, body := f.do(t, "GET", "/api/hooks/file?path="+rule, "")
	if code != http.StatusOK || body["body"] != "Use tabs.\n" {
		t.Fatalf("rule read: %d %v", code, body)
	}
	other := filepath.Join(f.home, "notes.md")
	_ = os.WriteFile(other, []byte("x"), 0o644)
	code, body = f.do(t, "GET", "/api/hooks/file?path="+other, "")
	if code != http.StatusBadRequest || !strings.Contains(body["error"].(string), "cannot be edited here") {
		t.Fatalf("unlisted file: %d %v", code, body)
	}
}
