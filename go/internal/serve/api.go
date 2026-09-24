package serve

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"sort"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/serve/watch"
	"github.com/andreylukin/bough/plugins/history"
)

// API is the session control surface: JSON over 127.0.0.1 plus one
// server-sent-events stream per session. It owns no state — every
// answer is derived from the supervisor's leases and the session's
// history on disk, so two readers never disagree about a session.
type API struct {
	sup *Supervisor
	mux *http.ServeMux
	// home is where the skill pools are looked up. A field rather than
	// a call to os.UserHomeDir() inside the handler, so a test lists a
	// seeded pool instead of whatever the developer happens to have.
	home string
	// watch is the watcher engine, nil until StartWatchers runs it (and
	// when it refuses to: watchers are shell, and a non-loopback bind
	// means no engine at all).
	watch *watch.Engine
	// ingest starts a wiki ingest (spawnIngest). A field so a test can
	// see the call without running a model.
	ingest func(only string) error
	// defaults names the llm row a child started in dir mounts
	// (SetDefaults); nil = unknown.
	defaults func(dir string) ModelDefault
	// brief writes today's brief now (spawnBrief); a field for the same reason.
	brief func() error
	// briefs counts the brief processes this serve started that have not
	// exited: the Me page shows a job while it runs, since its spinner is
	// only a timer. A tick's brief started elsewhere is not counted.
	briefs atomic.Int32
	// running is the runtime's running containers as of runningAt, the
	// snapshot containerUp answers from (see orbs.go).
	runningMu sync.Mutex
	running   map[string]bool
	runningAt time.Time
	// runningFor overrides runningTTL when set (SetRunningTTL).
	runningFor time.Duration
	// start is the directory serve was started in, the folder first-run
	// setup offers. getenv and setenv are fields so a test neither reads
	// the developer's keys nor writes the process environment.
	start  string
	getenv func(string) string
	setenv func(string, string) error
	// checkKey asks a provider whether a key is accepted: the HTTP status, or an error when it could not ask.
	checkKey func(ctx context.Context, provider, key string) (int, error)
}

// Row is one session as the wire sees it: what history knows, what the
// supervisor knows (lease, rename, archive) and the derived status.
type Row struct {
	ID    string `json:"id"`
	Title string `json:"title"`
	// Summary is a few sentences on what the session is about, written by
	// the small model as the conversation grows; "" before it has one.
	Summary  string    `json:"summary,omitempty"`
	Cwd      string    `json:"cwd"`
	Repo     string    `json:"repo,omitempty"`
	Branch   string    `json:"branch,omitempty"`
	Status   Status    `json:"status"`
	Live     bool      `json:"live"`
	Archived bool      `json:"archived"`
	Entries  int       `json:"entries"`
	Modified time.Time `json:"modified"`
	// LastAt is when the session last wrote an entry; a file's mtime can
	// move without any activity (a copy, a restore), so recency reads this.
	LastAt time.Time `json:"lastAt"`
	Ask    *Ask      `json:"ask,omitempty"`
	// Model and Effort are what this session was last ASKED to run as
	// (empty = whatever its own config says). They are not read back
	// from the child, so do not present them as ground truth.
	Model  string `json:"model,omitempty"`
	Effort string `json:"effort,omitempty"`
	// Configured is the session's own llm row while it still runs it:
	// what its bough.yml says (not serve's), with the model it answered
	// as when the config names none. nil once a model was set, from here
	// or by /model in the session: nothing switches back to it.
	Configured *ModelDefault `json:"configured,omitempty"`
	// Project is the project this conversation belongs to, by slug. For a
	// project session it is the slug its history recorded, which nothing
	// can re-file; for a local one it is where a person filed it.
	Project string `json:"project,omitempty"`
	// StartedIn is the project whose directory (and so MEMORY.md) the
	// running child was started with; "" when none or no child runs.
	// Filing a live session changes Project at once but not this: the
	// child reads the directory only at its start.
	StartedIn string `json:"startedIn,omitempty"`
	// Jobs are the background jobs still running; Cache is the prompt
	// cache after the last turn that reported one.
	Jobs  []Job  `json:"jobs,omitempty"`
	Cache *Cache `json:"cache,omitempty"`
	// Trouble is why this session needs a person ("failed",
	// "interrupted", "tests failed"), or "" once marked seen.
	Trouble string `json:"trouble,omitempty"`
	// Unseen marks a web session whose last turn finished cleanly after
	// it was last marked seen; opening it marks it seen.
	Unseen bool `json:"unseen,omitempty"`
	// TestsFailed is the last test run's recorded non-zero exit; unlike
	// Trouble it outlives being marked seen, since seen is not fixed.
	TestsFailed bool `json:"testsFailed,omitempty"`
	// TestsAt is when that last test run's result was recorded.
	TestsAt *time.Time `json:"testsAt,omitempty"`
	// Turns counts the lines of the session's running log (GET .../turns).
	Turns int `json:"turns,omitempty"`
	// Background marks a run nobody started by hand (a wiki ingest, a
	// bench, a test); the sidebar folds these away.
	Background bool `json:"background,omitempty"`
	// Empty marks a session nobody has sent a message yet (opened, then
	// left); the sidebar leaves these out unless one is open or live.
	Empty bool `json:"empty,omitempty"`
	// Mode is "local" or "project", from the session's meta entry; a
	// meta written before modes existed reads as local.
	Mode string `json:"mode"`
	// Writable is the git checkout a local session may edit; "" when it
	// started outside one (read-only) or is a project session.
	Writable string `json:"writable,omitempty"`
	// Orb is a project session's container state, nil for local.
	Orb *RowOrb `json:"orb,omitempty"`
	// SpawnedBy is the parent of a background agent; Queued marks one
	// waiting for a slot; Agents counts a parent's children (nil at 0).
	SpawnedBy string      `json:"spawnedBy,omitempty"`
	Queued    bool        `json:"queued,omitempty"`
	Agents    *AgentCount `json:"agents,omitempty"`
	// Error is a failed background agent's first error line, cleaned;
	// only the children listing fills it.
	Error string `json:"error,omitempty"`
}

