package serve

import (
	"fmt"
	"net/http"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// countTurns is how many running-log lines the entries hold.
func countTurns(entries []history.Entry) int {
	n := 0
	for _, e := range entries {
		if e.Kind == "turn-summary" {
			n++
		}
	}
	return n
}

// TurnLine is one line of a session's running log (the session-title
// plugin's "turn-summary" entries).
type TurnLine struct {
	Turn int       `json:"turn"`
	Text string    `json:"text"`
	At   time.Time `json:"at"`
}

func (a *API) turns(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	if _, ok := a.info(id); !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return
	}
	entries, err := a.sup.Entries(id)
	if err != nil {
		writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: read session %q: %w", id, err))
		return
	}
	out := []TurnLine{}
	for _, e := range entries {
		if e.Kind != "turn-summary" {
			continue
		}
		text, _ := e.Data["text"].(string)
		out = append(out, TurnLine{Turn: int(num(e.Data["turn"])), Text: text, At: e.At})
	}
	writeJSON(w, http.StatusOK, map[string]any{"turns": out})
}
