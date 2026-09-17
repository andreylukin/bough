package serve

// Orbs: the container side of project sessions, as serve sees it.
//
// serve never owns an orb. The child builds, starts and records its own
// (state.json, written only by the child); serve reads that record,
// edits the project definition files, runs image builds on request and
// can stop a container. Everything here is derived on each request so a
// page never disagrees with the files on disk.

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"sort"
	"strings"
	"syscall"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/plugins/history"
)

// OrbSummary is a definition's image at a glance.
type OrbSummary struct {
	Slug  string `json:"slug"`
	Image string `json:"image"`           // tag for the CURRENT hash
	Built bool   `json:"built"`           // that tag exists in the runtime
	Build string `json:"build,omitempty"` // orb.Build.State
	Error string `json:"error,omitempty"` // definition parse error
}

// OrbState is a session's state.json as the wire sees it. Up says the
// container is running whatever the status: a failed setup (resume.sh
// exited non-zero) leaves it up, and Stop must still be offered.
type OrbState struct {
	orb.State
	Up bool `json:"up,omitempty"`
}

// OrbRuntime says whether the engine can be used right now.
type OrbRuntime struct {
	Name      string `json:"name"`
	Available bool   `json:"available"`
	Error     string `json:"error,omitempty"`
}

// OrbDetail is everything the project orb surface shows.
type OrbDetail struct {
	Project Project           `json:"project"`
	Files   map[string]string `json:"files"`
	Hash    string            `json:"hash"`
	Summary OrbSummary        `json:"orb"`
	Build   orb.Build         `json:"build"`
	Orbs    []OrbState        `json:"orbs"`
	Runtime OrbRuntime        `json:"runtime"`
}

// projectRow is a label plus its orb, present only when a slug is set.
type projectRow struct {
	Project
	Orb *OrbSummary `json:"orb,omitempty"`
}

const (
	maxLogChunk    = 256 << 10
	runtimeTimeout = 5 * time.Second
)

func (a *API) routeOrbs() {
	a.mux.HandleFunc("POST /api/projects/{id}/orb", a.attachOrb)
	a.mux.HandleFunc("DELETE /api/projects/{id}/orb", a.detachOrb)
	a.mux.HandleFunc("GET /api/projects/{id}/orb", a.orbDetail)
	a.mux.HandleFunc("PUT /api/projects/{id}/orb/files/{name}", a.putOrbFile)
	a.mux.HandleFunc("POST /api/projects/{id}/orb/build", a.buildOrb)
	a.mux.HandleFunc("GET /api/projects/{id}/orb/build/log", a.buildLog)
	a.mux.HandleFunc("GET /api/sessions/{id}/orb", a.sessionOrb)
	a.mux.HandleFunc("GET /api/sessions/{id}/orb/log", a.sessionOrbLog)
	a.mux.HandleFunc("GET /api/sessions/{id}/orb/build/log", a.sessionBuildLog)
	a.mux.HandleFunc("POST /api/sessions/{id}/orb/stop", a.stopOrb)
}

// sessionMode reads the mode off the first meta entry. No meta, or a
// meta with no mode, is local: every session before modes existed was.
func sessionMode(entries []history.Entry) (mode, slug string) {
	for _, e := range entries {
		if e.Kind != "meta" {
			continue
		}
		if m, _ := e.Data["mode"].(string); m == "project" {
			p, _ := e.Data["project"].(string)
			return "project", p
		}
		return "local", ""
	}
	return "local", ""
}

// orbState is state.json with the status serve can honestly claim: a
// "running" orb whose owner is gone, or whose container serve stopped
// since the child last wrote, is stopped.
func (a *API) orbState(session string) OrbState {
	s, err := orb.ReadState(a.sup.Home(), session)
	if err != nil || s.Session == "" {
		return OrbState{}
	}
	st := OrbState{State: s}
	switch st.Status {
	case orb.StatusRunning:
		st.Up = true
	case orb.StatusFailed:
		// Only a failed orb asks the runtime: the list polls every row.
		st.Up = a.containerUp(session)
		return st
	case orb.StatusStarting:
		st.Up = a.containerUp(session)
	default:
		return st
	}
	if !a.ownerAlive(session, st.State) {
		st.Status = orb.StatusStopped
		st.Up = st.Up && a.containerUp(session)
		return st
	}
	a.sup.mu.Lock()
	at, stopped := a.sup.stoppedAt[session]
	a.sup.mu.Unlock()
	if stopped && !st.UpdatedAt.After(at) {
		st.Status, st.Up = orb.StatusStopped, false
	}
	return st
}

