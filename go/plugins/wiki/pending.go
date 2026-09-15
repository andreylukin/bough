package wiki

import (
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strconv"
	"sync"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// Pending is a session with entries the wiki has not seen: those after
// From, up to To.
type Pending struct {
	ID, Path, Title string
	From, To        int64
	Last            time.Time // the session file's last write
}

// ingestRE is the log.md heading an ingest writes per session; the
// highest seq it names is how far that session has been ingested.
var ingestRE = regexp.MustCompile(`(?m)^## \[[^\]]*\] ingest \| ([^#\s|]+)#(\d+)`)

// baselineRE marks when the wiki started: entries older than it count
// as already seen, so the first run does not ingest the whole history.
var baselineRE = regexp.MustCompile(`<!-- baseline: (\S+) -->`)

// readLog returns the highest ingested seq per session and the
// baseline (zero when log.md has none or does not exist).
func readLog(path string) (map[string]int64, time.Time, error) {
	done := map[string]int64{}
	b, err := os.ReadFile(path)
	if errors.Is(err, fs.ErrNotExist) {
		return done, time.Time{}, nil
	}
	if err != nil {
		return nil, time.Time{}, err
	}
	for _, m := range ingestRE.FindAllStringSubmatch(string(b), -1) {
		if n, err := strconv.ParseInt(m[2], 10, 64); err == nil && n > done[m[1]] {
			done[m[1]] = n
		}
	}
	var base time.Time
	if m := baselineRE.FindStringSubmatch(string(b)); m != nil {
		base, _ = time.Parse(time.RFC3339, m[1])
	}
	return done, base, nil
}

// FindPending lists sessions with conversation the wiki has not
// ingested, oldest first. A session is left alone until it has been
// quiet for `quiet` (it is probably still in use), and sessions run
// from inside the wiki directory are the ingests themselves. all
// ignores the baseline, for a backfill.
func FindPending(p paths, quiet time.Duration, all bool, now time.Time) ([]Pending, error) {
	done, base, err := readLog(p.log())
	if err != nil {
		return nil, err
	}
	if all {
		base = time.Time{}
	}
	infos, err := history.List(p.hist)
	if err != nil {
		return nil, err
	}
	var out []Pending
	for _, in := range infos {
		if in.Cwd != "" && sameDir(in.Cwd, p.wiki) {
			continue
		}
		if now.Sub(in.ModTime) < quiet {
			continue
		}
		marks, ok := marksOf(in)
		if !ok {
			continue
		}
		from := done[in.ID]
		var to int64
		talk := false
		for _, e := range marks {
			to = max(to, e.seq)
			if !base.IsZero() && e.at.Before(base) {
				from = max(from, e.seq) // before the wiki existed: seen
				continue
			}
			if e.seq > from && e.talk {
				talk = true
			}
		}
		if !talk {
			continue
		}
		out = append(out, Pending{ID: in.ID, Path: in.Path, Title: in.Title, From: from, To: to, Last: in.ModTime})
	}
	slices.SortFunc(out, func(a, b Pending) int { return a.Last.Compare(b.Last) })
	return out, nil
}

// mark is the little of an entry FindPending reads.
type mark struct {
	seq  int64
	at   time.Time
	talk bool
}

type marked struct {
	size  int64
	mod   time.Time
	marks []mark
}

var (
	marksMu    sync.Mutex
	marksCache = map[string]marked{}
)

// marksOf reads a session's marks, cached on the file's size and mtime:
// the index asks on every load, and most sessions never change again.
func marksOf(in history.SessionInfo) ([]mark, bool) {
	st, err := os.Stat(in.Path)
	if err != nil {
		return nil, false
	}
	marksMu.Lock()
	c, hit := marksCache[in.Path]
	marksMu.Unlock()
	if hit && c.size == st.Size() && c.mod.Equal(st.ModTime()) {
		return c.marks, true
	}
	entries, err := history.Read(in.Path)
	if err != nil {
		return nil, false
	}
	marks := make([]mark, len(entries))
	for i, e := range entries {
		marks[i] = mark{seq: e.Seq, at: e.At, talk: e.Kind == "input" || e.Kind == "assistant"}
	}
	marksMu.Lock()
	marksCache[in.Path] = marked{size: st.Size(), mod: st.ModTime(), marks: marks}
	marksMu.Unlock()
	return marks, true
}

func sameDir(a, b string) bool {
	clean := func(s string) string {
		if r, err := filepath.EvalSymlinks(s); err == nil {
			s = r
		}
		return filepath.Clean(s)
	}
	return clean(a) == clean(b)
}
