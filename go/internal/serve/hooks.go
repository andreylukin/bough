package serve

// The hooks surface of the control room: what is installed, what fired,
// and an editor over the two pools. Hook and watcher files are plain
// .js on disk, so editing them is a read and a write — the guard is
// that serve has no auth, and a path parameter that escaped the pools
// would turn this into an arbitrary-file editor.

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"slices"
	"sort"
	"strings"
	"time"

	"github.com/andreylukin/bough/plugins/codemode"
)

// HookRow is one installed hook file. lastFired/lastDecision/failing
// come from the ledger, which lives in the child process (see fires).
type HookRow struct {
	Name         string     `json:"name"`
	Event        string     `json:"event"`
	Path         string     `json:"path"`
	Scope        string     `json:"scope"`
	Shadowed     bool       `json:"shadowed"`
	LastFired    *time.Time `json:"lastFired"`
	LastDecision string     `json:"lastDecision"`
	Failing      bool       `json:"failing"`
	Error        string     `json:"error"`
}

// HookFire is one row of the ledger: what ran, how long it took, and
// what it decided.
type HookFire struct {
	At       time.Time `json:"at"`
	Session  string    `json:"session"`
	Event    string    `json:"event"`
	Name     string    `json:"name"`
	Ms       int64     `json:"ms"`
	Decision string    `json:"decision"`
	Error    string    `json:"error"`
}

// dryrunTimeout bounds a hand-run hook. A dry run is a person waiting
// on a reply, so it is shorter than a hook's own budget in a turn.
const dryrunTimeout = 5 * time.Second

// pool is one directory hook or watcher files may live in. Nothing
// outside a pool is readable or writable through this API.
type pool struct {
	dir   string
	scope string // "home" or "project"
	kind  string // "hooks" or "watchers"
}

// pools are the four directories, home first so a project file of the
// same base name shadows the global one, as plugins/hooks resolves it.
// home comes from the API field rather than os.UserHomeDir() so a test
// reads a seeded pool instead of the developer's.
func (a *API) pools() []pool {
	var out []pool
	if a.home != "" {
		out = append(out,
			pool{filepath.Join(a.home, ".bough", "hooks"), "home", "hooks"},
			pool{filepath.Join(a.home, ".bough", "watchers"), "home", "watchers"},
		)
	}
	if cwd, err := os.Getwd(); err == nil {
		out = append(out,
			pool{filepath.Join(cwd, ".bough", "hooks"), "project", "hooks"},
			pool{filepath.Join(cwd, ".bough", "watchers"), "project", "watchers"},
		)
	}
	return out
}

// hooks answers the control room's one read: installed hooks, watcher
// status, recent fires.
func (a *API) hooks(w http.ResponseWriter, r *http.Request) {
	fires := a.fires()
	writeJSON(w, http.StatusOK, map[string]any{
		"hooks":    withLast(a.installed(), fires),
		"watchers": a.watcherStatus(),
		"fires":    fires,
	})
}

// withLast stamps each hook with its most recent fire, so the view can
// answer "is this thing even loaded?" without the reader cross-checking
// the ledger below it by eye. fires is newest first, so the first match
// wins.
func withLast(rows []HookRow, fires []HookFire) []HookRow {
	for i, row := range rows {
		for _, f := range fires {
			if f.Name != row.Name || f.Event != row.Event {
				continue
			}
			at := f.At
			rows[i].LastFired = &at
			rows[i].LastDecision = f.Decision
			if f.Error != "" {
				rows[i].Failing, rows[i].Error = true, f.Error
			}
			break
		}
	}
	return rows
}

// fireLimit bounds the ledger a single read returns. The view shows
// what happened recently; a session that has fired a hook on every
// tool call for an hour is not a list anyone reads to the end.
const fireLimit = 200

// fires is the ledger, newest first. The records are written to the
// session history by the loop (kind "hook"), because the ring that
// holds them lives in the child process and serve cannot reach it.
// History is the one thing both processes share.
func (a *API) fires() []HookFire {
	sessions, err := a.sup.List()
	if err != nil {
		return []HookFire{}
	}
	out := []HookFire{}
	for _, si := range sessions {
		entries, err := a.sup.Entries(si.ID)
		if err != nil {
			continue // a session mid-write is not an error worth failing the page for
		}
		for _, e := range entries {
			if e.Kind != "hook" {
				continue
			}
			out = append(out, HookFire{
				At:       e.At,
				Session:  si.ID,
				Event:    str(e.Data["event"]),
				Name:     str(e.Data["name"]),
				Ms:       num(e.Data["ms"]),
				Decision: str(e.Data["decision"]),
				Error:    str(e.Data["error"]),
			})
		}
	}
	sort.Slice(out, func(i, j int) bool { return out[i].At.After(out[j].At) })
	if len(out) > fireLimit {
		out = out[:fireLimit]
	}
	return out
}

// num reads a history entry's data, which is JSON and so carries
// numbers as float64. The string reader is status.go's str.
func num(v any) int64 {
	switch n := v.(type) {
	case float64:
		return int64(n)
	case int64:
		return n
	case int:
		return int64(n)
	}
	return 0
}

