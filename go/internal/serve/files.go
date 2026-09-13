package serve

// File lookup for the composer's @ picker.
//
// The obvious implementation — walk the working directory and match —
// does not survive this user: they work from home, which holds ~1000
// repos and several million files. So the walk is bounded on every
// axis (how deep, how many entries seen, how many matches kept) and
// answers with the best it found rather than the complete truth. A
// picker that is fast and slightly incomplete beats one that is right
// and takes nine seconds.

import (
	"context"
	"net/http"
	"os"
	"os/exec"
	"path"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

const (
	fileWalkCap  = 60000 // entries looked at before giving up
	fileDepthCap = 8     // directories below the base
	fileHitCap   = 30    // matches returned
)

// skipDir is the directories worth never descending into: they are
// enormous, machine-written, and never what @ is reaching for. The
// Go module cache alone holds hundreds of thousands of vendored files
// and was swallowing the whole walk on a real machine.
var skipDir = map[string]bool{
	".git": true, "node_modules": true, "vendor": true, "target": true,
	"dist": true, "build": true, ".venv": true, "venv": true,
	"__pycache__": true, ".next": true, ".cache": true,
	"Library": true, "Applications": true, "Downloads": true,
	"Pictures": true, "Music": true, "Movies": true, "Trash": true,
	".cargo": true, ".rustup": true, ".npm": true, ".gradle": true, ".m2": true,
}

// vendorPath is a relative directory that is somebody else's code even
// though its own name is innocent: ~/go/pkg/mod is the one that
// mattered here.
func vendorPath(rel string) bool {
	return rel == "go/pkg" || strings.HasPrefix(rel, "go/pkg/")
}

// fileHit is one path, relative to the directory searched.
type fileHit struct {
	Path string `json:"path"`
	Dir  bool   `json:"dir"`
	rank int
}

// files answers the @ picker: paths under a session's directory that
// match a query. An empty query is a bare "@": it lists the top of the
// directory, one level down, so the picker has something to show the
// moment it opens.
func (a *API) files(w http.ResponseWriter, r *http.Request) {
	q := strings.ToLower(strings.TrimSpace(r.URL.Query().Get("q")))
	base := a.home
	if id := r.URL.Query().Get("session"); id != "" {
		if info, ok := a.info(id); ok && info.Cwd != "" {
			base = info.Cwd
		}
	}
	if base == "" {
		writeJSON(w, http.StatusOK, map[string]any{"files": []fileHit{}})
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"files": findFiles(base, q)})
}

// findFiles searches breadth-first, on purpose.
//
// filepath.WalkDir is depth-first, so on a real home directory it dove
// into the first large tree it met and spent the whole entry budget
// there — every result came back from the Go module cache and the
// repositories the person actually works in were never reached. Going
// a level at a time means the budget is spent near the top, which is
// where the answer almost always is, and running out degrades to
// "shallow results only" rather than "results from one arbitrary
// subtree".
func findFiles(base, q string) []fileHit {
	// Inside a repository, git already knows which files are the
	// project's: an ignored export or build tree was outranking the real
	// source ("@app" answered with ds-bundle/ before anything in src).
	if paths, ok := gitFiles(base); ok {
		return rankHits(gitHits(paths, q))
	}
	var hits []fileHit
	seen := 0
	depthCap := fileDepthCap
	if q == "" {
		depthCap = 1 // a bare "@" lists the top, not the whole tree
	}
	queue := []string{""} // relative dirs, shallowest first

	for len(queue) > 0 && seen < fileWalkCap {
		dir := queue[0]
		queue = queue[1:]
		entries, err := os.ReadDir(filepath.Join(base, dir))
		if err != nil {
			continue // an unreadable directory is not the picker's problem
		}
		for _, d := range entries {
			if seen++; seen > fileWalkCap {
				break
			}
			name := d.Name()
			rel := name
			if dir != "" {
				rel = dir + string(os.PathSeparator) + name
			}
			if d.IsDir() {
				if skipDir[name] || strings.HasPrefix(name, ".") || vendorPath(filepath.ToSlash(rel)) {
					continue
				}
				if strings.Count(rel, string(os.PathSeparator)) < depthCap {
					queue = append(queue, rel)
				}
			}
			slash := filepath.ToSlash(rel)
			if r := matchPath(strings.ToLower(slash), strings.ToLower(name), q); r >= 0 {
				// Depth is a tiebreaker, not a filter: a file near the
				// top is more likely to be the one meant.
				hits = append(hits, fileHit{Path: slash, Dir: d.IsDir(), rank: r*10 - strings.Count(slash, "/")})
			}
		}
	}
	return rankHits(hits)
}

// gitFiles lists what git shows for base — tracked files plus untracked
// ones that are not ignored — relative to base. ok is false outside a
// work tree, or when git is missing or slow, and the walk takes over.
func gitFiles(base string) ([]string, bool) {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	out, err := exec.CommandContext(ctx, "git", "-C", base, "ls-files",
		"--cached", "--others", "--exclude-standard", "-z").Output()
	if err != nil {
		return nil, false
	}
	var paths []string
	for _, p := range strings.Split(string(out), "\x00") {
		if p != "" {
			paths = append(paths, p)
		}
	}
	return paths, true
}

// gitHits matches git's file list, and the directories it implies,
// against a query the same way the walk does.
func gitHits(paths []string, q string) []fileHit {
	var hits []fileHit
	dirs := map[string]bool{}
	add := func(p string, dir bool) {
		depth := strings.Count(p, "/")
		if q == "" && depth > 1 {
			return
		}
		if r := matchPath(strings.ToLower(p), strings.ToLower(path.Base(p)), q); r >= 0 {
			rank := r*10 - depth
			// Tracked build output (an embedded dist/app.js) is still
			// reachable, but the source it was built from comes first.
			for _, seg := range strings.Split(p, "/") {
				if skipDir[seg] {
					rank -= 100
					break
				}
			}
			hits = append(hits, fileHit{Path: p, Dir: dir, rank: rank})
		}
	}
	for _, p := range paths {
		for d := path.Dir(p); d != "." && !dirs[d]; d = path.Dir(d) {
			dirs[d] = true
			add(d, true)
		}
		add(p, false)
	}
	return hits
}

// rankHits orders matches best-first and keeps the top fileHitCap.
func rankHits(hits []fileHit) []fileHit {
	sort.SliceStable(hits, func(i, j int) bool {
		if hits[i].rank != hits[j].rank {
			return hits[i].rank > hits[j].rank
		}
		return len(hits[i].Path) < len(hits[j].Path)
	})
	if len(hits) > fileHitCap {
		hits = hits[:fileHitCap]
	}
	if hits == nil {
		return []fileHit{}
	}
	return hits
}

// matchPath ranks a path against a query: -1 for no match.
func matchPath(rel, name, q string) int {
	if i := strings.Index(name, q); i >= 0 {
		if i == 0 {
			return 100 // the name starts with it
		}
		return 80
	}
	if strings.Contains(rel, q) {
		return 50 // somewhere in the path
	}
	// Scattered, in order: "aptsx" finds "app/palette.tsx".
	at := 0
	for _, ch := range q {
		i := strings.IndexRune(rel[at:], ch)
		if i < 0 {
			return -1
		}
		at += i + 1
	}
	return 20
}
