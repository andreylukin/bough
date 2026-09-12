package history

// Search across every stored session.
//
// Session pickers are conventionally scoped to the working directory,
// which assumes the directory identifies the work. Started from a home
// directory holding many repos, that assumption fails: every session
// records the same cwd, so the scope tells them apart not at all. What
// does tell them apart is what was said and done in them, so this
// searches the transcripts themselves — the kind of query people
// actually have ("the migration I abandoned last week") is words plus
// a time bound, not a path.
//
// Two failure modes in shipped agents are avoided deliberately: a
// filter that only narrows the page already on screen, and a hard cap
// of N most recent matches. Search reads the whole corpus and the
// caller sets the limit. It can afford to: the corpus is JSONL text,
// and a full scan of a year of sessions is a few tens of milliseconds,
// which is why there is no index to build, invalidate, or corrupt.

import (
	"cmp"
	"fmt"
	"slices"
	"strconv"
	"strings"
	"time"
)

// Query is a parsed search: free-text terms every match must contain,
// plus optional attribute filters. The zero Query matches everything.
type Query struct {
	// Terms must ALL appear somewhere in the session (not necessarily
	// in the same entry), matched case-insensitively as substrings.
	Terms []string
	// Repo matches the recorded repository root, the working
	// directory, or a file the session wrote — whichever it has.
	Repo string
	// Branch matches the recorded branch, which only sessions started
	// inside a checkout have.
	Branch string
	// Since bounds by last activity; the zero time means no bound.
	Since time.Time
}

// Match is one session that satisfied a Query, with a few of the
// entries that matched so a caller can show why.
type Match struct {
	SessionInfo
	Lines []Line // up to linesPerSession, in transcript order
	Hits  int    // entries that matched, whether or not they are in Lines
}

// Line is one matching entry, trimmed for display.
type Line struct {
	Seq  int64
	Kind string
	Text string
}

// linesPerSession caps the preview: a session that says "postgres"
// ninety times is not ninety results, it is one result worth opening.
const linesPerSession = 3

// ParseQuery reads "words repo:x branch:y since:7d" into a Query. An
// unknown prefix is not an error — it is a word, because "TODO:" and
// "note:" are things people search for.
func ParseQuery(s string) (Query, error) {
	var q Query
	for _, f := range strings.Fields(s) {
		name, val, ok := strings.Cut(f, ":")
		if !ok || val == "" {
			q.Terms = append(q.Terms, strings.ToLower(f))
			continue
		}
		switch strings.ToLower(name) {
		case "repo":
			q.Repo = strings.ToLower(val)
		case "branch":
			q.Branch = strings.ToLower(val)
		case "since":
			t, err := parseSince(val)
			if err != nil {
				return Query{}, err
			}
			q.Since = t
		default:
			q.Terms = append(q.Terms, strings.ToLower(f))
		}
	}
	return q, nil
}

// parseSince reads "7d", "24h", "2w" or "2026-09-01" as the instant a
// match must be no older than.
func parseSince(v string) (time.Time, error) {
	if t, err := time.ParseInLocation("2006-01-02", v, time.Local); err == nil {
		return t, nil
	}
	if n, err := strconv.Atoi(strings.TrimRight(v, "hdw")); err == nil && n > 0 {
		switch v[len(v)-1] {
		case 'h':
			return time.Now().Add(-time.Duration(n) * time.Hour), nil
		case 'd':
			return time.Now().AddDate(0, 0, -n), nil
		case 'w':
			return time.Now().AddDate(0, 0, -7*n), nil
		}
	}
	return time.Time{}, fmt.Errorf("since: want 7d, 24h, 2w or 2026-09-01, got %q", v)
}

// Search returns the sessions in dir matching q, most recently active
// first, at most limit of them (limit <= 0 means no cap). Sessions
// whose file cannot be read are skipped, as in List.
func Search(dir string, q Query, limit int) ([]Match, error) {
	infos, err := List(dir) // already newest-first: recency is the default order
	if err != nil {
		return nil, err
	}
	var out []Match
	for _, in := range infos {
		if !q.Since.IsZero() && in.ModTime.Before(q.Since) {
			continue
		}
		if q.Branch != "" && !strings.Contains(strings.ToLower(in.Branch), q.Branch) {
			continue
		}
		// The whole file is needed for the terms anyway, and repo:
		// falls back to the files a session wrote when it has no
		// recorded repo — both want the entries.
		entries, err := Read(in.Path)
		if err != nil {
			continue
		}
		if q.Repo != "" && !matchesRepo(in, entries, q.Repo) {
			continue
		}
		m, ok := matchTerms(in, entries, q.Terms)
		if !ok {
			continue
		}
		out = append(out, m)
		if limit > 0 && len(out) >= limit {
			break
		}
	}
	return out, nil
}

