package serve

import (
	"sync"
	"time"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

// Every /api/sessions builds a row per session, and every row read that
// session's whole transcript. With a few hundred sessions that is the
// entire history directory parsed on each poll — the sidebar took about
// nine seconds to fill against 182 MB of them.
//
// A finished transcript derives the same row for ever, so the values a
// row takes from one is computed once per (size, mtime) and kept.
// history.List already does this for the fields IT reads; this is the
// same bargain for the rest of the row.
//
// What a row also takes from outside the file — whether the child is
// alive, the session's meta, the clock, the orb — stays out of the
// digest and is applied per request. The two places the file and the
// liveness meet (status and jobs) keep both answers, so liveness
// flipping never needs a re-read.

type rowDigest struct {
	statusDead  Status
	askDead     *Ask
	statusLive  Status
	askLive     *Ask
	jobsLive    []Job
	model       string
	ownModel    string // ownModel's answer
	switched    bool
	mode        string
	project     string
	lastAt      time.Time
	testsFailed bool
	testsAt     *time.Time
	turns       int
	hasInput    bool
	// LastCache reads the same "done" entry whatever the model; only the
	// TTL it reports is the model's. So the entry is found once and the
	// TTL applied per row, which also keeps a model meta names but no
	// turn ran from losing the chip.
	cacheBase *Cache

	// The last entry that is not the turn's trailing summary or title,
	// which is all troubled needs of the transcript.
	lastSeq     int64
	lastEntryAt time.Time
	empty       bool
	// The last turn's close. Unseen compares the ack against it rather
	// than the last entry, since a title or a notice written after the
	// person looked must not bring the dot back.
	doneSeq int64
	doneAt  time.Time
}

func digestOf(entries []history.Entry, fallback time.Time) *rowDigest {
	sd, ad := StatusOf(entries, false)
	sl, al := StatusOf(entries, true)
	mode, project := sessionMode(entries)
	d := &rowDigest{
		statusDead: sd, askDead: ad,
		statusLive: sl, askLive: al,
		jobsLive:    RunningJobs(entries, true),
		model:       lastModel(entries),
		mode:        mode,
		project:     project,
		lastAt:      lastAt(entries, fallback),
		testsFailed: lastTestFailed(entries),
		testsAt:     testsAt(entries),
		turns:       countTurns(entries),
		hasInput:    hasInput(entries),
		empty:       len(entries) == 0,
	}
	d.ownModel, d.switched = ownModel(entries)
	// The turn's summary and title are written after its done, a
	// small-model call later: a Mark seen clicked on the failure in
	// between was taken back when they landed. They are the turn already
	// looked at, so trouble compares the ack with the entry before them.
	for i := len(entries) - 1; i >= 0; i-- {
		if k := entries[i].Kind; k != "turn-summary" && k != "title" {
			d.lastSeq, d.lastEntryAt = entries[i].Seq, entries[i].At
			break
		}
	}
	for i := len(entries) - 1; i >= 0; i-- {
		if entries[i].Kind == "done" {
			d.doneSeq, d.doneAt = entries[i].Seq, entries[i].At
			break
		}
	}
	d.cacheBase = LastCache(entries, "")
	return d
}

// cacheFor is the digest's cache entry with this model's TTL.
func (d *rowDigest) cacheFor(model string) *Cache {
	if d.cacheBase == nil {
		return nil
	}
	c := *d.cacheBase
	c.TTL = int(llm.CacheTTL(model) / time.Second)
	return &c
}

// troubled is Troubled with the last entry the digest kept, so a row
// still expires its own trouble on the clock without the transcript.
func (d *rowDigest) troubled(st Status, ack int64, now time.Time, background bool) string {
	window := troubleWindow
	if background {
		window = backgroundTroubleWindow
	}
	if d.empty || d.lastSeq <= ack || now.Sub(d.lastEntryAt) >= window {
		return ""
	}
	switch {
	case st == StatusError:
		return "failed"
	case st == StatusInterrupted:
		return "interrupted"
	case d.testsFailed:
		return "tests failed"
	}
	return ""
}

// unseen reports a turn that finished cleanly after the person last
// marked the session seen: "done" alone does not say whether anyone saw
// it end. Only web sessions: serve is where a person watches those, and a
// terminal session ended in front of whoever ran it. Trouble has its own
// marks, so a failed or interrupted finish is never also unseen; the
// window is trouble's, so the first load after this shipped does not
// flag every finish ever recorded.
func (d *rowDigest) unseen(st Status, trouble string, ack int64, now time.Time, origin string) bool {
	return origin == "web" && st == StatusDone && trouble == "" &&
		d.doneSeq > ack && now.Sub(d.doneAt) < troubleWindow
}

// digests holds one digest per session, valid while the file it was read
// from has the same size and mtime. Unbounded on purpose: it holds one
// small struct per session, not the transcripts, and the process already
// keeps a row per session in flight.
var digests struct {
	sync.Mutex
	m map[string]digestEntry
}

type digestEntry struct {
	entries int
	mod     time.Time
	d       *rowDigest
}

// digest returns the session's digest, reading the transcript only when
// the file changed since the last one.
func (a *API) digest(in history.SessionInfo) *rowDigest {
	digests.Lock()
	e, hit := digests.m[in.Path]
	digests.Unlock()
	if hit && e.entries == in.Entries && e.mod.Equal(in.ModTime) {
		return e.d
	}
	entries, err := a.sup.Entries(in.ID)
	if err != nil {
		entries = nil
	}
	d := digestOf(entries, in.ModTime)
	digests.Lock()
	if digests.m == nil {
		digests.m = map[string]digestEntry{}
	}
	digests.m[in.Path] = digestEntry{entries: in.Entries, mod: in.ModTime, d: d}
	digests.Unlock()
	return d
}
