package serve

import (
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// A row used to read the whole transcript every time it was built, so a
// list of a few hundred sessions parsed the whole history directory on
// every poll. The digest is read once per (entry count, mtime); these
// pin both halves of that: it is not read again while the file is
// unchanged, and it IS read again once the file moves on.
func TestDigestReadsTheTranscriptOncePerChange(t *testing.T) {
	f := newAPI(t)
	now := time.Now().UTC()
	f.seed(t, "s1",
		history.Entry{Seq: 1, Kind: "input", At: now, Data: map[string]any{"text": "hello"}},
		history.Entry{Seq: 2, Kind: "done", At: now, Data: map[string]any{}},
	)
	infos, err := f.sup.List()
	if err != nil || len(infos) != 1 {
		t.Fatalf("list: %v %d", err, len(infos))
	}
	in := infos[0]

	first := f.api.digest(in)
	again := f.api.digest(in)
	if first != again {
		t.Fatal("an unchanged transcript was digested twice")
	}

	// A new entry is a new file: the digest must follow it.
	f.seed(t, "s1",
		history.Entry{Seq: 1, Kind: "input", At: now, Data: map[string]any{"text": "hello"}},
		history.Entry{Seq: 2, Kind: "done", At: now, Data: map[string]any{}},
		history.Entry{Seq: 3, Kind: "input", At: now, Data: map[string]any{"text": "more"}},
	)
	infos, _ = f.sup.List()
	if got := f.api.digest(infos[0]); got == first {
		t.Fatal("a changed transcript kept its old digest")
	}
}

// Liveness is not in the file, so it must not need a re-read: the digest
// keeps both answers and the row picks one.
func TestDigestKeepsBothLiveAnswers(t *testing.T) {
	now := time.Now().UTC()
	entries := []history.Entry{
		{Seq: 1, Kind: "input", At: now, Data: map[string]any{"text": "hi"}},
	}
	d := digestOf(entries, now)
	if d.statusLive == d.statusDead {
		t.Fatalf("an open turn reads the same alive and dead: %q", d.statusDead)
	}
	if d.jobsLive == nil && RunningJobs(entries, true) != nil {
		t.Fatal("digest dropped the live jobs")
	}
}

// Trouble expires on the clock, so it is computed per request from the
// last entry the digest kept, not frozen into it.
func TestDigestTroubleStillExpires(t *testing.T) {
	now := time.Now().UTC()
	entries := []history.Entry{
		{Seq: 1, Kind: "input", At: now, Data: map[string]any{"text": "hi"}},
		{Seq: 2, Kind: "error", At: now, Data: map[string]any{"text": "boom"}},
	}
	d := digestOf(entries, now)
	if got := d.troubled(StatusError, 0, now); got != "failed" {
		t.Fatalf("fresh failure: %q, want failed", got)
	}
	if got := d.troubled(StatusError, 0, now.Add(troubleWindow)); got != "" {
		t.Fatalf("failure past the window: %q, want none", got)
	}
	// Seen is seen, whatever the clock says.
	if got := d.troubled(StatusError, 2, now); got != "" {
		t.Fatalf("acked failure: %q, want none", got)
	}
}

// LastCache reads the same entry whatever the model; only the TTL is the
// model's. A model meta names but no turn ran must still get the chip.
func TestDigestCacheKeepsItsChipForAnyModel(t *testing.T) {
	now := time.Now().UTC()
	entries := []history.Entry{
		{Seq: 1, Kind: "assistant", At: now, Data: map[string]any{"model": "ran-this-one"}},
		{Seq: 2, Kind: "done", At: now, Data: map[string]any{
			"usage": map[string]any{"cache_read": float64(10), "cache_write": float64(5), "in": float64(100)},
		}},
	}
	d := digestOf(entries, now)
	for _, model := range []string{"ran-this-one", "a-model-no-turn-ran"} {
		c := d.cacheFor(model)
		if c == nil {
			t.Fatalf("model %q lost the cache chip", model)
		}
		if c.Read != 10 || c.Write != 5 {
			t.Fatalf("model %q: %+v", model, c)
		}
	}
}