// containerUp asks the runtime whether a session's container runs.
func (a *API) containerUp(session string) bool {
	if a.sup.Runtime() == nil {
		return false
	}
	ctx, cancel := context.WithTimeout(context.Background(), runtimeTimeout)
	defer cancel()
	cs, err := a.sup.Runtime().Inspect(ctx, container.OrbName(session))
	return err == nil && cs == container.StateRunning
}

// ownerAlive decides whether state.json's PID still owns the orb. The
// supervisor's own child table wins when it knows the session, because
// a bare kill(pid, 0) cannot tell a reused pid from the real owner; a
// pid serve did not spawn only counts if it wrote after serve started.
func (a *API) ownerAlive(session string, st orb.State) bool {
	if pid := a.sup.childPID(session); pid != 0 {
		return pid == st.PID
	}
	if st.PID <= 0 || !pidAlive(st.PID) {
		return false
	}
	return st.UpdatedAt.After(a.sup.started)
}

func pidAlive(pid int) bool {
	p, err := os.FindProcess(pid)
	if err != nil {
		return false
	}
	return p.Signal(syscall.Signal(0)) == nil
}

// orbSummary is best-effort: a broken definition or an unusable runtime
// still yields a summary the page can show, never an error response.
func (a *API) orbSummary(ctx context.Context, slug string) (OrbSummary, string) {
	sum := OrbSummary{Slug: slug}
	home := a.sup.Home()
	if b, err := orb.ReadBuild(home, slug); err == nil {
		sum.Build = b.State
	}
	if msg := a.buildFailure(slug); msg != "" {
		sum.Build = "failed"
	}
	if a.building(slug) {
		sum.Build = "building"
	}
	p, err := projectdef.Load(home, slug)
	if err != nil {
		sum.Error = err.Error()
		return sum, ""
	}
	hash, err := projectdef.ImageHash(home, p)
	if err != nil {
		sum.Error = err.Error()
		return sum, ""
	}
	sum.Image = projectdef.ImageTag(slug, hash)
	ctx, cancel := context.WithTimeout(ctx, runtimeTimeout)
	defer cancel()
	if ok, err := a.sup.Runtime().ImageExists(ctx, sum.Image); err == nil {
		sum.Built = ok
	}
	// A failure recorded for another tag says nothing about this one.
	if sum.Built && sum.Build == "failed" {
		sum.Build = "ok"
	}
	return sum, hash
}

func (a *API) building(slug string) bool {
	a.sup.mu.Lock()
	defer a.sup.mu.Unlock()
	return a.sup.building[slug]
}

// buildFailure is why the last build this serve started failed, "" when
// it did not.
func (a *API) buildFailure(slug string) string {
	a.sup.mu.Lock()
	defer a.sup.mu.Unlock()
	return a.sup.buildErr[slug]
}

func (a *API) listProjects(w http.ResponseWriter, r *http.Request) {
	ps := a.sup.Projects()
	out := make([]projectRow, 0, len(ps))
	for _, p := range ps {
		row := projectRow{Project: p}
		if p.Slug != "" {
			sum, _ := a.orbSummary(r.Context(), p.Slug)
			row.Orb = &sum
		}
		out = append(out, row)
	}
	writeJSON(w, http.StatusOK, map[string]any{"projects": out})
}

// labelWithOrb resolves a label that must carry a definition, answering
// 404 itself when it does not.
func (a *API) labelWithOrb(w http.ResponseWriter, id string) (Project, bool) {
	p, ok := a.sup.Project(id)
	if !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: no project %q", id))
		return Project{}, false
	}
	if p.Slug == "" {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: project %q has no orb", p.Name))
		return Project{}, false
	}
	return p, true
}

var slugJunk = regexp.MustCompile(`[^a-z0-9]+`)

// slugify turns a label name into a definition directory name.
func slugify(name string) string {
	s := strings.Trim(slugJunk.ReplaceAllString(strings.ToLower(name), "-"), "-")
	if len(s) > 63 {
		s = strings.TrimRight(s[:63], "-")
	}
	return s
}

