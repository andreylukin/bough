package serve

import (
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/orb"
)

// A session waiting on its project's image build can read the build log
// live, from an offset, and starts over when a newer build truncates it.
func TestSessionBuildLog(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	seedModeSession(t, f, "waiting", map[string]any{"cwd": "/w", "mode": "project", "project": "app"})
	writeState(t, f.home, orb.State{Session: "waiting", Project: "app", Status: orb.StatusBuilding, PID: 1 << 30, UpdatedAt: time.Now()})
	logPath := orb.ImageLogPath(f.home, "app")
	if err := os.MkdirAll(filepath.Dir(logPath), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(logPath, []byte("#1 apt-get update\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	started := time.Now().Add(-90 * time.Second).UTC().Truncate(time.Second)
	if err := os.WriteFile(filepath.Join(filepath.Dir(logPath), "build.json"), []byte(`{"tag":"t","hash":"h","state":"building","startedAt":"`+started.Format(time.RFC3339)+`"}`), 0o644); err != nil {
		t.Fatal(err)
	}
	code, body := f.do(t, "GET", "/api/sessions/waiting/orb/build/log?offset=0", "")
	if code != http.StatusOK || body["text"] != "#1 apt-get update\n" || body["status"] != "building" {
		t.Fatalf("first read = %d %v", code, body)
	}
	// The web's timer counts from the build's start; no end while building.
	if got, _ := time.Parse(time.RFC3339, fmt.Sprint(body["startedAt"])); !got.Equal(started) || body["endedAt"] != nil {
		t.Errorf("startedAt = %v, endedAt = %v; want %v and none", body["startedAt"], body["endedAt"], started)
	}
	off := int(body["offset"].(float64))

	fh, err := os.OpenFile(logPath, os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		t.Fatal(err)
	}
	fh.WriteString("#2 DONE 12.0s\n")
	fh.Close()
	_, body = f.do(t, "GET", "/api/sessions/waiting/orb/build/log?offset="+itoa(off), "")
	if body["text"] != "#2 DONE 12.0s\n" {
		t.Errorf("append read = %v, want only the new line", body["text"])
	}

	os.WriteFile(logPath, []byte("new\n"), 0o644) // a newer build truncated it
	_, body = f.do(t, "GET", "/api/sessions/waiting/orb/build/log?offset=999", "")
	if body["text"] != "new\n" {
		t.Errorf("after truncation = %v, want the new log from the start", body["text"])
	}
	if code, _ := f.do(t, "GET", "/api/sessions/waiting/orb/build/log?offset=-1", ""); code != http.StatusBadRequest {
		t.Errorf("negative offset = %d, want 400", code)
	}
}

func itoa(n int) string {
	b := []byte{}
	if n == 0 {
		return "0"
	}
	for ; n > 0; n /= 10 {
		b = append([]byte{byte('0' + n%10)}, b...)
	}
	return string(b)
}