// AgentCount is a parent's background agents.
type AgentCount struct {
	Running int `json:"running"`
	Queued  int `json:"queued"`
	Total   int `json:"total"`
}

// RowOrb is the cheap orb summary a session row carries.
type RowOrb struct {
	Project string     `json:"project"`
	Status  orb.Status `json:"status"`
	Up      bool       `json:"up,omitempty"` // container running (a failed setup can leave it up)
	// Portals are the guest ports of this session's LIVE portals, in the order
	// they were opened. The list already dials every recorded portal to build
	// the orb state, so carrying the answer here is free; a dead record is
	// dropped, because the header must not offer a port nothing accepts on.
	Portals []int `json:"portals,omitempty"`
	// Phase is the step in progress (orb.PhaseSync…PhaseReady); PhaseAt is when
	// it began, so a row can time it without a second fetch.
	Phase   string     `json:"phase,omitempty"`
	PhaseAt *time.Time `json:"phaseAt,omitempty"`
}

// maxBody caps every JSON request body. Prompts carry pasted logs and
// files, so it is generous; images go through /api/attachments.
const maxBody = 8 << 20

// heartbeat keeps an idle SSE stream alive through proxies and tells a
// client the server is still there while a session sits quiet.
const heartbeat = 15 * time.Second

// NewAPI wires the routes. Method+pattern routing means a wrong method
// on a real path is the mux's own 405, not a 404.
func NewAPI(sup *Supervisor) *API {
	home, _ := os.UserHomeDir()
	start, _ := os.Getwd()
	a := &API{sup: sup, mux: http.NewServeMux(), home: home, start: start, getenv: os.Getenv, setenv: os.Setenv, checkKey: keyChecker(os.Getenv("BOUGH_SETUP_CHECK_URL"))}
	a.ingest = a.spawnIngest
	a.brief = a.spawnBrief
	a.mux.HandleFunc("GET /api/health", a.health)
	a.mux.HandleFunc("GET /api/setup", a.setup)
	a.mux.HandleFunc("GET /api/dirs", a.dirs)
	a.mux.HandleFunc("POST /api/setup/key", a.setKey)
	a.mux.HandleFunc("GET /api/setup/check", a.checkSetupKey)
	a.mux.HandleFunc("GET /api/sessions", a.listSessions)
	a.mux.HandleFunc("POST /api/sessions", a.createSession)
	a.mux.HandleFunc("GET /api/sessions/{id}", a.getSession)
	a.mux.HandleFunc("POST /api/sessions/{id}/prompt", a.prompt)
	a.mux.HandleFunc("GET /api/sessions/{id}/prompts/{rid}", a.promptState)
	a.mux.HandleFunc("POST /api/sessions/{id}/answer", a.answer)
	a.mux.HandleFunc("POST /api/sessions/{id}/interrupt", a.interrupt)
	a.mux.HandleFunc("POST /api/sessions/{id}/rename", a.rename)
	a.mux.HandleFunc("POST /api/sessions/{id}/archive", a.archive)
	a.mux.HandleFunc("POST /api/sessions/{id}/unarchive", a.unarchive)
	a.mux.HandleFunc("POST /api/sessions/{id}/ack", a.ack)
	a.mux.HandleFunc("POST /api/sessions/{id}/model", a.setModel)
	a.mux.HandleFunc("POST /api/sessions/{id}/effort", a.setEffort)
	a.mux.HandleFunc("POST /api/attachments", a.upload)
	a.mux.HandleFunc("GET /api/attachments", a.attachment)
	a.mux.HandleFunc("POST /api/sessions/{id}/files", a.uploadFile)
	a.mux.HandleFunc("GET /api/search", a.search)
	a.mux.HandleFunc("GET /api/files", a.files)
	a.mux.HandleFunc("GET /api/models", a.models)
	a.mux.HandleFunc("GET /api/skills", a.skills)
	a.mux.HandleFunc("GET /api/hooks", a.hooks)
	a.mux.HandleFunc("GET /api/sessions/{id}/context", a.sessionContext)
	a.mux.HandleFunc("GET /api/sessions/{id}/changes", a.changes)
	a.mux.HandleFunc("GET /api/sessions/{id}/diff", a.diff)
	a.mux.HandleFunc("GET /api/sessions/{id}/edits", a.edits)
	a.mux.HandleFunc("GET /api/sessions/{id}/turns", a.turns)
	a.mux.HandleFunc("POST /api/sessions/{id}/jobs/{job}/kill", a.killJob)
	a.mux.HandleFunc("POST /api/off", a.setOff)
	a.mux.HandleFunc("GET /api/hooks/file", a.hookFile)
	a.mux.HandleFunc("PUT /api/hooks/file", a.putHookFile)
	a.mux.HandleFunc("POST /api/hooks/dryrun", a.dryrun)
	a.mux.HandleFunc("GET /api/projects", a.listProjects)
	a.mux.HandleFunc("GET /api/projects/by-repo", a.byRepo)
	a.mux.HandleFunc("POST /api/projects/from-repo", a.projectFromRepo)
	a.mux.HandleFunc("POST /api/projects", a.createProject)
	a.mux.HandleFunc("GET /api/projects/{slug}", a.projectDetail)
	a.mux.HandleFunc("POST /api/projects/{slug}/rename", a.renameProject)
	a.mux.HandleFunc("POST /api/projects/{slug}/message", a.messageProject)
	a.mux.HandleFunc("POST /api/projects/{slug}/archive", a.archiveProject)
	a.mux.HandleFunc("DELETE /api/projects/{slug}", a.deleteProject)
	a.mux.HandleFunc("POST /api/sessions/{id}/project", a.assignProject)
	a.mux.HandleFunc("GET /api/sessions/{id}/events", a.events)
	a.mux.HandleFunc("GET /api/sessions/{id}/children", a.children)
	a.mux.HandleFunc("GET /api/sessions/{id}/agent", a.agent)
	a.mux.HandleFunc("POST /api/sessions/{id}/stop", a.stopAgent)
	a.mux.HandleFunc("POST /api/sessions/{id}/notify", a.notify)
	a.routeOrbs()
	a.mux.HandleFunc("GET /api/wiki", a.wikiIndex)
	a.mux.HandleFunc("GET /api/wiki/page", a.wikiPage)
	a.mux.HandleFunc("PUT /api/wiki/page", a.putWikiPage)
	a.mux.HandleFunc("GET /api/wiki/history", a.wikiHistory)
	a.mux.HandleFunc("GET /api/wiki/source", a.wikiSource)
	a.mux.HandleFunc("GET /api/wiki/review", a.wikiReview)
	a.mux.HandleFunc("POST /api/wiki/claim", a.wikiClaim)
	a.mux.HandleFunc("GET /api/wiki/activity", a.wikiActivity)
	a.mux.HandleFunc("POST /api/wiki/check", a.wikiCheck)
	a.mux.HandleFunc("GET /api/wiki/search", a.wikiSearch)
	a.mux.HandleFunc("POST /api/wiki/ingest", a.wikiIngest)
	a.mux.HandleFunc("GET /api/me", a.me)
	a.mux.HandleFunc("POST /api/me/refresh", a.meRefresh)
	a.mux.HandleFunc("POST /api/me/triage", a.meTriage)
	a.mux.HandleFunc("POST /api/me/steer", a.meSteer)
	// The UI, on EXACT paths only. A catch-all "GET /" would match a
	// wrong-method request to a real API route (GET on a POST-only
	// path), and ServeMux then serves the page instead of the 405 it
	// owes the caller. "{$}" matches the root and nothing else.
	if ui, err := staticHandler(); err == nil {
		a.mux.Handle("GET /{$}", ui)
		a.mux.Handle("GET /app.js", ui)
	}
	return a
}

