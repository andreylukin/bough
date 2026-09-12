package serve

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"sort"
	"strconv"
	"time"

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
}

// Row is one session as the wire sees it: what history knows, what the
// supervisor knows (lease, rename, archive) and the derived status.
type Row struct {
	ID       string    `json:"id"`
	Title    string    `json:"title"`
	Cwd      string    `json:"cwd"`
	Repo     string    `json:"repo,omitempty"`
	Branch   string    `json:"branch,omitempty"`
	Status   Status    `json:"status"`
	Live     bool      `json:"live"`
	Archived bool      `json:"archived"`
	Entries  int       `json:"entries"`
	Modified time.Time `json:"modified"`
	Ask      *Ask      `json:"ask,omitempty"`
	// Model and Effort are what this session was last ASKED to run as
	// (empty = whatever its own config says). They are not read back
	// from the child, so do not present them as ground truth.
	Model  string `json:"model,omitempty"`
	Effort string `json:"effort,omitempty"`
	// Project is the grouping this conversation was put in, by id.
	Project string `json:"project,omitempty"`
}

// maxBody caps every request body: the API takes prompts and titles,
// never uploads.
const maxBody = 1 << 20

// heartbeat keeps an idle SSE stream alive through proxies and tells a
// client the server is still there while a session sits quiet.
const heartbeat = 15 * time.Second

// NewAPI wires the routes. Method+pattern routing means a wrong method
// on a real path is the mux's own 405, not a 404.
func NewAPI(sup *Supervisor) *API {
	home, _ := os.UserHomeDir()
	a := &API{sup: sup, mux: http.NewServeMux(), home: home}
	a.mux.HandleFunc("GET /api/health", a.health)
	a.mux.HandleFunc("GET /api/sessions", a.listSessions)
	a.mux.HandleFunc("POST /api/sessions", a.createSession)
	a.mux.HandleFunc("GET /api/sessions/{id}", a.getSession)
	a.mux.HandleFunc("POST /api/sessions/{id}/prompt", a.prompt)
	a.mux.HandleFunc("POST /api/sessions/{id}/answer", a.answer)
	a.mux.HandleFunc("POST /api/sessions/{id}/interrupt", a.interrupt)
	a.mux.HandleFunc("POST /api/sessions/{id}/rename", a.rename)
	a.mux.HandleFunc("POST /api/sessions/{id}/archive", a.archive)
	a.mux.HandleFunc("POST /api/sessions/{id}/unarchive", a.unarchive)
	a.mux.HandleFunc("POST /api/sessions/{id}/model", a.setModel)
	a.mux.HandleFunc("POST /api/sessions/{id}/effort", a.setEffort)
	a.mux.HandleFunc("GET /api/models", a.models)
	a.mux.HandleFunc("GET /api/skills", a.skills)
	a.mux.HandleFunc("GET /api/hooks", a.hooks)
	a.mux.HandleFunc("GET /api/sessions/{id}/context", a.sessionContext)
	a.mux.HandleFunc("POST /api/off", a.setOff)
	a.mux.HandleFunc("GET /api/hooks/file", a.hookFile)
	a.mux.HandleFunc("PUT /api/hooks/file", a.putHookFile)
	a.mux.HandleFunc("POST /api/hooks/dryrun", a.dryrun)
	a.mux.HandleFunc("GET /api/projects", a.listProjects)
	a.mux.HandleFunc("POST /api/projects", a.createProject)
	a.mux.HandleFunc("POST /api/projects/{id}/rename", a.renameProject)
	a.mux.HandleFunc("DELETE /api/projects/{id}", a.deleteProject)
	a.mux.HandleFunc("POST /api/sessions/{id}/project", a.assignProject)
	a.mux.HandleFunc("GET /api/sessions/{id}/events", a.events)
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

func (a *API) ServeHTTP(w http.ResponseWriter, r *http.Request) { a.mux.ServeHTTP(w, r) }

func (a *API) health(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
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
	for _, in := range infos {
		if cwd != "" && in.Cwd != cwd {
			continue
		}
		row := a.row(in)
		if row.Archived && !all {
			continue
		}
		rows = append(rows, row)
	}
	sort.SliceStable(rows, func(i, j int) bool { return rows[i].Modified.After(rows[j].Modified) })
	writeJSON(w, http.StatusOK, map[string]any{"sessions": rows})
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
		Cwd    string `json:"cwd"`
		Prompt string `json:"prompt"`
	}
	if !decode(w, r, &body) {
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
	id, err := a.sup.Create(body.Cwd, body.Prompt)
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
	a.textVerb(w, r, a.sup.Send)
}

func (a *API) answer(w http.ResponseWriter, r *http.Request) {
	a.textVerb(w, r, a.sup.Answer)
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
	a.metaVerb(w, id, func() error { return a.sup.SetArchived(id, true) })
}

func (a *API) unarchive(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	a.metaVerb(w, id, func() error { return a.sup.SetArchived(id, false) })
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
	fmt.Fprintf(w, "id: %d\ndata: %s\n\n", ev.Seq, b)
	fl.Flush()
}

// row derives a session's status from its entries and the lease. A
// read failure is not fatal: the row still names the session, with the
// status it can honestly claim.
func (a *API) row(in history.SessionInfo) Row {
	entries, err := a.sup.Entries(in.ID)
	if err != nil {
		entries = nil
	}
	return a.rowFrom(in, entries)
}

func (a *API) rowFrom(in history.SessionInfo, entries []history.Entry) Row {
	meta := a.sup.Meta(in.ID)
	live := a.sup.Live(in.ID)
	st, ask := StatusOf(entries, live)
	title := meta.Title
	if title == "" {
		title = in.Title
	}
	return Row{
		ID:       in.ID,
		Title:    title,
		Cwd:      in.Cwd,
		Repo:     in.Repo,
		Branch:   in.Branch,
		Status:   st,
		Live:     live,
		Archived: meta.Archived,
		Entries:  in.Entries,
		Modified: in.ModTime,
		Ask:      ask,
		Model:    meta.Model,
		Effort:   meta.Effort,
		Project:  meta.Project,
	}
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
	case errors.Is(err, ErrNoAsk), errors.Is(err, ErrArchived):
		return http.StatusConflict
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
