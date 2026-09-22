package tools

import (
	"strconv"
	"strings"
	"time"

	"github.com/andreylukin/bough/internal/hookmeta"
)

// Per-call events. A code block is one thing to the loop (a "code"
// entry, then its "result"), but to the person watching it is the
// commands it ran and the files it read and changed, one after
// another. Every foreground tools.bash/view/write/patch announces
// itself as it starts (a live "call" event with phase "start", never
// recorded) and records itself as it returns (a "call" history entry:
// tool, what it did, how long it took, its exit or error, an edit's
// added and removed lines). Inside a subagent the kind is "sub:call",
// like the rest of a child's mirrored activity. DefaultProject ignores
// the kind, so the model never sees these; only the UI does.

// callSink is where a call's events go: emit sends the live event,
// and record (when true) also appends it to the session history.
type callSink func(kind, text string, data map[string]any, record bool)

// callEnd closes a call: err is what the tool returned, extra is the
// tool's own evidence (exit, add, del).
type callEnd func(err error, extra map[string]any)

// call opens a per-call event for tool, described by detail. It is a
// no-op (returns a no-op end) when no sink is wired, or when the call
// is a hook's: a hook is the user's script running on the host, not a
// step of the block.
func (s *Stats) call(tool, detail string) callEnd {
	if s.callSink == nil || (s.runCtx != nil && hookmeta.OnHost(s.runCtx())) {
		return func(error, map[string]any) {}
	}
	s.mu.Lock()
	s.calls++
	id := s.calls
	s.mu.Unlock()
	kind, worker := "call", 0
	if s.subWorker != nil {
		if worker = s.subWorker(); worker > 0 {
			kind = "sub:call"
		}
	}
	started := time.Now()
	start := map[string]any{"tool": tool, "id": id, "phase": "start"}
	if worker > 0 {
		start["worker"] = worker
	}
	s.callSink(kind, detail, start, false)
	return func(err error, extra map[string]any) {
		data := map[string]any{"tool": tool, "id": id, "ms": time.Since(started).Milliseconds()}
		if worker > 0 {
			data["worker"] = worker
		}
		for k, v := range extra {
			data[k] = v
		}
		if err != nil {
			data["error"] = strings.TrimPrefix(err.Error(), tool+": ")
		}
		s.callSink(kind, detail, data, true)
	}
}

// diffCounts counts the added and removed lines of a lineDiff.
func diffCounts(diff string) (add, del int) {
	for _, l := range strings.Split(diff, "\n") {
		switch {
		case strings.HasPrefix(l, "+"):
			add++
		case strings.HasPrefix(l, "-"):
			del++
		}
	}
	return add, del
}

// viewDetail is "path", "path:start" or "path:start-end".
func viewDetail(path string, rng []int) string {
	switch len(rng) {
	case 0:
		return path
	case 1:
		return path + ":" + strconv.Itoa(rng[0])
	default:
		return path + ":" + strconv.Itoa(rng[0]) + "-" + strconv.Itoa(rng[1])
	}
}