// ServeHTTP stamps every answer — the page, the bundle and the API —
// with the build that gave it, so a tab left open across `bough update`
// can notice the restart and offer a reload (see build.go).
func (a *API) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	w.Header().Set(BuildHeader, buildID())
	a.mux.ServeHTTP(w, r)
}

// health also carries where a new session would start. Creating one
// needs a directory, and the page has no way to know the home it is
// being served from — without this the UI can only offer to create a
// session somewhere the person has to type out.
func (a *API) health(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, http.StatusOK, map[string]any{"ok": true, "home": a.home})
}

// listSessions answers newest-first. Archived sessions are hidden
// unless asked for: archiving is the "stop showing me this" gesture.
func (a *API) listSessions(w http.ResponseWriter, r *http.Request) {
	infos, err := a.sup.List()
	if err != nil {
		writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: list sessions: %w", err))
		return
	}
	all := r.URL.Query().Get("all") == "1"
	cwd := r.URL.Query().Get("cwd")
	rows := make([]Row, 0, len(infos))
	seen := map[string]bool{}
	for _, in := range infos {
		seen[in.ID] = true
		if cwd != "" && in.Cwd != cwd {
			continue
		}
		row := a.row(in)
		if row.Archived && !all {
			continue
		}
		rows = append(rows, row)
	}
	// Queued and booting children have no history file yet, so List
	// cannot see them.
	if cwd == "" {
		rows = append(rows, a.pendingRows(seen)...)
	}
	sort.SliceStable(rows, func(i, j int) bool { return rows[i].Modified.After(rows[j].Modified) })
	writeJSONTagged(w, r, map[string]any{"sessions": rows})
}