func (a *API) attachOrb(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	var body struct {
		Slug string `json:"slug"`
	}
	// An empty body means "use the name": allowed.
	if r.ContentLength != 0 && !decode(w, r, &body) {
		return
	}
	p, ok := a.sup.Project(id)
	if !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: no project %q", id))
		return
	}
	slug := strings.TrimSpace(body.Slug)
	if slug == "" {
		slug = slugify(p.Name)
	}
	if err := projectdef.ValidSlug(slug); err != nil {
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: attach orb: %w", err))
		return
	}
	for _, o := range a.sup.Projects() {
		if o.ID != id && o.Slug == slug {
			writeErr(w, http.StatusConflict, fmt.Errorf("serve: api: %q is already the orb of %q", slug, o.Name))
			return
		}
	}
	home := a.sup.Home()
	// An existing directory is attached as it stands, broken yaml and
	// all: the person is here to fix it, not to lose it to a skeleton.
	if _, err := os.Stat(filepath.Join(projectdef.Root(home), slug, projectdef.FileYAML)); errors.Is(err, os.ErrNotExist) {
		if _, err := projectdef.Create(home, slug); err != nil {
			writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: create orb %q: %w", slug, err))
			return
		}
	}
	p, err := a.sup.SetProjectSlug(id, slug)
	if err != nil {
		code := http.StatusInternalServerError
		if errors.Is(err, ErrSlugTaken) {
			code = http.StatusConflict
		}
		writeErr(w, code, err)
		return
	}
	sum, _ := a.orbSummary(r.Context(), slug)
	writeJSON(w, http.StatusOK, map[string]any{"project": p, "orb": sum})
}

