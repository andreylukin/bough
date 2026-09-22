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

// A call-delta is a running native call's live output (the engine's
// boughcall Progress). It is a fragment the recorded "call" entry
// supersedes, exactly like reply text, so it takes the same path.
func isDelta(kind string) bool {
	return kind == "assistant-delta" || kind == "thinking-delta" || kind == "call-delta"
}

// deltaRun is one uninterrupted stretch of a single delta kind.
// Thinking and assistant text interleave within a turn, so the runs
// are kept in arrival order rather than merged into one buffer per
// kind — otherwise a flush would reorder the reply against its own
// reasoning. A call-delta run also belongs to one call (call): with
// several calls running at once their output interleaves, and merged
// it would land under whichever row the first fragment named.
type deltaRun struct {
	kind string
	call string
	text strings.Builder
}

// deltaCall is the call a call-delta belongs to; "" for reply text.
func deltaCall(kind string, extra map[string]any) string {
	if kind != "call-delta" {
		return ""
	}
	v, _ := extra["id"].(string)
	return v
}

// dropTextDeltasLocked discards buffered reply text that a
// "delta-reset" says was superseded (a retry, or a newer request): sent
// now, the browser would draw it only to clear it on the next frame.
// Call output is unaffected. Caller holds s.mu.
func (s *Supervisor) dropTextDeltasLocked(id string) {
	st := s.deltas[id]
	if st == nil {
		return
	}
	kept := st.runs[:0]
	for _, r := range st.runs {
		if r.kind == "call-delta" {
			kept = append(kept, r)
		}
	}
	st.runs = kept
}

type deltaState struct {
	runs  []*deltaRun
	timer *time.Timer
}

// bufferDeltaLocked accumulates a delta and arms the flush. Caller
// holds s.mu.
func (s *Supervisor) bufferDeltaLocked(id, kind, text string, extra map[string]any) {
	if text == "" {
		return
	}
	st := s.deltas[id]
	if st == nil {
		st = &deltaState{}
		s.deltas[id] = st
	}
	call := deltaCall(kind, extra)
	if n := len(st.runs); n > 0 && st.runs[n-1].kind == kind && st.runs[n-1].call == call {
		st.runs[n-1].text.WriteString(text)
	} else {
		r := &deltaRun{kind: kind, call: call}
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
		ev := Event{Session: id, Seq: 0, At: now, Kind: r.kind, Text: r.text.String()}
		if r.kind == "call-delta" {
			ev.Extra = map[string]any{"id": r.call}
		}
		s.fanoutLocked(id, ev)
	}
}