// getSession is the row plus a transcript slice. since/limit exist so a
// client that already holds part of the transcript can catch up without
// re-reading a long session.
func (a *API) getSession(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	in, ok := a.info(id)
	if !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return
	}
	since, err := intParam(r, "since")
	if err != nil {
		writeErr(w, http.StatusBadRequest, err)
		return
	}
	limit, err := intParam(r, "limit")
	if err != nil {
		writeErr(w, http.StatusBadRequest, err)
		return
	}
	entries, err := a.sup.Entries(id)
	if err != nil {
		writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: read session %q: %w", id, err))
		return
	}
	lines := Transcript(entries, since, int(limit))
	if lines == nil {
		lines = []Line{}
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"session": a.rowFrom(in, entries),
		"entries": lines,
	})
}

func (a *API) createSession(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Cwd     string `json:"cwd"`
		Prompt  string `json:"prompt"`
		Mode    string `json:"mode"`
		Project string `json:"project"` // project slug, project mode only
		// Background agent fields: a session starting a child.
		Slug          string `json:"slug"`
		Model         string `json:"model"`
		SpawnedBy     string `json:"spawnedBy"`
		MaxPerSession int    `json:"maxPerSession"`
		MaxRunning    int    `json:"maxRunning"`
		// RequestID makes a retried create safe: the same id makes one
		// session (CreateOnce). Local sessions only.
		RequestID string `json:"requestId"`
	}
	if !decode(w, r, &body) {
		return
	}
	if body.SpawnedBy != "" {
		a.createChild(w, CreateOptions{Cwd: body.Cwd, Prompt: body.Prompt, Slug: body.Slug, Model: body.Model, SpawnedBy: body.SpawnedBy}, body.MaxPerSession, body.MaxRunning)
		return
	}
	switch body.Mode {
	case "", "local":
	case "project":
		a.createProjectSession(w, body.Prompt, body.Project, body.MaxPerSession, body.MaxRunning)
		return
	default:
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: unknown mode %q (have local, project)", body.Mode))
		return
	}
	if body.Cwd == "" {
		writeErr(w, http.StatusBadRequest, errors.New("serve: api: cwd is required"))
		return
	}
	// A child spawned into a missing directory dies with a bare exec
	// error; catching it here gives the caller the real reason.
	if st, err := os.Stat(body.Cwd); err != nil || !st.IsDir() {
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: cwd %q is not a directory", body.Cwd))
		return
	}
	opt := CreateOptions{Cwd: body.Cwd, Prompt: body.Prompt}
	if body.RequestID != "" {
		id, made, err := a.sup.CreateOnce(opt, body.RequestID)
		if err != nil {
			writeErr(w, statusFor(err), fmt.Errorf("serve: api: create session: %w", err))
			return
		}
		code := http.StatusCreated
		if !made {
			code = http.StatusOK
		}
		a.writeRow(w, code, id)
		return
	}
	id, err := a.sup.Create(opt)
	if err != nil {
		writeErr(w, statusFor(err), fmt.Errorf("serve: api: create session: %w", err))
		return
	}
	a.writeRow(w, http.StatusCreated, id)
}

