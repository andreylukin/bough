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
	"strings"

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
	text, status := paletteQuery(raw)
	q, err := history.ParseQuery(text)
	if err != nil || (len(q.Terms) == 0 && q.Repo == "" && q.Branch == "" && q.Since.IsZero() && status == "") {
		writeJSON(w, http.StatusOK, map[string]any{"hits": []SearchHit{}})
		return
	}
	// status: is the row's state, which history does not know: search
	// uncapped, then keep the ones in that state up to the limit.
	capAt := limit
	var want map[string]bool
	if status != "" {
		capAt = 0
		want = a.idsInStatus(status)
	}
	matches, err := history.Search(a.sup.HistDir(), q, capAt)
	if err != nil {
		writeErr(w, http.StatusInternalServerError, err)
		return
	}
	out := make([]SearchHit, 0, len(matches))
	for _, m := range matches {
		if want != nil && !want[m.ID] {
			continue
		}
		if len(out) >= limit {
			break
		}
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

// paletteQuery reads the palette's operators into history's: project:
// is repo:, after: is since:, and status: is taken out and returned,
// since it is a session state and not text. A status word may be the
// one the UI shows ("failed", "waiting") or the API's.
func paletteQuery(raw string) (text string, status string) {
	var keep []string
	for _, f := range strings.Fields(raw) {
		name, val, ok := strings.Cut(f, ":")
		if !ok || val == "" {
			keep = append(keep, f)
			continue
		}
		switch strings.ToLower(name) {
		case "project":
			keep = append(keep, "repo:"+val)
		case "after":
			keep = append(keep, "since:"+val)
		case "status":
			status = statusAlias(strings.ToLower(val))
		default:
			keep = append(keep, f)
		}
	}
	return strings.Join(keep, " "), status
}

func statusAlias(v string) string {
	switch v {
	case "failed", "error":
		return string(StatusError)
	case "waiting", "needs-you":
		return string(StatusNeedsYou)
	}
	return v
}

// idsInStatus is the set of sessions whose row is in status.
func (a *API) idsInStatus(status string) map[string]bool {
	ids := map[string]bool{}
	infos, err := a.sup.List()
	if err != nil {
		return ids
	}
	for _, in := range infos {
		if string(a.row(in).Status) == status {
			ids[in.ID] = true
		}
	}
	return ids
}
