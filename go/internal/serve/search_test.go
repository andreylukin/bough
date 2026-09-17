package serve

import (
	"net/http"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// A title is written by a small model after the first turn; what a
// person remembers is something that was said. Searching titles only
// is the promise the sidebar box made and could not keep.
func TestSearchFindsWordsInTheBody(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	now := time.Now()
	f.seed(t, "01a00000-0000-7000-8000-00000000f001",
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": f.home}},
		history.Entry{Seq: 2, At: now, Kind: "input", Data: map[string]any{"text": "a dull title"}},
		history.Entry{Seq: 3, At: now, Kind: "assistant", Data: map[string]any{
			"text": "the deploy failed with ENOSPC on the runner"}},
		history.Entry{Seq: 4, At: now, Kind: "done", Data: nil},
	)

	code, body := f.do(t, "GET", "/api/search?q=ENOSPC", "")
	if code != http.StatusOK {
		t.Fatalf("GET /api/search = %d", code)
	}
	hits, _ := body["hits"].([]any)
	if len(hits) != 1 {
		t.Fatalf("hits = %d, want the session whose BODY matched (%v)", len(hits), body["hits"])
	}
	h, _ := hits[0].(map[string]any)
	lines, _ := h["lines"].([]any)
	if len(lines) == 0 {
		t.Fatal("no matching line returned: the result cannot say why it is here")
	}
}

// The box is typed into one character at a time, and this reads every
// transcript — an empty or junk query must cost nothing and return a
// list, never an error.
func TestSearchEmptyQueryIsEmptyList(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	for _, q := range []string{"", "%20%20", "repo%3A"} {
		code, body := f.do(t, "GET", "/api/search?q="+q, "")
		if code != http.StatusOK {
			t.Errorf("q=%q = %d, want 200", q, code)
		}
		if _, ok := body["hits"].([]any); !ok {
			t.Errorf("q=%q: hits is not a list: %#v", q, body["hits"])
		}
	}
}

// The palette's operators: project: narrows by repo, after: by last
// activity, status: by the session's state. None of them is a word to
// find in the text, and each works without any words at all.
func TestSearchOperators(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	now := time.Now()
	const alpha, beta = "01a00000-0000-7000-8000-00000000f0a1", "01a00000-0000-7000-8000-00000000f0b2"
	f.seed(t, alpha,
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": f.home + "/alpha"}},
		history.Entry{Seq: 2, At: now, Kind: "input", Data: map[string]any{"text": "fix ENOSPC"}},
		history.Entry{Seq: 3, At: now, Kind: "done", Data: nil},
	)
	f.seed(t, beta,
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": f.home + "/beta"}},
		history.Entry{Seq: 2, At: now, Kind: "input", Data: map[string]any{"text": "fix ENOSPC"}},
		history.Entry{Seq: 3, At: now, Kind: "cancelled", Data: nil},
	)
	old := now.AddDate(0, 0, -30)
	if err := os.Chtimes(filepath.Join(f.hist, beta+".jsonl"), old, old); err != nil {
		t.Fatal(err)
	}
	for q, want := range map[string]string{
		"ENOSPC+project%3Aalpha": alpha,
		"ENOSPC+after%3A7d":      alpha,
		"status%3Astopped":       beta,
		"ENOSPC+status%3Adone":   alpha,
		"project%3Abeta":         beta,
	} {
		code, body := f.do(t, "GET", "/api/search?q="+q, "")
		if code != http.StatusOK {
			t.Errorf("q=%s = %d", q, code)
			continue
		}
		hits, _ := body["hits"].([]any)
		if len(hits) != 1 || hits[0].(map[string]any)["id"] != want {
			t.Errorf("q=%s: hits = %v, want only %s", q, hits, want)
		}
	}
}