// prompt hands one line to the child's stdin. The child decides whether
// that line steers a running turn or starts a new one — the API never
// classifies, because only the loop knows if a turn is open.
func (a *API) prompt(w http.ResponseWriter, r *http.Request) {
	// An armed ask eats the next stdin line, so a prompt sent now would
	// silently become the answer. Refuse it here rather than let the
	// child misread it.
	if id := r.PathValue("id"); a.sup.PendingAsk(id) != nil {
		writeErr(w, http.StatusConflict, fmt.Errorf("serve: api: session %q has a pending ask: answer it first", id))
		return
	}
	id := r.PathValue("id")
	var body struct {
		Text string `json:"text"`
		// ID is the client's request id: a retry under the same one is
		// written at most once (requests.go). Without one the line is
		// written every time.
		ID string `json:"id"`
	}
	if !decode(w, r, &body) {
		return
	}
	if _, ok := a.info(id); !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return
	}
	var err error
	if body.ID == "" {
		err = a.sup.Send(id, body.Text)
	} else {
		err = a.sup.SendOnce(id, body.ID, body.Text)
	}
	if err != nil {
		writeErr(w, statusFor(err), fmt.Errorf("serve: api: session %q: %w", id, err))
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

// promptState answers what serve knows about a prompt's request id, so
// a page whose answer never came can tell "Not sent" from "sent".
func (a *API) promptState(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	if _, ok := a.info(id); !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"state": a.sup.PromptState(id, r.PathValue("rid"))})
}

