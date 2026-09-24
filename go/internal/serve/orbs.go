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
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"sort"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/google/uuid"
)

// OrbSummary is a definition's image at a glance.
type OrbSummary struct {
	Slug  string `json:"slug"`
	Image string `json:"image"`           // tag for the CURRENT hash
	Built bool   `json:"built"`           // that tag exists in the runtime
	Build string `json:"build,omitempty"` // orb.Build.State
	Error string `json:"error,omitempty"` // definition parse error
	// Repos names the repos the definition declares (worktree names).
	Repos []string `json:"repos,omitempty"`
}

// OrbState is a session's state.json as the wire sees it. Up says the
// container is running whatever the status: a failed setup (resume.sh
// exited non-zero) leaves it up, and Stop must still be offered.
type OrbState struct {
	orb.State
	Up bool `json:"up,omitempty"`
	// Title is the session's, so the orb list names archived sessions too.
	Title string `json:"title,omitempty"`
	// Portals shadows State.Portals (a shallower field wins in JSON) to
	// add Live. state.json records what a session opened, but the
	// listener lives in that session's process: after it exits the entry
	// is still there and the URL is dead. The UI has to be able to say
	// so rather than hand over a link that hangs.
	Portals []PortalView `json:"portals,omitempty"`
}

// PortalView is one portal as the control room sees it.
type PortalView struct {
	orb.PortalState
	URL  string `json:"url"`
	Live bool   `json:"live"`
}

// portalViews answers, for each recorded portal, whether anything still
// accepts on its host port.
func portalViews(ps []orb.PortalState) []PortalView {
	out := make([]PortalView, 0, len(ps))
	for _, p := range ps {
		v := PortalView{PortalState: p, URL: p.URL()}
		if c, err := net.DialTimeout("tcp", "127.0.0.1:"+strconv.Itoa(p.Host), 150*time.Millisecond); err == nil {
			c.Close()
			v.Live = true
		}
		out = append(out, v)
	}
	return out
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
	// Preflight is what a session start needs, checked now. A broken
	// definition checks only the runtime; the image row says why.
	Preflight []orb.PreflightCheck `json:"preflight"`
}

// projectRow is a project plus the state of the image its sessions run
// in. Every project has one: the definition directory IS the project.
type projectRow struct {
	Project
	Orb *OrbSummary `json:"orb,omitempty"`
}

const (
	maxLogChunk    = 256 << 10
	runtimeTimeout = 5 * time.Second
	// runningTTL is how long one running-container snapshot answers for.
	runningTTL = 10 * time.Second
)

