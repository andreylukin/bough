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