// answer replies to the armed ask. A client that names the question it
// answered is refused when that question has since been replaced, so a
// late answer never lands on a newer question nobody read.
func (a *API) answer(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	var body struct {
		Text string `json:"text"`
		Ask  string `json:"ask"`
	}
	if !decode(w, r, &body) {
		return
	}
	if _, ok := a.info(id); !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return
	}
	if p := a.sup.PendingAsk(id); body.Ask != "" && p != nil && p.ID != body.Ask {
		writeErr(w, http.StatusConflict, fmt.Errorf("serve: api: session %q: that question expired; a newer one is pending", id))
		return
	}
	if err := a.sup.Answer(id, body.Text); err != nil {
		writeErr(w, statusFor(err), fmt.Errorf("serve: api: session %q: %w", id, err))
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

func (a *API) textVerb(w http.ResponseWriter, r *http.Request, fn func(id, text string) error) {
	id := r.PathValue("id")
	var body struct {
		Text string `json:"text"`
	}
	if !decode(w, r, &body) {
		return
	}
	if _, ok := a.info(id); !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return
	}
	if err := fn(id, body.Text); err != nil {
		writeErr(w, statusFor(err), fmt.Errorf("serve: api: session %q: %w", id, err))
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

func (a *API) interrupt(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	if _, ok := a.info(id); !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return
	}
	if err := a.sup.Interrupt(id); err != nil {
		writeErr(w, statusFor(err), fmt.Errorf("serve: api: interrupt %q: %w", id, err))
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

// rename stores the title beside history rather than rewriting it:
// history is an append-only record of what happened.
func (a *API) rename(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	var body struct {
		Title string `json:"title"`
	}
	if !decode(w, r, &body) {
		return
	}
	a.metaVerb(w, id, func() error { return a.sup.SetTitle(id, body.Title) })
}

func (a *API) archive(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	var body struct {
		StopChildren bool `json:"stopChildren"`
	}
	// The body is optional: a bare POST archives as it always did.
	if r.ContentLength != 0 {
		if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, maxBody)).Decode(&body); err != nil && !errors.Is(err, io.EOF) {
			writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: bad request body: %w", err))
			return
		}
	}
	a.metaVerb(w, id, func() error {
		// Parent first: once it is archived and dead it can spawn no
		// agent, so the list of children to end is complete. Ending
		// them first let the live parent start one the list never saw.
		if err := a.sup.SetArchived(id, true); err != nil || !body.StopChildren {
			return err
		}
		for _, c := range a.sup.Children(id) {
			if err := a.sup.EndChild(c.ID); err != nil {
				return err
			}
		}
		return nil
	})
}

func (a *API) unarchive(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	a.metaVerb(w, id, func() error { return a.sup.SetArchived(id, false) })
}

func (a *API) ack(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	a.metaVerb(w, id, func() error { return a.sup.Acknowledge(id) })
}

func (a *API) metaVerb(w http.ResponseWriter, id string, fn func() error) {
	if _, ok := a.info(id); !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return
	}
	if err := fn(); err != nil {
		writeErr(w, statusFor(err), fmt.Errorf("serve: api: session %q: %w", id, err))
		return
	}
	a.writeRow(w, http.StatusOK, id)
}

// events streams the session's child output. It deliberately never
// spawns a child: watching a session must not restart it.
func (a *API) events(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	fl, ok := w.(http.Flusher)
	if !ok {
		http.Error(w, "no streaming", http.StatusInternalServerError)
		return
	}
	if _, ok := a.info(id); !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return
	}
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	fmt.Fprint(w, ": open\n\n")
	fl.Flush()

	// Subscribe before replaying the ring: an event that lands in
	// between is then duplicated (harmless, ids are monotonic) rather
	// than lost.
	ch, unsub := a.sup.Subscribe(id)
	defer unsub()
	for _, ev := range a.sup.Recent(id) {
		writeEvent(w, fl, ev)
	}

	t := time.NewTicker(heartbeat)
	defer t.Stop()
	for {
		select {
		case <-r.Context().Done():
			return
		case ev, ok := <-ch:
			if !ok {
				return // supervisor closed the subscription
			}
			writeEvent(w, fl, ev)
		case <-t.C:
			fmt.Fprint(w, ": ping\n\n")
			fl.Flush()
		}
	}
}

func writeEvent(w http.ResponseWriter, fl http.Flusher, ev Event) {
	b, err := json.Marshal(ev)
	if err != nil {
		return // an event that will not marshal is not worth killing the stream over
	}
	// No "event:" line on purpose. EventSource has no wildcard listener,
	// so a named event only reaches a client that registered that exact
	// name — and bough's kind vocabulary is open-ended (sub:*, job,
	// usage, kinds a later plugin adds). Naming the frame would drop
	// those silently. The kind rides in the payload, where a client
	// cannot miss it.
	// Seq 0 is an ephemeral frame (a streaming delta): it holds no
	// place in the session's sequence, so it gets no SSE id line
	// either — a reconnecting EventSource must never ask to resume
	// from text that was never recorded.
	if ev.Seq == 0 {
		fmt.Fprintf(w, "data: %s\n\n", b)
	} else {
		fmt.Fprintf(w, "id: %d\ndata: %s\n\n", ev.Seq, b)
	}
	fl.Flush()
}