func (a *API) routeOrbs() {
	a.mux.HandleFunc("GET /api/projects/{slug}/orb", a.orbDetail)
	a.mux.HandleFunc("PUT /api/projects/{slug}/orb/files/{name}", a.putOrbFile)
	a.mux.HandleFunc("POST /api/projects/{slug}/orb/build", a.buildOrb)
	a.mux.HandleFunc("GET /api/projects/{slug}/orb/build/log", a.buildLog)
	a.mux.HandleFunc("GET /api/sessions/{id}/orb", a.sessionOrb)
	a.mux.HandleFunc("GET /api/sessions/{id}/orb/log", a.sessionOrbLog)
	a.mux.HandleFunc("GET /api/sessions/{id}/orb/build/log", a.sessionBuildLog)
	a.mux.HandleFunc("POST /api/sessions/{id}/orb/stop", a.stopOrb)
	a.mux.HandleFunc("POST /api/sessions/{id}/orb/restart", a.restartOrb)
	a.mux.HandleFunc("GET /api/sessions/{id}/orb/remove", a.removeOrbPlan)
	a.mux.HandleFunc("DELETE /api/sessions/{id}/orb", a.removeOrb)
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
	st := OrbState{State: s, Portals: portalViews(s.Portals)}
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
//
// It reads a snapshot of every running container, taken at most every
// runningTTL: the session list asks this for every orb row on every
// poll, and on a laptop with sixty orbs the per-row inspect it used to
// run took the list past twenty seconds. A stop or start this serve
// performs drops the snapshot, so its own actions show at once.
func (a *API) containerUp(session string) bool {
	rt := a.sup.Runtime()
	if rt == nil {
		return false
	}
	name := container.OrbName(session)
	// A stop serve made since the snapshot makes it stale for this
	// session. The Stop orb handler drops the snapshot itself, but Kill
	// (archive, a stopped agent) stops through the supervisor, which
	// cannot reach it, and a restart then showed the stopped container up.
	a.sup.mu.Lock()
	stoppedAt, stopped := a.sup.stoppedAt[session]
	a.sup.mu.Unlock()
	a.runningMu.Lock()
	defer a.runningMu.Unlock()
	ttl := runningTTL
	if a.runningFor > 0 {
		ttl = a.runningFor
	}
	if a.running == nil || time.Since(a.runningAt) > ttl || (stopped && !stoppedAt.Before(a.runningAt)) {
		ctx, cancel := context.WithTimeout(context.Background(), runtimeTimeout)
		names, err := rt.Running(ctx)
		cancel()
		if err != nil {
			// A runtime that cannot list (or a stub) still answers the one
			// question, the slow way, and nothing is cached from it.
			ctx, cancel := context.WithTimeout(context.Background(), runtimeTimeout)
			defer cancel()
			cs, err := rt.Inspect(ctx, name)
			return err == nil && cs == container.StateRunning
		}
		a.running = map[string]bool{}
		for _, n := range names {
			a.running[n] = true
		}
		a.runningAt = time.Now()
	}
	return a.running[name]
}

// forgetRunning drops the running-container snapshot: the next question
// asks the runtime again. Called after anything this serve starts or stops.
func (a *API) forgetRunning() {
	a.runningMu.Lock()
	a.running = nil
	a.runningMu.Unlock()
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
	for _, r := range p.Def.Repos {
		sum.Repos = append(sum.Repos, r.RepoName())
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
		sum, _ := a.orbSummary(r.Context(), p.Slug)
		out = append(out, projectRow{Project: p, Orb: &sum})
	}
	writeJSON(w, http.StatusOK, map[string]any{"projects": out})
}

// projectOr404 resolves a slug from the path, answering itself when it
// does not name a project.
func (a *API) projectOr404(w http.ResponseWriter, slug string) (Project, bool) {
	p, ok := a.sup.Project(slug)
	if ok {
		return p, true
	}
	if oldProjectID(slug) {
		writeErr(w, http.StatusBadRequest, errOldProjectID)
		return Project{}, false
	}
	writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: no project %q", slug))
	return Project{}, false
}

// errOldProjectID answers a link minted when projects were labels with
// their own ids. A UUIDv7 is lowercase hex and hyphens, which ValidSlug
// accepts, so the id shape is tested outright — otherwise these arrive
// as a bare 404 and read as "the project is gone".
var errOldProjectID = errors.New("serve: api: that link is from an older control room; projects are named by slug now")

// oldProjectID says whether a path or body value is one of those ids.
func oldProjectID(s string) bool { _, err := uuid.Parse(s); return err == nil }

var slugJunk = regexp.MustCompile(`[^a-z0-9]+`)

// slugify turns a label name into a definition directory name.
func slugify(name string) string {
	s := strings.Trim(slugJunk.ReplaceAllString(strings.ToLower(name), "-"), "-")
	if len(s) > 63 {
		s = strings.TrimRight(s[:63], "-")
	}
	return s
}

func (a *API) orbDetail(w http.ResponseWriter, r *http.Request) {
	p, ok := a.projectOr404(w, r.PathValue("slug"))
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
	def, err := projectdef.Load(home, p.Slug)
	if err != nil {
		def = projectdef.Project{Slug: p.Slug}
	}
	d.Preflight = orb.Preflight(r.Context(), home, rt, def)
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
			st.Title = a.sup.childTitle(st.Session)
			out = append(out, st)
		}
	}
	sort.SliceStable(out, func(i, j int) bool { return out[i].UpdatedAt.After(out[j].UpdatedAt) })
	return out
}