// installed lists every hook file in both pools, sorted by event then
// base name. A home file is shadowed when a project file of the same
// base name exists for the same event, which is the rule
// plugins/hooks' own lookup uses.
func (a *API) installed() []HookRow {
	type key struct{ event, name string }
	projects := map[key]bool{}
	var rows []HookRow
	for _, p := range a.pools() {
		if p.kind != "hooks" {
			continue
		}
		events, err := os.ReadDir(p.dir)
		if err != nil {
			continue
		}
		for _, ev := range events {
			if !ev.IsDir() {
				continue
			}
			files, err := os.ReadDir(filepath.Join(p.dir, ev.Name()))
			if err != nil {
				continue
			}
			for _, f := range files {
				if f.IsDir() || !strings.HasSuffix(f.Name(), ".js") {
					continue
				}
				if p.scope == "project" {
					projects[key{ev.Name(), f.Name()}] = true
				}
				rows = append(rows, HookRow{
					Name:  f.Name(),
					Event: ev.Name(),
					Path:  filepath.Join(p.dir, ev.Name(), f.Name()),
					Scope: p.scope,
				})
			}
		}
	}
	out := make([]HookRow, 0, len(rows))
	for _, row := range rows {
		row.Shadowed = row.Scope == "home" && projects[key{row.Event, row.Name}]
		out = append(out, row)
	}
	slices.SortStableFunc(out, func(x, y HookRow) int {
		if x.Event != y.Event {
			return strings.Compare(x.Event, y.Event)
		}
		return strings.Compare(x.Name, y.Name)
	})
	return out
}

// hookFile reads one file out of the pools.
func (a *API) hookFile(w http.ResponseWriter, r *http.Request) {
	path, err := a.poolPath(r.URL.Query().Get("path"))
	if err != nil {
		writeErr(w, http.StatusBadRequest, err)
		return
	}
	body, err := os.ReadFile(path)
	if err != nil {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: read hook file: %w", err))
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"path": path, "body": string(body)})
}

// putHookFile writes one file back. The parent directory is created so
// the first hook for an event can be written from the UI.
func (a *API) putHookFile(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Path string `json:"path"`
		Body string `json:"body"`
	}
	if !decode(w, r, &body) {
		return
	}
	path, err := a.poolPath(body.Path)
	if err != nil {
		writeErr(w, http.StatusBadRequest, err)
		return
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: write hook file: %w", err))
		return
	}
	if err := os.WriteFile(path, []byte(body.Body), 0o644); err != nil {
		writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: write hook file: %w", err))
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

// dryrun runs one file by hand against a made-up event, so someone can
// see what a hook decides without waiting for a turn to trip it. The VM
// has no tools: a hook only inspects the event it is handed.
func (a *API) dryrun(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Path  string `json:"path"`
		Event string `json:"event"`
	}
	if !decode(w, r, &body) {
		return
	}
	path, err := a.poolPath(body.Path)
	if err != nil {
		writeErr(w, http.StatusBadRequest, err)
		return
	}
	src, err := os.ReadFile(path)
	if err != nil {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: read hook file: %w", err))
		return
	}
	// A watcher's first call is its config phase; a hook is called with
	// the event itself.
	event := map[string]any{"event": body.Event}
	if a.isWatcher(path) {
		event["phase"] = "config"
	}
	ctx, cancel := context.WithTimeout(r.Context(), dryrunTimeout)
	defer cancel()
	start := time.Now()
	res, runErr := codemode.New(dryrunTimeout).RunHook(ctx, string(src), event)
	ms := time.Since(start).Milliseconds()

	out := map[string]any{"result": map[string]any{}, "error": "", "ms": ms}
	if runErr != nil {
		out["error"] = runErr.Error()
	} else if res != nil {
		out["result"] = res
	}
	writeJSON(w, http.StatusOK, out)
}

func (a *API) isWatcher(path string) bool {
	for _, p := range a.pools() {
		if p.kind == "watchers" && under(resolveExisting(p.dir), path) {
			return true
		}
	}
	return false
}

// poolPath resolves a caller-supplied path and refuses anything that
// does not land inside one of the pools. Symlinks are resolved on both
// sides before the comparison: serve has no auth, so a traversal or a
// symlink out of the pool would make this an editor for every file the
// user owns.
func (a *API) poolPath(raw string) (string, error) {
	if strings.TrimSpace(raw) == "" {
		return "", errors.New("serve: api: path is required")
	}
	if !strings.HasSuffix(raw, ".js") {
		return "", fmt.Errorf("serve: api: %q is not a .js file", raw)
	}
	abs, err := filepath.Abs(raw)
	if err != nil {
		return "", fmt.Errorf("serve: api: bad path %q: %w", raw, err)
	}
	real := resolveExisting(abs)
	for _, p := range a.pools() {
		if under(resolveExisting(p.dir), real) {
			return real, nil
		}
	}
	return "", fmt.Errorf("serve: api: %q is outside the hook and watcher directories", raw)
}

// resolveExisting resolves symlinks as far as the path exists and
// re-joins the rest, so a file that is not written yet still resolves
// through the symlinked directories above it.
func resolveExisting(path string) string {
	path = filepath.Clean(path)
	rest := ""
	for {
		if p, err := filepath.EvalSymlinks(path); err == nil {
			return filepath.Join(p, rest)
		}
		parent := filepath.Dir(path)
		if parent == path {
			return filepath.Join(path, rest)
		}
		rest = filepath.Join(filepath.Base(path), rest)
		path = parent
	}
}

// under reports whether path is root itself or inside it.
func under(root, path string) bool {
	rel, err := filepath.Rel(root, path)
	if err != nil {
		return false
	}
	return rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator))
}
