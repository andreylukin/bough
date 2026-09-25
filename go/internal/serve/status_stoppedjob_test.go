package serve

import (
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// A job killed by Stop orb queues no notice, so its typed entry is the
// only record: the transcript turns it into a stopped job note.
func TestTranscriptStoppedJobNote(t *testing.T) {
	t.Parallel()
	now := time.Now()
	lines := Transcript([]history.Entry{
		{Seq: 1, At: now, Kind: "job", Data: map[string]any{"id": float64(2), "event": "started", "cmd": "bun run dev\n--watch"}},
		{Seq: 2, At: now, Kind: "job", Data: map[string]any{"id": float64(2), "event": "finished", "cmd": "bun run dev\n--watch", "exit": float64(137), "stopped": true}},
		{Seq: 3, At: now, Kind: "job", Data: map[string]any{"id": float64(3), "event": "finished", "cmd": "x", "exit": float64(1)}},
	}, 0, 0)
	if len(lines) != 1 {
		t.Fatalf("lines = %+v", lines)
	}
	if l := lines[0]; l.Kind != "job" || l.Text != "job 2 [stopped with the orb] bun run dev …" || l.Data["cmd"] != "bun run dev\n--watch" {
		t.Fatalf("line = %+v", l)
	}
}

// An engine call adopted as a job has no loop note: its typed entries
// are its only record, and the page's Work and the adopting turn's
// footer read them by call id. Dropped, a finished adopted job vanished
// from Work and the footer said "1 call still running" after serve
// recorded it stopped at the child's reap.
func TestTranscriptKeepsAdoptedCallJobs(t *testing.T) {
	t.Parallel()
	now := time.Now()
	lines := Transcript([]history.Entry{
		{Seq: 1, At: now, Kind: "job", Data: map[string]any{"id": float64(1), "event": "started", "call": "c1", "text": "job 1: sleep 9"}},
		{Seq: 2, At: now, Kind: "job", Data: map[string]any{"id": float64(1), "event": "finished", "call": "c1", "exit": float64(0)}},
		{Seq: 3, At: now, Kind: "job", Data: map[string]any{"id": float64(2), "event": "started", "call": "c2"}},
		{Seq: 4, At: now, Kind: "job", Data: map[string]any{"id": float64(2), "event": "finished", "call": "c2", "cmd": "make", "stopped": true}},
	}, 0, 0)
	if len(lines) != 4 {
		t.Fatalf("lines = %+v", lines)
	}
	for i, l := range lines {
		if l.Kind != "job" || l.Data["event"] == nil || l.Data["call"] == nil || l.Data["id"] == nil {
			t.Errorf("line %d = %+v, want the typed entry with its id, event and call", i, l)
		}
	}
	if l := lines[3]; l.Data["stopped"] != true {
		t.Errorf("stopped finish = %+v, want stopped kept", l)
	}
}