func (a *API) putOrbFile(w http.ResponseWriter, r *http.Request) {
	p, ok := a.projectOr404(w, r.PathValue("slug"))
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
	p, ok := a.projectOr404(w, r.PathValue("slug"))
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
	p, ok := a.projectOr404(w, r.PathValue("slug"))
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
	// A failed build never reached resume.sh: its log is the project's build.log.
	name, path := orb.LogFor(a.sup.Home(), st.State)
	failedAt := orb.FailedAt(st.State)
	text := ""
	if b, err := os.ReadFile(path); err == nil {
		if len(b) > maxLogChunk {
			b = b[len(b)-maxLogChunk:]
		}
		text = string(b)
	}
	writeJSON(w, http.StatusOK, map[string]any{"status": st.Status, "error": st.Error, "phase": failedAt, "image": st.Image, "log": name, "text": text})
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
	} else if state == "building" && st.Status != orb.StatusBuilding {
		// build.json is the project's and a child killed mid-build leaves
		// it "building": a session not building now waits on no build,
		// and "building" here kept its log polling a dead one.
		state = "interrupted"
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
	a.forgetRunning() // the snapshot said it was up; it is not now
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

// restartOrb asks a live session to apply its project's current
// definition to its orb ({"fresh":true} recreates the container even when
// nothing changed). Only the owning process can swap its orb, so this
// writes the request file it polls: 202, scheduled. A session nobody runs
// needs nothing — its next start applies the definition — so that is 200
// with scheduled false.
func (a *API) restartOrb(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	st, err := orb.ReadState(a.sup.Home(), id)
	if err != nil || st.Session == "" {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: restart orb %q: no orb", id))
		return
	}
	var body struct {
		Fresh bool `json:"fresh"`
	}
	if r.ContentLength != 0 {
		if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 4<<10)).Decode(&body); err != nil && !errors.Is(err, io.EOF) {
			writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: restart orb %q: %w", id, err))
			return
		}
	}
	if a.sup.childPID(id) == 0 && !a.ownerAlive(id, st) {
		writeJSON(w, http.StatusOK, map[string]any{"ok": true, "scheduled": false})
		return
	}
	if err := orb.RequestRestart(a.sup.Home(), id, orb.RestartRequest{Fresh: body.Fresh, By: "web"}); err != nil {
		writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: restart orb %q: %w", id, err))
		return
	}
	writeJSON(w, http.StatusAccepted, map[string]any{"ok": true, "scheduled": true})
}

// createProjectSession starts a thread inside a project's orb. cwd is
// ignored: the child works in its own worktree.
//
// The thread is a CHILD of the project's main thread, not a session of
// its own. That is what makes the report land: a parentless session's
// finish never reaches anybody (childEventLocked returns early without
// a SpawnedBy), so every thread a person starts from the project page
// would end in silence. Starting the first one starts the main thread.
func (a *API) createProjectSession(w http.ResponseWriter, prompt, slug string, maxPerSession, maxRunning int) {
	p, ok := a.sup.Project(slug)
	if !ok {
		if oldProjectID(slug) {
			writeErr(w, http.StatusBadRequest, errOldProjectID)
			return
		}
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: no project %q", slug))
		return
	}
	main, err := a.sup.Main(p.Slug)
	if err != nil {
		writeErr(w, statusFor(err), fmt.Errorf("serve: api: project %s: %w", p.Slug, err))
		return
	}
	// CreateChild mints its own id, files the membership and starts the
	// child in the project's orb; SpawnedBy is the whole point. Thread
	// keeps it a full agent: a person started it, not main's model.
	a.createChild(w, CreateOptions{Prompt: prompt, Slug: p.Slug, SpawnedBy: main, Thread: true}, maxPerSession, maxRunning)
}

// removeOrbPlan is what Remove orb would delete and keep, for its confirm.
func (a *API) removeOrbPlan(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	ctx, cancel := context.WithTimeout(r.Context(), 30*time.Second)
	defer cancel()
	plan, err := orb.PlanRemove(ctx, a.sup.Runtime(), a.sup.Home(), id, r.URL.Query().Get("branches") == "1")
	if err != nil {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: remove orb %q: %w", id, err))
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"plan": plan, "live": a.sup.childPID(id) != 0})
}

// removeOrb deletes a session's container, worktrees and orb dir, and with
// ?branches=1 its merged or pushed branches. serve's own child is ended
// first so it cannot recreate the orb; another live owner is refused.
func (a *API) removeOrb(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	st, err := orb.ReadState(a.sup.Home(), id)
	if err != nil || st.Session == "" {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: remove orb %q: no orb", id))
		return
	}
	if a.sup.childPID(id) != 0 {
		if err := a.sup.Kill(id); err != nil {
			writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: remove orb %q: %w", id, err))
			return
		}
	} else if a.ownerAlive(id, st) {
		writeErr(w, http.StatusConflict, fmt.Errorf("serve: api: remove orb %q: its session runs in another bough (pid %d); quit it first", id, st.PID))
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 2*time.Minute)
	defer cancel()
	plan, err := orb.PlanRemove(ctx, a.sup.Runtime(), a.sup.Home(), id, r.URL.Query().Get("branches") == "1")
	if err != nil {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: remove orb %q: %w", id, err))
		return
	}
	if len(plan.Dirty) > 0 {
		writeErr(w, http.StatusConflict, fmt.Errorf("serve: api: remove orb %q: uncommitted changes in %s; commit or discard them first", id, strings.Join(plan.Dirty, ", ")))
		return
	}
	if err := orb.RemovePlanned(ctx, a.sup.Runtime(), a.sup.Home(), plan); err != nil {
		writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: remove orb %q: %w", id, err))
		return
	}
	a.forgetRunning()
	writeJSON(w, http.StatusOK, map[string]any{"ok": true, "plan": plan})
}
