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
	// Test is the turn's last recorded test command and its exit, when it ran one.
	Test *TurnTest `json:"test,omitempty"`
}

// TurnTest is a test command a turn ran and the exit its result recorded.
type TurnTest struct {
	Cmd  string `json:"cmd"`
	Exit int    `json:"exit"`
}

// turnTests maps each turn (counted by its "done") to its last test run.
func turnTests(entries []history.Entry) map[int]*TurnTest {
	out := map[int]*TurnTest{}
	turn := 1
	for i, e := range entries {
		switch {
		case e.Kind == "done":
			turn++
		case e.Kind == "result" && i > 0 && entries[i-1].Kind == "code":
			code, _ := entries[i-1].Data["text"].(string)
			exit, ok := e.Data["exit"].(float64)
			if n, isInt := e.Data["exit"].(int); isInt {
				exit, ok = float64(n), true
			}
			if ok && testCmd.MatchString(code) {
				out[turn] = &TurnTest{Cmd: testCmd.FindString(code), Exit: int(exit)}
			}
		}
	}
	return out
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
	tests := turnTests(entries)
	for _, e := range entries {
		if e.Kind != "turn-summary" {
			continue
		}
		text, _ := e.Data["text"].(string)
		out = append(out, TurnLine{Turn: int(num(e.Data["turn"])), Text: text, At: e.At, Test: tests[int(num(e.Data["turn"]))]})
	}
	writeJSON(w, http.StatusOK, map[string]any{"turns": out})
}
