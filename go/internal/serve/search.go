package serve

// Full-text search across every transcript.
//
// The sidebar box matched titles only, which is a poor promise: a title
// is written by a small model after the first turn, and what you
// actually remember is something that was SAID — a file name, an error,
// a command. plugins/history already searches the bodies; this exposes
// it so the palette can reach it.

import (
	"net/http"
	"strconv"

	"github.com/andreylukin/bough/plugins/history"
)

// searchLimit bounds a palette query. Someone scanning results reads
// the first handful; searching 111MB for the hundredth is waste.
const searchLimit = 25

// SearchHit is one session that matched, with the lines that matched.
type SearchHit struct {
	ID     string       `json:"id"`
	Title  string       `json:"title"`
	Repo   string       `json:"repo"`
	Branch string       `json:"branch"`
	Hits   int          `json:"hits"`
	Lines  []SearchLine `json:"lines"`
}

// SearchLine is one matching entry, trimmed for display.
type SearchLine struct {
	Seq  int64  `json:"seq"`
	Kind string `json:"kind"`
	Text string `json:"text"`
}

// search answers the palette. An empty or unparseable query is an empty
// result, not an error: the box is typed into a character at a time.
func (a *API) search(w http.ResponseWriter, r *http.Request) {
	raw := r.URL.Query().Get("q")
	limit := searchLimit
	if n, err := strconv.Atoi(r.URL.Query().Get("limit")); err == nil && n > 0 && n <= 100 {
		limit = n
	}
	q, err := history.ParseQuery(raw)
	if err != nil || (len(q.Terms) == 0 && q.Repo == "" && q.Branch == "") {
		writeJSON(w, http.StatusOK, map[string]any{"hits": []SearchHit{}})
		return
	}
	matches, err := history.Search(a.sup.HistDir(), q, limit)
	if err != nil {
		writeErr(w, http.StatusInternalServerError, err)
		return
	}
	out := make([]SearchHit, 0, len(matches))
	for _, m := range matches {
		lines := make([]SearchLine, 0, len(m.Lines))
		for _, l := range m.Lines {
			lines = append(lines, SearchLine{Seq: l.Seq, Kind: l.Kind, Text: l.Text})
		}
		out = append(out, SearchHit{
			ID: m.ID, Title: m.Title, Repo: m.Repo, Branch: m.Branch,
			Hits: m.Hits, Lines: lines,
		})
	}
	writeJSON(w, http.StatusOK, map[string]any{"hits": out})
}