// row derives a session's status from its entries and the lease. A
// read failure is not fatal: the row still names the session, with the
// status it can honestly claim.
func (a *API) row(in history.SessionInfo) Row {
	return a.rowOf(in, a.digest(in))
}

// rowFrom builds a row from entries already in hand (a caller that just
// read the transcript for its own reasons).
func (a *API) rowFrom(in history.SessionInfo, entries []history.Entry) Row {
	return a.rowOf(in, digestOf(entries, in.ModTime))
}

// rowOf joins what the transcript says (the digest, cached per file)
// with what only this moment can say: whether the child is alive, the
// session's meta, the orb, the clock.
func (a *API) rowOf(in history.SessionInfo, d *rowDigest) Row {
	meta := a.sup.Meta(in.ID)
	live := a.sup.Live(in.ID)
	st, ask := d.statusDead, d.askDead
	var jobs []Job
	if live {
		st, ask, jobs = d.statusLive, d.askLive, d.jobsLive
	}
	title := meta.Title
	if title == "" {
		title = in.Title
	}
	// meta.Model is only set when the model was changed FROM here, so a
	// session left on its configured model reported nothing and the
	// picker could only say "as configured" — which names no model and
	// tells you nothing. Every assistant entry records the model that
	// answered, so the session says what it is actually running.
	model := meta.Model
	if model == "" {
		model = d.model
	}
	var configured *ModelDefault
	if meta.Model == "" && !d.switched {
		var c ModelDefault
		// A project session's child reads its config inside the orb,
		// which serve cannot resolve from here.
		if d.mode != "project" && a.defaults != nil {
			c = a.defaults(in.Cwd)
		}
		if c.Model == "" {
			c.Model = d.ownModel
		}
		if c.Plugin != "" || c.Model != "" {
			configured = &c
		}
	}
	// A project session's project is the one its history names: it is
	// where its orb and its worktrees came from, so the membership serve
	// stores cannot contradict it.
	project := meta.Project
	if d.mode == "project" && d.project != "" {
		project = d.project
	}
	var rowOrb *RowOrb
	if d.mode == "project" {
		st := a.orbState(in.ID)
		var live []int
		for _, p := range st.Portals {
			if p.Live {
				live = append(live, p.Guest)
			}
		}
		rowOrb = &RowOrb{Project: d.project, Status: st.Status, Up: st.Up, Portals: live, Phase: st.Phase}
		// The step in progress is timed from its own start, so a row can
		// say how long a build has been going without a second fetch.
		if n := len(st.Phases); n > 0 {
			if last := st.Phases[n-1]; last.EndedAt.IsZero() {
				at := last.StartedAt
				rowOrb.PhaseAt = &at
			}
		}
	}
	now := time.Now()
	trouble := d.troubled(st, meta.Ack, now, in.Background)
	return Row{
		ID:         in.ID,
		Title:      title,
		Summary:    in.Summary,
		Cwd:        in.Cwd,
		Repo:       in.Repo,
		Branch:     in.Branch,
		Status:     st,
		Live:       live,
		Archived:   meta.Archived,
		Entries:    in.Entries,
		Modified:   in.ModTime,
		LastAt:     d.lastAt,
		Ask:        ask,
		Model:      model,
		Effort:     meta.Effort,
		Configured: configured,
		Project:    project,
		StartedIn:  a.sup.StartedIn(in.ID),
		Jobs:       jobs,
		Cache:      d.cacheFor(model),
		Trouble:    trouble,
		Unseen:     d.unseen(st, trouble, meta.Ack, now, in.Origin),

		TestsFailed: d.testsFailed,
		TestsAt:     d.testsAt,
		Turns:       d.turns,

		Background: in.Background,
		Empty:      !d.hasInput,

		Mode:     d.mode,
		Writable: a.writableRoot(d.mode, in.Cwd),
		Orb:      rowOrb,

		SpawnedBy: firstDir(in.SpawnedBy, meta.SpawnedBy),
		Agents:    a.agentCount(in.ID),
	}
}

