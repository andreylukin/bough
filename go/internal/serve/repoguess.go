package serve

// Which repo a session was about.
//
// A session records the directory it started in, and for this user that
// is always their home directory — not a repo — so Row.Repo is empty on
// every one of them. The repo is only ever knowable from the paths the
// session touched: `cd repos/worktree/<repo>.<branch> && …`. Reading
// that back is what makes grouping 150 conversations possible without
// filing each one by hand.

import (
	"bufio"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"sync"
)

// repoPath matches the repos/<name> and repos/worktree/<name> prefixes
// an agent working from home uses to reach a checkout.
// The prefix must start a path segment, so "myrepos/x" is not a repo.
var repoPath = regexp.MustCompile(`(?:^|[^A-Za-z0-9._-])repos/(?:worktree/)?([A-Za-z0-9._-]+)`)

// placeholder names are what docs and prompts write for "some repo"
// ("~/repos/repo", "repos/<name>"): grouping sessions under them is noise.
var placeholder = map[string]bool{"repo": true, "repos": true, "worktree": true, "name": true, "x": true}

// scanCap bounds the read: a transcript here reaches 10MB, and the
// directory 111MB, which is far too much to read on a page load. The
// repo a session is about is established by its first few commands, so
// a bounded prefix answers the question at a fraction of the cost.
const scanCap = 1 << 20 // 1MiB

type guess struct {
	repo string
	mod  int64
	size int64
}

var (
	guessMu sync.Mutex
	guesses = map[string]guess{}
)

// guessRepo names the repo a transcript is mostly about, or "" when it
// touched none. Cached per file on mtime and size: a finished session
// never changes, and a live one is re-read only after it grows.
func guessRepo(path string) string {
	fi, err := os.Stat(path)
	if err != nil {
		return ""
	}
	guessMu.Lock()
	if g, ok := guesses[path]; ok && g.mod == fi.ModTime().UnixNano() && g.size == fi.Size() {
		guessMu.Unlock()
		return g.repo
	}
	guessMu.Unlock()

	repo := scanRepo(path)
	guessMu.Lock()
	guesses[path] = guess{repo: repo, mod: fi.ModTime().UnixNano(), size: fi.Size()}
	guessMu.Unlock()
	return repo
}

func scanRepo(path string) string {
	f, err := os.Open(path)
	if err != nil {
		return ""
	}
	defer f.Close()

	counts := map[string]int{}
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 0, 64*1024), 4*1024*1024)
	read := 0
	for sc.Scan() {
		line := sc.Bytes()
		read += len(line)
		if read > scanCap {
			break
		}
		if !strings.Contains(string(line), "repos/") {
			continue
		}
		for _, m := range repoPath.FindAllStringSubmatch(string(line), -1) {
			// A worktree is named "<repo>.<branch>"; the repo is what
			// comes before the first dot, so every branch of one repo
			// groups together instead of fragmenting.
			name := m[1]
			if i := strings.Index(name, "."); i > 0 {
				name = name[:i]
			}
			if name == "" || placeholder[name] {
				continue
			}
			counts[name]++
		}
	}
	best, top := "", 0
	for name, n := range counts {
		if n > top || (n == top && name < best) {
			best, top = name, n
		}
	}
	return best
}

// RepoGroup is the unassigned sessions that share a repo.
type RepoGroup struct {
	Repo     string   `json:"repo"`
	Count    int      `json:"count"`
	Sessions []string `json:"sessions"`
}

// repoGroups buckets every unassigned session by the repo it worked in,
// largest first. Assigned sessions are left alone: this exists to clear
// the unassigned pile, not to re-file what you already filed.
func (a *API) repoGroups() []RepoGroup {
	sessions, err := a.sup.List()
	if err != nil {
		return []RepoGroup{}
	}
	by := map[string][]string{}
	for _, si := range sessions {
		if m := a.sup.Meta(si.ID); m.Project != "" || m.Archived {
			continue
		}
		repo := guessRepo(filepath.Join(a.sup.HistDir(), si.ID+".jsonl"))
		if repo == "" {
			continue
		}
		by[repo] = append(by[repo], si.ID)
	}
	out := make([]RepoGroup, 0, len(by))
	for repo, ids := range by {
		sort.Strings(ids)
		out = append(out, RepoGroup{Repo: repo, Count: len(ids), Sessions: ids})
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].Count != out[j].Count {
			return out[i].Count > out[j].Count
		}
		return out[i].Repo < out[j].Repo
	})
	return out
}
