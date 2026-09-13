package serve

import (
	"strings"
	"time"
)

// Streaming deltas are the one kind of child output the supervisor
// does not record. A delta is a fragment of a reply that is still
// being written: it exists so a watching browser can show the text
// forming, and it is superseded seconds later by the "assistant" or
// "thinking" line the child records in history. Keeping one would
// mean a late joiner replays half-written prose out of the ring and
// then reads the finished entry again underneath it.
//
// So deltas never enter s.events (the replay ring) and never consume a
// supervisor seq. They are fanned out to whoever is watching right now
// and then forgotten. A browser that connects late gets the finished
// transcript through GET /api/sessions/{id}?since=, untouched.

// deltaWindow is how long a run of deltas is allowed to accumulate
// before it is sent as one event. A provider chunk is not a token, but
// a fast model still produces tens of chunks a second and each one
// would otherwise be a separate SSE frame, a separate JSON parse and a
// separate React render per watching browser. 50ms is under the
// threshold where text stops looking like it is being typed, and it
// collapses a burst into a single frame.
const deltaWindow = 50 * time.Millisecond

func isDelta(kind string) bool {
	return kind == "assistant-delta" || kind == "thinking-delta"
}

// deltaRun is one uninterrupted stretch of a single delta kind.
// Thinking and assistant text interleave within a turn, so the runs
// are kept in arrival order rather than merged into one buffer per
// kind — otherwise a flush would reorder the reply against its own
// reasoning.
type deltaRun struct {
	kind string
	text strings.Builder
}

type deltaState struct {
	runs  []*deltaRun
	timer *time.Timer
}

// bufferDeltaLocked accumulates a delta and arms the flush. Caller
// holds s.mu.
func (s *Supervisor) bufferDeltaLocked(id, kind, text string) {
	if text == "" {
		return
	}
	st := s.deltas[id]
	if st == nil {
		st = &deltaState{}
		s.deltas[id] = st
	}
	if n := len(st.runs); n > 0 && st.runs[n-1].kind == kind {
		st.runs[n-1].text.WriteString(text)
	} else {
		r := &deltaRun{kind: kind}
		r.text.WriteString(text)
		st.runs = append(st.runs, r)
	}
	if st.timer == nil {
		st.timer = time.AfterFunc(deltaWindow, func() {
			s.mu.Lock()
			defer s.mu.Unlock()
			s.flushDeltasLocked(id)
		})
	}
}

// flushDeltasLocked sends whatever has accumulated. It runs both from
// the timer and, synchronously, before any recorded event for the same
// session: a delta that arrived before the "assistant" line must not
// land after it, or the browser would append a duplicate tail to a
// reply it has already finished rendering. Caller holds s.mu.
func (s *Supervisor) flushDeltasLocked(id string) {
	st := s.deltas[id]
	if st == nil {
		return
	}
	if st.timer != nil {
		st.timer.Stop()
	}
	delete(s.deltas, id)
	now := time.Now()
	for _, r := range st.runs {
		// Seq 0 marks the frame as ephemeral: it is not a position in
		// the supervisor's per-session sequence, and writeEvent leaves
		// the SSE id line off entirely so a reconnecting EventSource
		// never resumes from a delta.
		s.fanoutLocked(id, Event{Session: id, Seq: 0, At: now, Kind: r.kind, Text: r.text.String()})
	}
}