func lastAt(entries []history.Entry, fallback time.Time) time.Time {
	for i := len(entries) - 1; i >= 0; i-- {
		if !entries[i].At.IsZero() {
			return entries[i].At
		}
	}
	return fallback
}

// lastModel is the model that answered most recently, or "" for a
// session that has not had a reply yet. A /model switch's one-line echo
// ("model: plugin · id") counts too: before the next reply it is what
// the session runs as.
func lastModel(entries []history.Entry) string {
	for i := len(entries) - 1; i >= 0; i-- {
		if entries[i].Kind == "system" {
			text, _ := entries[i].Data["text"].(string)
			if rest, ok := strings.CutPrefix(text, "model: "); ok && !strings.Contains(rest, "\n") {
				if _, m, ok := strings.Cut(rest, " · "); ok && m != "" {
					return m
				}
			}
			continue
		}
		// An engine session names its model on the quiet "engine" entry
		// it writes at every coordinator build, before any reply.
		if entries[i].Kind != "assistant" && entries[i].Kind != "engine" {
			continue
		}
		if m, ok := entries[i].Data["model"].(string); ok && m != "" {
			return m
		}
	}
	return ""
}

// ownModel is what the session's own llm row answered as before any
// /model swap, and whether one happened. The swap's "model" entry is
// what a resumed child replays over bough.yml, so after one the session
// never runs its configured row again.
func ownModel(entries []history.Entry) (model string, switched bool) {
	for _, e := range entries {
		switch e.Kind {
		case "model":
			return model, true
		case "engine", "assistant":
			if m, _ := e.Data["model"].(string); model == "" {
				model = m
			}
		}
	}
	return model, false
}

// writeRow re-reads the session so the reply reflects the state after
// the verb rather than the state the caller assumed.
func (a *API) writeRow(w http.ResponseWriter, code int, id string) {
	in, ok := a.info(id)
	if !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return
	}
	writeJSON(w, code, map[string]any{"session": a.row(in)})
}

func (a *API) info(id string) (history.SessionInfo, bool) {
	if id == "" {
		return history.SessionInfo{}, false
	}
	infos, err := a.sup.List()
	if err != nil {
		return history.SessionInfo{}, false
	}
	for _, in := range infos {
		if in.ID == id {
			return in, true
		}
	}
	return history.SessionInfo{}, false
}

func decode(w http.ResponseWriter, r *http.Request, v any) bool {
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, maxBody)).Decode(v); err != nil {
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: bad request body: %w", err))
		return false
	}
	return true
}

func intParam(r *http.Request, name string) (int64, error) {
	raw := r.URL.Query().Get(name)
	if raw == "" {
		return 0, nil
	}
	n, err := strconv.ParseInt(raw, 10, 64)
	if err != nil {
		return 0, fmt.Errorf("serve: api: bad %s %q: %w", name, raw, err)
	}
	return n, nil
}

// statusFor maps the supervisor's sentinels onto HTTP: a pending ask or
// an archived session is a conflict with the session's current state,
// not a malformed request.
func statusFor(err error) int {
	switch {
	case errors.Is(err, ErrUnknownSession):
		return http.StatusNotFound
	case errors.Is(err, ErrBadAnswer):
		return http.StatusBadRequest
	case errors.Is(err, ErrNoAsk), errors.Is(err, ErrArchived), errors.Is(err, ErrProjectExists), errors.Is(err, ErrProjectSession):
		return http.StatusConflict
	case errors.Is(err, ErrUnknownProject):
		return http.StatusNotFound
	case errors.Is(err, ErrBadName), errors.Is(err, ErrBrokenProject):
		return http.StatusBadRequest
	default:
		return http.StatusInternalServerError
	}
}

func writeJSON(w http.ResponseWriter, code int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(code)
	_ = json.NewEncoder(w).Encode(v)
}

func writeErr(w http.ResponseWriter, code int, err error) {
	writeJSON(w, code, map[string]any{"error": err.Error()})
}

// testsAt is when the last recorded test run ended, or nil.
func testsAt(entries []history.Entry) *time.Time {
	if _, at := lastTest(entries); !at.IsZero() {
		return &at
	}
	return nil
}