func (a *API) detachOrb(w http.ResponseWriter, r *http.Request) {
	if _, err := a.sup.SetProjectSlug(r.PathValue("id"), ""); err != nil {
		code := http.StatusInternalServerError
		if errors.Is(err, ErrUnknownProject) {
			code = http.StatusNotFound
		}
		writeErr(w, code, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

func (a *API) orbDetail(w http.ResponseWriter, r *http.Request) {
	p, ok := a.labelWithOrb(w, r.PathValue("id"))
	if !ok {
		return
	}
	home := a.sup.Home()
	d := OrbDetail{Project: p, Files: map[string]string{}, Orbs: []OrbState{}}
	for _, name := range projectdef.EditableFiles {
		text, err := projectdef.ReadFile(home, p.Slug, name)
		if err != nil {
			writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: read %s/%s: %w", p.Slug, name, err))
			return
		}
		d.Files[name] = text
	}
	d.Summary, d.Hash = a.orbSummary(r.Context(), p.Slug)
	if b, err := orb.ReadBuild(home, p.Slug); err == nil {
		d.Build = b
	}
	if msg := a.buildFailure(p.Slug); msg != "" {
		d.Build.State, d.Build.Error = "failed", msg
	}
	// The page polls the log only while this says building: build.json
	// still holds the previous build until EnsureImage rewrites it.
	if a.building(p.Slug) {
		d.Build.State, d.Build.Error = "building", ""
		d.Build.Tag = d.Summary.Image
	}
	d.Orbs = a.orbsOf(p.Slug)
	rt := a.sup.Runtime()
	d.Runtime.Name = rt.Name()
	ctx, cancel := context.WithTimeout(r.Context(), runtimeTimeout)
	defer cancel()
	if err := rt.Available(ctx); err != nil {
		d.Runtime.Error = err.Error()
	} else {
		d.Runtime.Available = true
	}
	writeJSON(w, http.StatusOK, d)
}

// orbsOf lists every session orb recorded for slug, newest first. The
// orbs directory also holds images/ and cache/, which carry no
// state.json and so fall out on their own.
func (a *API) orbsOf(slug string) []OrbState {
	dirs, _ := os.ReadDir(filepath.Join(a.sup.Home(), ".bough", "orbs"))
	out := []OrbState{}
	for _, d := range dirs {
		if !d.IsDir() {
			continue
		}
		st := a.orbState(d.Name())
		if st.Session != "" && st.Project == slug {
			out = append(out, st)
		}
	}
	sort.SliceStable(out, func(i, j int) bool { return out[i].UpdatedAt.After(out[j].UpdatedAt) })
	return out
}

func (a *API) putOrbFile(w http.ResponseWriter, r *http.Request) {
	p, ok := a.labelWithOrb(w, r.PathValue("id"))
	if !ok {
		return
	}
	name := r.PathValue("name")
	if !slices.Contains(projectdef.EditableFiles, name) {
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: %q is not an orb file (have %s)", name, strings.Join(projectdef.EditableFiles, ", ")))
		return
	}
	var body struct {
		Text string `json:"text"`
	}
	if !decode(w, r, &body) {
		return
	}
	if err := projectdef.WriteFile(a.sup.Home(), p.Slug, name, body.Text); err != nil {
		// A validation list is for the person at the editor; send it bare.
		if inv := (*projectdef.Invalid)(nil); errors.As(err, &inv) {
			writeErr(w, http.StatusBadRequest, inv)
			return
		}
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: save %s/%s: %w", p.Slug, name, err))
		return
	}
	sum, _ := a.orbSummary(r.Context(), p.Slug)
	writeJSON(w, http.StatusOK, map[string]any{"ok": true, "orb": sum})
}

// buildOrb starts an image build and returns at once; the page follows
// it through the log endpoint. orb.EnsureImage's own lock keeps this
// from racing a child that is building the same image.
func (a *API) buildOrb(w http.ResponseWriter, r *http.Request) {
	p, ok := a.labelWithOrb(w, r.PathValue("id"))
	if !ok {
		return
	}
	home := a.sup.Home()
	def, err := projectdef.Load(home, p.Slug)
	if err != nil {
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: build %s: %w", p.Slug, err))
		return
	}
	hash, err := projectdef.ImageHash(home, def)
	if err != nil {
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: build %s: %w", p.Slug, err))
		return
	}
	a.sup.mu.Lock()
	if a.sup.building[p.Slug] {
		a.sup.mu.Unlock()
		writeErr(w, http.StatusConflict, fmt.Errorf("serve: api: %s is already building", p.Slug))
		return
	}
	a.sup.building[p.Slug] = true
	delete(a.sup.buildErr, p.Slug)
	a.sup.mu.Unlock()

	rt := a.sup.Runtime()
	go func() {
		defer func() {
			a.sup.mu.Lock()
			delete(a.sup.building, p.Slug)
			a.sup.mu.Unlock()
		}()
		// Not the request's context: the build outlives the 202. Remote
		// repos are cloned first, as orb.Open does, or the tag would be
		// computed without their lockfiles.
		bctx := context.Background()
		err := orb.SyncRepos(bctx, home, def)
		if err == nil {
			_, err = orb.EnsureImage(bctx, rt, home, def, nil)
		}
		a.sup.mu.Lock()
		if err != nil {
			// A failure before build.json is written (clone, lock, image
			// check) would otherwise leave the page showing the previous
			// build as if nothing happened.
			a.sup.buildErr[p.Slug] = err.Error()
		}
		a.sup.mu.Unlock()
	}()
	writeJSON(w, http.StatusAccepted, map[string]any{"build": orb.Build{
		Tag:       projectdef.ImageTag(p.Slug, hash),
		Hash:      hash,
		State:     "building",
		StartedAt: time.Now(),
	}})
}

func (a *API) buildLog(w http.ResponseWriter, r *http.Request) {
	p, ok := a.labelWithOrb(w, r.PathValue("id"))
	if !ok {
		return
	}
	offset, err := intParam(r, "offset")
	if err != nil || offset < 0 {
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: bad offset %q", r.URL.Query().Get("offset")))
		return
	}
	home := a.sup.Home()
	state := ""
	if b, err := orb.ReadBuild(home, p.Slug); err == nil {
		state = b.State
	}
	msg := a.buildFailure(p.Slug)
	if msg != "" {
		state = "failed"
	}
	// Until EnsureImage writes build.json the build this serve started is
	// still building; a poller must not read "" and stop.
	if a.building(p.Slug) {
		state = "building"
	}
	text := ""
	if f, err := os.Open(orb.ImageLogPath(home, p.Slug)); err == nil {
		defer f.Close()
		if st, err := f.Stat(); err == nil && offset > st.Size() {
			offset = 0 // truncated by a newer build: start over
		}
		buf, _ := io.ReadAll(io.LimitReader(io.NewSectionReader(f, offset, maxLogChunk), maxLogChunk))
		text = string(buf)
		offset += int64(len(buf))
	}
	writeJSON(w, http.StatusOK, map[string]any{"text": text, "offset": offset, "state": state, "error": msg})
}

// sessionOrbLog is the tail of a session's resume.log with the orb's
// status and error, so a "failed" orb can be opened and read instead of
// being a dead label. The script's own output only: bough writes no
// secret values there, and the tail is capped like the build log.
func (a *API) sessionOrbLog(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	if _, ok := a.info(id); !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return
	}
	st := a.orbState(id)
	text := ""
	if b, err := os.ReadFile(filepath.Join(orb.Dir(a.sup.Home(), id), "resume.log")); err == nil {
		if len(b) > maxLogChunk {
			b = b[len(b)-maxLogChunk:]
		}
		text = string(b)
	}
	writeJSON(w, http.StatusOK, map[string]any{"status": st.Status, "error": st.Error, "image": st.Image, "text": text})
}

// sessionBuildLog streams the image build a session is waiting on: the
// project's build.log from offset, in chunks, like the project page's build
// log. A session opened on a project mid-build only said "building".
func (a *API) sessionBuildLog(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	if _, ok := a.info(id); !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return
	}
	offset, err := intParam(r, "offset")
	if err != nil || offset < 0 {
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: bad offset %q", r.URL.Query().Get("offset")))
		return
	}
	st := a.orbState(id)
	if st.Project == "" {
		writeJSON(w, http.StatusOK, map[string]any{"text": "", "offset": 0, "status": st.Status})
		return
	}
	home := a.sup.Home()
	state := ""
	var started, ended time.Time // the web's build timer
	if b, err := orb.ReadBuild(home, st.Project); err == nil {
		state, started, ended = b.State, b.StartedAt, b.EndedAt
	}
	// A build serve just started (Rebuild) is building before EnsureImage
	// rewrites build.json; without this the poller read the last build's
	// "ok" and stopped at once.
	if a.building(st.Project) {
		state = "building"
	}
	text := ""
	if f, err := os.Open(orb.ImageLogPath(home, st.Project)); err == nil {
		defer f.Close()
		if fi, err := f.Stat(); err == nil && offset > fi.Size() {
			offset = 0 // a newer build truncated it: start over
		}
		buf, _ := io.ReadAll(io.LimitReader(io.NewSectionReader(f, offset, maxLogChunk), maxLogChunk))
		text = string(buf)
		offset += int64(len(buf))
	}
	resp := map[string]any{"text": text, "offset": offset, "state": state, "status": st.Status}
	if !started.IsZero() {
		resp["startedAt"] = started
	}
	if !ended.IsZero() && state != "building" {
		resp["endedAt"] = ended
	}
	writeJSON(w, http.StatusOK, resp)
}

func (a *API) sessionOrb(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	if _, ok := a.info(id); !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return
	}
	entries, _ := a.sup.Entries(id)
	if mode, _ := sessionMode(entries); mode != "project" {
		writeJSON(w, http.StatusOK, map[string]any{"orb": nil})
		return
	}
	st := a.orbState(id)
	if st.Session == "" {
		writeJSON(w, http.StatusOK, map[string]any{"orb": nil})
		return
	}
	// The container may have been stopped behind the child's back (the
	// engine restarted): ask the runtime when state.json claims running.
	if st.Status == orb.StatusRunning {
		ctx, cancel := context.WithTimeout(r.Context(), runtimeTimeout)
		defer cancel()
		if cs, err := a.sup.Runtime().Inspect(ctx, container.OrbName(id)); err == nil && cs != container.StateRunning {
			st.Status, st.Up = orb.StatusStopped, false
		}
	}
	writeJSON(w, http.StatusOK, map[string]any{"orb": st})
}

// stopOrb stops the container and marks state.json stopped; the child's
// next exec restarts the container and writes running again.
func (a *API) stopOrb(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	if _, ok := a.info(id); !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 30*time.Second)
	defer cancel()
	if err := a.sup.stopOrb(ctx, id); err != nil {
		writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: stop orb %q: %w", id, err))
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

// createProjectSession starts a session inside a label's orb. cwd is
// ignored: the child works in its own worktree.
func (a *API) createProjectSession(w http.ResponseWriter, prompt, label string) {
	p, ok := a.sup.Project(label)
	if !ok {
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: no project %q", label))
		return
	}
	if p.Slug == "" {
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: project %q has no orb definition; add one first", p.Name))
		return
	}
	id, err := a.sup.Create(CreateOptions{Cwd: a.sup.Home(), Prompt: prompt, Mode: "project", Slug: p.Slug})
	if err != nil {
		writeErr(w, statusFor(err), fmt.Errorf("serve: api: create session: %w", err))
		return
	}
	if err := a.sup.AssignProject(id, p.ID); err != nil {
		writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: create session: assign %s: %w", p.Name, err))
		return
	}
	a.writeRow(w, http.StatusCreated, id)
}
