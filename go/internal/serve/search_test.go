package serve

import (
	"net/http"
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
