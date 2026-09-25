// Package serve is the session control API: a supervisor that owns
// headless bough children and an HTTP/SSE surface over them.
//
// This file is the pure half. Status is DERIVED from the history
// entries on every read and never persisted: history is the only
// record of a session, so a stored status would go stale the moment a
// child died, a turn finished, or the user drove the session from a
// terminal instead.
package serve

import (
	"fmt"
	"regexp"
	"strings"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// jobNoteID reads the id off the loop's job note: "job 7 [exited 1] …".
var jobNoteID = regexp.MustCompile(`^job (\d+) \[`)

// Status is a session's derived state.
type Status string

const (
	StatusIdle        Status = "idle"        // no entries, or nothing but "meta"
	StatusRunning     Status = "running"     // open turn AND a live child
	StatusInterrupted Status = "interrupted" // open turn AND no live child
	StatusNeedsYou    Status = "needs-you"   // unanswered ask
	StatusError       Status = "error"       // last closed turn contained an error entry
	StatusStopped     Status = "stopped"     // last turn closed with "cancelled"
	StatusDone        Status = "done"        // last turn closed with "done", no error in it
)

// Ask is a tools.ask waiting on the user (plugins/ask records kind
// "ask" with {question, options, id}, and "ask/answer" {id, text}).
type Ask struct {
	ID      string   `json:"id"`
	Text    string   `json:"text"`
	Options []string `json:"options"`
	Seq     int64    `json:"seq"`
	Secret  bool     `json:"secret,omitempty"` // tools.secret: the answer is a credential
	// Call is the engine call a native ask or secret blocks; "" for a
	// code-mode ask. Not on the wire: it only says which end is its own.
	Call string `json:"-"`
}

// endsAsk says whether a recorded or live event of kind with data ends
// ask: for a code-mode ask its block's result (tools.ask blocks its
// block), for the engine's native ask or secret the end of its own call.
// On the engine the calls of one reply run at once, so a sibling run_js's
// result, or another call's end, lands while the ask still waits.
func endsAsk(ask *Ask, kind string, data map[string]any) bool {
	switch kind {
	case "result":
		return ask.Call == ""
	case "call":
		if data["phase"] == "start" {
			return false
		}
		t := str(data["tool"])
		return (t == "ask" || t == "secret") && (ask.Call == "" || str(data["id"]) == ask.Call)
	}
	return false
}

// Line is one transcript row on the wire: a history entry with its
// text lifted out of Data, so a client never re-implements EntryText.
type Line struct {
	Seq  int64          `json:"seq"`
	At   time.Time      `json:"at"`
	Kind string         `json:"kind"`
	Text string         `json:"text"`
	Data map[string]any `json:"data,omitempty"` // entry data minus "text"
}

// StatusOf derives a session's status from its entries. childAlive is
// the supervisor's lease: an open turn with no live child is
// interrupted, not running — that distinction is the whole reason the
// caller passes it in. The second return is non-nil only for
// StatusNeedsYou.
//
// The scan mirrors history's own openTurn(): kind "input" opens a
// turn, "done"/"cancelled" close it. There is no turn-level "error"
// kind — a turn that errored still closes with "done" — so the error
// signal is an "error"-kind entry recorded inside the turn by
// loop.note, and it resets on the next input.
func StatusOf(entries []history.Entry, childAlive bool) (Status, *Ask) {
	var (
		open      bool
		errInTurn bool
		lastClose string
		seen      bool // any entry that is not "meta"
		pending   *Ask
	)
	for _, e := range entries {
		if e.Kind != "meta" && e.Kind != "origin" {
			seen = true
		}
		switch e.Kind {
		case "input":
			open = true
			errInTurn = false
			lastClose = ""
		case "error":
			errInTurn = true
		case "ask":
			pending = askOf(e)
		case "ask/answer":
			// Match on the id, never positionally: a turn can arm
			// several asks, and the answers need not interleave.
			if pending != nil && pending.ID == str(e.Data["id"]) {
				pending = nil
			}
		case "ask/end":
			if pending != nil && pending.ID == str(e.Data["id"]) {
				pending = nil
			}
		case "result", "call":
			// The ask returned — answered, timed out or cancelled — and a
			// timeout records no answer: without this the session said
			// "Waiting for you" with live buttons for a question nobody
			// was waiting on. Which entry is its end: endsAsk.
			if pending != nil && endsAsk(pending, e.Kind, e.Data) {
				pending = nil
			}
		case "done", "cancelled":
			// The loop writes a done after every cancel. With the turn
			// already closed by the cancel, that done is bookkeeping, not
			// a finish: the session was stopped, and said "Done".
			if e.Kind == "done" && !open && lastClose == "cancelled" {
				break
			}
			open = false
			lastClose = e.Kind
			pending = nil // a closed turn cannot still be waiting on you
		}
	}

	switch {
	// A question only waits on you while the child that asked it is alive
	// to hear the answer. One left behind by a child that is gone — a
	// crash, a restart, a session from last week — is resolved: an answer
	// would reach a fresh process that never asked, so it falls through
	// to the open turn it interrupted.
	case pending != nil && childAlive:
		return StatusNeedsYou, pending
	case open && childAlive:
		return StatusRunning, nil
	case open:
		return StatusInterrupted, nil
	case errInTurn:
		return StatusError, nil
	case lastClose == "cancelled":
		return StatusStopped, nil
	case lastClose == "done":
		return StatusDone, nil
	case !seen:
		return StatusIdle, nil
	}
	// Entries but no closed turn and no open one: nothing has been
	// asked of this session yet.
	return StatusIdle, nil
}

// askOf reads an Ask out of an "ask" entry. Everything is converted
// defensively: Data comes off disk as JSON, so a field can be absent,
// null, or the wrong type, and a status read must never panic on a
// hand-edited or truncated history file.
func askOf(e history.Entry) *Ask {
	a := &Ask{
		ID:   str(e.Data["id"]),
		Text: str(e.Data["question"]),
		Seq:  e.Seq,
	}
	a.Secret, _ = e.Data["secret"].(bool)
	a.Call = str(e.Data["call"])
	if a.Text == "" {
		a.Text = history.EntryText(e)
	}
	switch opts := e.Data["options"].(type) {
	case []string:
		a.Options = append([]string(nil), opts...)
	case []any:
		for _, o := range opts {
			if s := str(o); s != "" {
				a.Options = append(a.Options, s)
			}
		}
	}
	return a
}

func str(v any) string {
	s, _ := v.(string)
	return s
}

// Transcript returns the entries after sinceSeq, in order, at most
// limit of them (limit <= 0 means all). No kind is dropped: the caller
// decides what a given view shows.
func Transcript(entries []history.Entry, sinceSeq int64, limit int) []Line {
	var out []Line
	var cmds map[string]string // typed job id -> whole command
	for _, e := range entries {
		if e.Seq <= sinceSeq {
			continue
		}
		if limit > 0 && len(out) >= limit {
			break
		}
		// Typed job entries are serve bookkeeping; the transcript shows
		// the loop's text job note for the same event. An engine call
		// adopted as a job (it names its call) has no note: its typed
		// entries are its only record, and the page's Work and the
		// adopting turn's "still running" footer read them by call.
		if _, adopted := e.Data["call"].(string); typedJob(e) && !adopted {
			if c, ok := e.Data["cmd"].(string); ok && c != "" {
				if cmds == nil {
					cmds = map[string]string{}
				}
				cmds[fmt.Sprint(e.Data["id"])] = c
			}
			// A job Stop orb killed queues no notice (no wake-up turn):
			// this entry is its only outcome, so it becomes the note.
			if e.Data["event"] == "finished" && e.Data["stopped"] == true {
				c := str(e.Data["cmd"])
				first, rest, cut := strings.Cut(c, "\n")
				if first = strings.TrimSpace(first); cut && strings.TrimSpace(rest) != "" {
					first += " …"
				}
				out = append(out, Line{Seq: e.Seq, At: e.At, Kind: "job",
					Text: fmt.Sprintf("job %v [stopped with the orb] %s", e.Data["id"], first),
					Data: map[string]any{"cmd": c}})
			}
			continue
		}
		l := Line{Seq: e.Seq, At: e.At, Kind: e.Kind, Text: history.EntryText(e)}
		// The note keeps only "first line …": carry the typed entry's whole command.
		if e.Kind == "job" {
			if m := jobNoteID.FindStringSubmatch(l.Text); m != nil && cmds[m[1]] != "" {
				l.Data = map[string]any{"cmd": cmds[m[1]]}
			}
		}
		// Text is already lifted out; leaving it in Data would double
		// every assistant message on the wire.
		for k, v := range e.Data {
			if k == "text" {
				continue
			}
			if l.Data == nil {
				l.Data = make(map[string]any, len(e.Data))
			}
			l.Data[k] = v
		}
		out = append(out, l)
	}
	return out
}

// LastActivity is the time of the last entry, zero when there are none.
func LastActivity(entries []history.Entry) time.Time {
	if len(entries) == 0 {
		return time.Time{}
	}
	return entries[len(entries)-1].At
}
