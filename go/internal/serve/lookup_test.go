package serve

import (
	"github.com/andreylukin/bough/plugins/history"
	"net/http"
	"os"
	"runtime"
	"testing"
	"time"
)

func TestOpenSessionDoesNotListUnrelatedHistories(t *testing.T) {
	t.Parallel()
	if runtime.GOOS == "windows" || os.Geteuid() == 0 {
		t.Skip("requires Unix directory permissions")
	}
	f := newAPI(t)
	f.seed(t, "selected", history.Entry{Seq: 1, Kind: "input", At: time.Now(), Data: map[string]any{"text": "hello"}})
	// A known file can be read with directory search permission alone.
	// This pins the absence of a fleet scan without a timing assertion.
	dir := f.sup.HistDir()
	if err := os.Chmod(dir, 0o100); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.Chmod(dir, 0o700) })
	if code, _ := f.do(t, "GET", "/api/sessions", ""); code != http.StatusInternalServerError {
		t.Fatalf("list = %d; permission test ineffective", code)
	}
	code, body := f.do(t, "GET", "/api/sessions/selected", "")
	if code != http.StatusOK {
		t.Fatalf("open = %d, %v", code, body)
	}
	if got := rowOf(t, body)["title"]; got != "hello" {
		t.Fatalf("title = %v", got)
	}
	if code, _ := f.do(t, "GET", "/api/sessions/missing", ""); code != http.StatusNotFound {
		t.Fatalf("missing = %d", code)
	}
	if err := os.Chmod(dir, 0); err != nil {
		t.Fatal(err)
	}
	if code, _ := f.do(t, "GET", "/api/sessions/selected", ""); code != http.StatusInternalServerError {
		t.Fatalf("unsearchable = %d", code)
	}
}