// matchesRepo reports whether the session belongs to repo: by its
// recorded root, its working directory, or a file it wrote. The
// fallbacks matter because repo capture is newer than most sessions.
func matchesRepo(in SessionInfo, entries []Entry, repo string) bool {
	for _, s := range []string{in.Repo, in.Cwd} {
		if s != "" && strings.Contains(strings.ToLower(s), repo) {
			return true
		}
	}
	for _, e := range entries {
		if e.Kind != "done" {
			continue
		}
		for _, f := range fileList(e) {
			if strings.Contains(strings.ToLower(f), repo) {
				return true
			}
		}
	}
	return false
}

// fileList is a done entry's "files", which round-trip through JSON as
// []any.
func fileList(e Entry) []string {
	raw, _ := e.Data["files"].([]any)
	out := make([]string, 0, len(raw))
	for _, v := range raw {
		if s, ok := v.(string); ok {
			out = append(out, s)
		}
	}
	return out
}

// matchTerms reports whether every term appears somewhere in the
// session, and picks the few lines worth showing as the reason. No
// terms is a filter-only query: it matches, with nothing to preview.
func matchTerms(in SessionInfo, entries []Entry, terms []string) (Match, bool) {
	m := Match{SessionInfo: in}
	if len(terms) == 0 {
		return m, true
	}
	seen := make(map[string]bool, len(terms))
	var cands []candidate
	for _, e := range entries {
		text := EntryText(e)
		if text == "" {
			continue
		}
		hit := false
		for _, t := range terms {
			if strings.Contains(strings.ToLower(text), t) {
				seen[t], hit = true, true
			}
		}
		if !hit {
			continue
		}
		m.Hits++
		line, score := bestLine(text, terms)
		cands = append(cands, candidate{
			Line:  Line{Seq: e.Seq, Kind: e.Kind, Text: line},
			score: score, said: isSaid(e.Kind),
		})
	}
	m.Lines = pickLines(cands)
	return m, len(seen) == len(terms)
}

// candidate is one matching entry in the running for the preview.
type candidate struct {
	Line
	score int  // terms present on the chosen line
	said  bool // a human turn rather than tool output
}

// isSaid reports whether a kind is something said, as opposed to
// something a tool printed. Tool output is worth SEARCHING — the
// command you ran is as memorable as the answer — but it makes a poor
// preview: a result's first lines are usually a path or a JSON blob.
func isSaid(kind string) bool {
	switch kind {
	case "input", "assistant", "sub:assistant", "thinking":
		return true
	}
	return false
}

// pickLines chooses the preview: what was said beats what a tool
// printed, a line carrying more of the query beats one carrying less,
// and the survivors are shown in transcript order so they read as a
// conversation rather than a ranking.
func pickLines(cands []candidate) []Line {
	if len(cands) == 0 {
		return nil
	}
	slices.SortStableFunc(cands, func(a, b candidate) int {
		if a.said != b.said {
			if a.said {
				return -1
			}
			return 1
		}
		return cmp.Compare(b.score, a.score)
	})
	cands = cands[:min(len(cands), linesPerSession)]
	slices.SortFunc(cands, func(a, b candidate) int { return cmp.Compare(a.Seq, b.Seq) })
	out := make([]Line, 0, len(cands))
	for _, c := range cands {
		out = append(out, c.Line)
	}
	return out
}

// bestLine is the line of text carrying the most query terms, and how
// many. Showing the entry's first line instead was actively unhelpful:
// the match is usually somewhere in the middle of a long result, and
// the head of one is a path.
func bestLine(text string, terms []string) (string, int) {
	best, bestScore := "", -1
	for line := range strings.SplitSeq(text, "\n") {
		t := strings.TrimSpace(line)
		if t == "" {
			continue
		}
		lower := strings.ToLower(t)
		score := 0
		for _, term := range terms {
			if strings.Contains(lower, term) {
				score++
			}
		}
		if score > bestScore {
			best, bestScore = t, score
		}
	}
	if bestScore < 0 {
		return "", 0
	}
	return best, bestScore
}

// EntryText is what an entry says: its text, plus the code of a result
// (the command that produced it is as memorable as its output). The
// picker searches by the same notion, so it is exported rather than
// reimplemented there with a different idea of which fields count.
func EntryText(e Entry) string {
	text, _ := e.Data["text"].(string)
	if code, _ := e.Data["code"].(string); code != "" {
		text = code + "\n" + text
	}
	return text
}
