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
	"io/fs"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

const (
	fileWalkCap  = 60000 // entries looked at before giving up
	fileDepthCap = 8     // directories below the base
	fileHitCap   = 30    // matches returned
)

// skipDir is the directories worth never descending into: they are
// enormous, machine-written, and never what @ is reaching for.
var skipDir = map[string]bool{
	".git": true, "node_modules": true, "vendor": true, "target": true,
	"dist": true, "build": true, ".venv": true, "venv": true,
	"__pycache__": true, ".next": true, ".cache": true, "Library": true,
}

// fileHit is one path, relative to the directory searched.
type fileHit struct {
	Path string `json:"path"`
	Dir  bool   `json:"dir"`
	rank int
}

// files answers the @ picker: paths under a session's directory that
// match a query. Needs a query — listing a home directory is not a
// useful answer to "which file".
func (a *API) files(w http.ResponseWriter, r *http.Request) {
	q := strings.ToLower(strings.TrimSpace(r.URL.Query().Get("q")))
	base := a.home
	if id := r.URL.Query().Get("session"); id != "" {
		if info, ok := a.info(id); ok && info.Cwd != "" {
			base = info.Cwd
		}
	}
	if q == "" || base == "" {
		writeJSON(w, http.StatusOK, map[string]any{"files": []fileHit{}})
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"files": findFiles(base, q)})
}

func findFiles(base, q string) []fileHit {
	var hits []fileHit
	seen := 0
	_ = filepath.WalkDir(base, func(p string, d fs.DirEntry, err error) error {
		if err != nil {
			return nil // an unreadable directory is not the picker's problem
		}
		if seen++; seen > fileWalkCap {
			return fs.SkipAll
		}
		rel, rerr := filepath.Rel(base, p)
		if rerr != nil || rel == "." {
			return nil
		}
		name := d.Name()
		if d.IsDir() {
			if skipDir[name] || (strings.HasPrefix(name, ".") && name != ".") {
				return fs.SkipDir
			}
			if strings.Count(rel, string(os.PathSeparator)) >= fileDepthCap {
				return fs.SkipDir
			}
		}
		if rank := matchPath(strings.ToLower(rel), strings.ToLower(name), q); rank >= 0 {
			hits = append(hits, fileHit{Path: filepath.ToSlash(rel), Dir: d.IsDir(), rank: rank})
		}
		return nil
	})
	// Best first, then shortest: a match on the file's own name beats
	// one buried in its directories, and a short path is more likely to
	// be the thing meant.
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
