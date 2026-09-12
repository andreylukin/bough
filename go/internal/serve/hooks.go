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

	"github.com/andreylukin/bough/internal/serve/watch"
	"github.com/andreylukin/bough/plugins/ccplugins"
	"github.com/andreylukin/bough/plugins/codemode"
	"github.com/andreylukin/bough/plugins/offlist"
	"github.com/andreylukin/bough/plugins/rules"
)

// HookRow is one installed hook file. lastFired/lastDecision/failing
// come from the ledger, which lives in the child process (see fires).
type HookRow struct {
	// ID is the off-switch id: "<event>/<name>", the same string
	// plugins/hooks checks before it runs a file.
	ID           string     `json:"id"`
	Name         string     `json:"name"`
	Event        string     `json:"event"`
	Path         string     `json:"path"`
	Scope        string     `json:"scope"`
	Shadowed     bool       `json:"shadowed"`
	LastFired    *time.Time `json:"lastFired"`
	LastDecision string     `json:"lastDecision"`
	Failing      bool       `json:"failing"`
	Error        string     `json:"error"`
	Off          bool       `json:"off"`
}

// WatcherRow is the engine's status plus the two fields the control
// room needs on every switchable thing: its off-switch id and whether
// that switch is thrown.
type WatcherRow struct {
	ID string `json:"id"`
	watch.WatcherStatus
	Off bool `json:"off"`
}

// RuleRow is one rule file as the control room lists it. Globs is the
// `paths:` frontmatter — what makes a rule scoped.
type RuleRow struct {
	ID    string   `json:"id"`
	Name  string   `json:"name"`
	Path  string   `json:"path"`
	Scope string   `json:"scope"`
	Kind  string   `json:"kind"`
	Globs []string `json:"globs"`
	Off   bool     `json:"off"`
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
	// Notice is what a hook wanted the human to know. It never reached
	// the model — that is the point of the channel — so this page is
	// the only place it is visible after the turn scrolls away.
	Notice    string   `json:"notice"`
	Truncated []string `json:"truncated"`
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
	// The user runs bough from home, so the project pools are then the
	// home pools again. Listing a file twice would give it two rows
	// with the same off-switch id, each claiming to shadow the other.
	if cwd, err := os.Getwd(); err == nil && !sameDir(cwd, a.home) {
		out = append(out,
			pool{filepath.Join(cwd, ".bough", "hooks"), "project", "hooks"},
			pool{filepath.Join(cwd, ".bough", "watchers"), "project", "watchers"},
		)
	}
	return out
}

// sameDir compares two directories as the filesystem sees them, so a
// cwd reached through a symlink (/tmp on macOS) still matches home.
func sameDir(a, b string) bool {
	if a == "" || b == "" {
		return false
	}
	if filepath.Clean(a) == filepath.Clean(b) {
		return true
	}
	ra, err1 := filepath.EvalSymlinks(a)
	rb, err2 := filepath.EvalSymlinks(b)
	return err1 == nil && err2 == nil && ra == rb
}

// hooks answers the control room's one read: installed hooks, watcher
// status, recent fires.
func (a *API) hooks(w http.ResponseWriter, r *http.Request) {
	fires := a.fires()
	cwd, _ := os.Getwd()
	writeJSON(w, http.StatusOK, map[string]any{
		"hooks":    withLast(a.installed(), fires),
		"watchers": a.watchers(),
		"fires":    fires,
		"rules":    a.ruleRows(cwd),
		"plugins":  a.pluginRows(),
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
				At:        e.At,
				Session:   si.ID,
				Event:     str(e.Data["event"]),
				Name:      str(e.Data["name"]),
				Ms:        num(e.Data["ms"]),
				Decision:  str(e.Data["decision"]),
				Error:     str(e.Data["error"]),
				Notice:    str(e.Data["notice"]),
				Truncated: strs(e.Data["truncated"]),
			})
		}
	}
	sort.Slice(out, func(i, j int) bool { return out[i].At.After(out[j].At) })
	if len(out) > fireLimit {
		out = out[:fireLimit]
	}
	return out
}

// strs reads a history entry's list of strings; JSON gives []any.
func strs(v any) []string {
	raw, _ := v.([]any)
	out := make([]string, 0, len(raw))
	for _, e := range raw {
		if s, ok := e.(string); ok {
			out = append(out, s)
		}
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
					ID:    ev.Name() + "/" + f.Name(),
					Name:  f.Name(),
					Event: ev.Name(),
					Path:  filepath.Join(p.dir, ev.Name(), f.Name()),
					Scope: p.scope,
				})
			}
		}
	}
	off := a.off()
	out := make([]HookRow, 0, len(rows))
	for _, row := range rows {
		row.Off = off.Off("hook", row.ID)
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

// off is the switch list for this home. The handlers read it fresh:
// off.yml is a file the user edits by hand as well as through the API.
func (a *API) off() *offlist.List { return offlist.Load(a.boughHome()) }

func (a *API) boughHome() string { return filepath.Join(a.home, ".bough") }

// watchers decorates the engine's rows with the off switch. The id is
// the file's base name, which is what plugins/hooks and the watcher
// engine both name a watcher by.
func (a *API) watchers() []WatcherRow {
	off := a.off()
	out := []WatcherRow{}
	for _, st := range a.watcherStatus() {
		out = append(out, WatcherRow{ID: st.Name, WatcherStatus: st, Off: off.Off("watcher", st.Name)})
	}
	return out
}

// ruleRows lists the rules in force for a directory. Rules() has
// already dropped the ones switched off — they would otherwise be
// invisible, and a switch nothing shows cannot be thrown back — so
// every "rule:" entry in off.yml is added afterwards from its path.
func (a *API) ruleRows(project string) []RuleRow {
	off := a.off()
	out := []RuleRow{}
	seen := map[string]bool{}
	for _, r := range rules.New(a.home, project).Rules() {
		globs := r.Globs
		if globs == nil {
			globs = []string{}
		}
		seen[r.ID()] = true
		out = append(out, RuleRow{
			ID:    r.ID(),
			Name:  filepath.Base(r.Path),
			Path:  r.Path,
			Scope: a.ruleScope(r.Path),
			Kind:  r.Kind(),
			Globs: globs,
		})
	}
	for _, e := range off.Entries() {
		path, ok := strings.CutPrefix(e, "rule:")
		if !ok || seen[path] {
			continue
		}
		// An off rule is never parsed, so its kind is read off the
		// name: only Codex's .rules files gate shell commands.
		kind := "prose"
		if strings.HasSuffix(path, ".rules") {
			kind = "gate"
		}
		out = append(out, RuleRow{
			ID: path, Name: filepath.Base(path), Path: path,
			Scope: a.ruleScope(path), Kind: kind, Globs: []string{}, Off: true,
		})
	}
	return out
}

// ruleScope says where a rule came from. Anything under the user's
// home rule directories is "home"; everything else was found beside
// the code, which is what "repo" means to the reader.
func (a *API) ruleScope(path string) string {
	for _, sub := range []string{".claude", ".codex"} {
		if a.home != "" && under(filepath.Join(a.home, sub, "rules"), path) {
			return "home"
		}
	}
	return "repo"
}

// pluginRows is every Claude Code plugin in the manifest, present or
// not — a plugin whose install directory has gone is exactly what the
// control room exists to show.
func (a *API) pluginRows() []ccplugins.Plugin {
	out := ccplugins.Installed(a.home)
	if out == nil {
		return []ccplugins.Plugin{}
	}
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
