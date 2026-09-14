package serve

import (
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

func typedJobEntry(seq int64, at time.Time, id float64, event string) history.Entry {
	return history.Entry{Seq: seq, At: at, Kind: "job", Data: map[string]any{"id": id, "event": event, "cmd": "cmd"}}
}

func TestRunningJobsTypedOnly(t *testing.T) {
	t.Parallel()
	now := time.Now()
	es := []history.Entry{
		typedJobEntry(1, now, 1, "started"),
		typedJobEntry(2, now.Add(time.Second), 2, "started"),
		typedJobEntry(3, now.Add(2*time.Second), 1, "finished"),
	}
	got := RunningJobs(es, true)
	if len(got) != 1 || got[0].ID != 2 || got[0].Cmd != "cmd" {
		t.Fatalf("RunningJobs = %+v, want job 2", got)
	}
}

func TestRunningJobsLegacyOnly(t *testing.T) {
	t.Parallel()
	now := time.Now()
	es := []history.Entry{
		{Seq: 1, At: now, Kind: "result", Data: map[string]any{"text": "job 1 started in the background (limit 1m0s): make\nYou will be told"}},
		{Seq: 2, At: now, Kind: "result", Data: map[string]any{"text": "job 2 started in the background (limit 1m0s): sleep 9"}},
		{Seq: 3, At: now, Kind: "job", Data: map[string]any{"text": "job 1 [exited 0] make (1s)"}},
	}
	got := RunningJobs(es, true)
	if len(got) != 1 || got[0].ID != 2 || got[0].Cmd != "sleep 9" {
		t.Fatalf("RunningJobs = %+v, want job 2", got)
	}
}

// Any typed entry means the file is new: the legacy text is ignored,
// or every job would be counted twice.
func TestRunningJobsTypedWinsOverLegacy(t *testing.T) {
	t.Parallel()
	now := time.Now()
	es := []history.Entry{
		{Seq: 1, At: now, Kind: "result", Data: map[string]any{"text": "job 1 started in the background (limit 1m0s): old"}},
		typedJobEntry(2, now, 1, "started"),
		typedJobEntry(3, now, 1, "finished"),
		{Seq: 4, At: now, Kind: "job", Data: map[string]any{"text": "job 1 [exited 0] old (1s)"}},
	}
	if got := RunningJobs(es, true); len(got) != 0 {
		t.Fatalf("RunningJobs = %+v, want none", got)
	}
}

func TestTranscriptSkipsTypedJobs(t *testing.T) {
	t.Parallel()
	now := time.Now()
	es := []history.Entry{
		typedJobEntry(1, now, 1, "started"),
		{Seq: 2, At: now, Kind: "job", Data: map[string]any{"text": "job 1 [exited 0] cmd (1s)"}},
	}
	got := Transcript(es, 0, 0)
	if len(got) != 1 || got[0].Seq != 2 {
		t.Fatalf("Transcript = %+v, want only the text job note", got)
	}
}
