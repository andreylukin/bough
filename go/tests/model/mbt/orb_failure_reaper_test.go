//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/orb_failure_reaper.fizz against serve: a project session's orb
// between its failures, the idle reaper, background jobs and the thread
// page's "Why?" view with its Rebuild and Retry.
//
// serve runs in process on container.Fake, as in orb_lifecycle_test.go
// and for the same reason: a `bough serve` process opens the host's
// container runtime, which this suite may never touch. The session's
// child is the same stub process serve spawns there (started here by a
// real POST .../prompt), and the adapter plays what the real child does
// inside it: the state.json writes of orb.Prepare/Start/resumeLocked/
// ensureRunningLocked/Stop, resume.sh run through the runtime's exec
// seam (the fake runs it on the host, so a bad script really exits 1 and
// a stopped container really refuses it), and a background job's typed
// history entries as tools' recordJob writes them. There is no model
// turn in this flow, so no llm row: the stub never runs one.
//
// Every field is read off the ground truth — state.json, the fake
// runtime, the supervisor's lease, the session's history, the project's
// resume.sh — except what lives only in the child (whether its handle
// settled well, whether it is inside resume.sh, where it really failed)
// or on the page (the Why view, a Retry clicked), which the adapter
// tracks. What serve SHOWS of that state (the row's status and up, the
// failure log endpoint's phase and log, the running jobs, the
// transcript's "stopped with the orb" note) is checked against the
// spec's status/orb_up/failed_at/log_for at every step.

// ofrFields is the Orb role's state.
type ofrFields struct {
	file, phase, ctr, job, jobend, stopper, fail                  string
	owner, opened, resuming, idle, blamed, scriptBad, view, retry bool
}

func (o ofrFields) state() map[string]any {
	return map[string]any{
		"file": o.file, "phase": o.phase, "ctr": o.ctr, "owner": o.owner, "opened": o.opened,
		"resuming": o.resuming, "idle": o.idle, "job": o.job, "jobend": o.jobend, "stopper": o.stopper,
		"fail": o.fail, "blamed": o.blamed, "script_bad": o.scriptBad, "view": o.view, "retry": o.retry,
	}
}

// The spec's derived views, line for line.

func (o ofrFields) inFlight() bool { return o.file == "starting" || o.resuming }

func (o ofrFields) status() string {
	if (o.file == "running" || o.file == "starting") && !o.owner {
		return "stopped"
	}
	return o.file
}

func (o ofrFields) orbUp() bool {
	if o.status() == "running" {
		return true
	}
	if o.file == "running" || o.file == "starting" || o.file == "failed" {
		return o.ctr == "running"
	}
	return false
}

func (o ofrFields) failedAt() string {
	switch {
	case o.file != "failed":
		return ""
	case o.phase == "build":
		return "build"
	case o.phase == "resume":
		return "setup"
	}
	return "start"
}

func (o ofrFields) logFor() string {
	if o.failedAt() == "build" {
		return "build.log"
	}
	return "resume.log"
}

func (o ofrFields) shown() bool { return o.view && o.status() == "failed" }

const (
	ofrBadResume  = "#!/bin/sh\necho 'npm ci: cannot find package.json' >&2\nexit 1\n"
	ofrGoodResume = "#!/bin/sh\necho ready\n"
)

type ofrAdapter struct {
	t    *testing.T
	home string
	hist string
	pids string
	rt   *container.Fake
	sup  *serve.Supervisor
	api  *serve.API
	srv  *httptest.Server
	slug string // the one project every walk's session belongs to

	n     int
	id    string
	wrote time.Time // UpdatedAt of the child's last state.json write
	// idleAt is the last activity when GoIdle let time pass; the orb is
	// idle while nothing has been written since.
	idleAt time.Time

	// The child's own state, which serve cannot see.
	opened, resuming bool
	fail             string
	blamed           bool
	jobs             int       // the child's job counter: ids restart with the child
	jobAt            time.Time // when the running job started; zero when none
	// The page's.
	view, retry bool
	// Who made serve's last stop, and who stopped the last job: serve's
	// record of either says only "stopped".
	serveStopper, killedBy string

	trace []tracecheck.Step
	walks []ofrWalk

	// retryDoesNotStop is the deliberate wiring bug the wrong-adapter
	// test injects: a Retry button that sends nothing.
	retryDoesNotStop bool
}

// ofrWalk is one walk as it happened, with the session whose history
// the job check reads.
type ofrWalk struct {
	id    string
	steps []tracecheck.Step
}

var errOfrDisabled = errors.New("the adapter's view says the action is not enabled")

func newOfrAdapter(t *testing.T) *ofrAdapter {
	root, err := os.MkdirTemp("", "bofr-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(root) })
	root, _ = filepath.EvalSymlinks(root)
	a := &ofrAdapter{t: t, home: filepath.Join(root, "home"), pids: filepath.Join(root, "pids"), rt: container.NewFake()}
	a.hist = filepath.Join(a.home, ".bough", "history")
	for _, d := range []string{a.hist, a.pids} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	exe := filepath.Join(root, "child.sh")
	if err := os.WriteFile(exe, []byte(orbChild), 0o755); err != nil {
		t.Fatal(err)
	}
	// The session's image, and the base a Rebuild commits the project's on.
	a.rt.AddImage("img")
	a.rt.AddImage(projectdef.BaseTag())
	a.sup, err = serve.NewSupervisor(serve.Options{
		Exe:      exe,
		HistDir:  a.hist,
		MetaPath: filepath.Join(a.home, ".bough", "serve", "meta.json"),
		Env:      []string{"HOME=" + a.home, "PATH=" + os.Getenv("PATH"), "MODEL_PIDS=" + a.pids},
		Runtime:  a.rt,
	})
	if err != nil {
		t.Fatal(err)
	}
	a.api = serve.NewAPI(a.sup)
	a.srv = httptest.NewServer(a.api)
	t.Cleanup(func() {
		a.srv.Close()
		a.sup.Close()
	})
	var created struct {
		Project struct {
			Slug string `json:"slug"`
		} `json:"project"`
	}
	if err := a.call(http.MethodPost, "/api/projects", `{"name":"reaper"}`, http.StatusOK, &created); err != nil {
		t.Fatal(err)
	}
	a.slug = created.Project.Slug
	return a
}

func (a *ofrAdapter) call(method, path, body string, code int, out any) error {
	ctx, cancel := actionCtx()
	defer cancel()
	var rdr io.Reader
	if body != "" {
		rdr = strings.NewReader(body)
	}
	req, err := http.NewRequestWithContext(ctx, method, a.srv.URL+path, rdr)
	if err != nil {
		return err
	}
	resp, err := a.srv.Client().Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	if resp.StatusCode != code {
		return fmt.Errorf("%s %s = %d (%s), want %d", method, path, resp.StatusCode, strings.TrimSpace(string(raw)), code)
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(raw, out)
}

func (a *ofrAdapter) putResume(text string) error {
	b, _ := json.Marshal(map[string]string{"text": text})
	return a.call(http.MethodPut, "/api/projects/"+a.slug+"/orb/files/"+projectdef.FileResume, string(b), http.StatusOK, nil)
}

func (a *ofrAdapter) resumePath() string {
	return filepath.Join(projectdef.Root(a.home), a.slug, projectdef.FileResume)
}

// Init starts each walk on a fresh project session with no orb, and the
// project's resume.sh broken again (the spec starts with script_bad).
func (a *ofrAdapter) Init() error {
	a.n++
	a.id = fmt.Sprintf("ofr%04d", a.n)
	a.wrote, a.idleAt = time.Time{}, time.Time{}
	a.opened, a.resuming, a.fail, a.blamed, a.jobs, a.jobAt = false, false, "", false, 0, time.Time{}
	a.view, a.retry, a.serveStopper, a.killedBy = false, false, "", ""
	if err := a.putResume(ofrBadResume); err != nil {
		return err
	}
	if err := a.appendHistory("meta", map[string]any{"cwd": a.home, "mode": "project", "project": a.slug}); err != nil {
		return err
	}
	a.trace = nil
	return nil
}

// Cleanup ends the walk's child (serve's Kill also stops a running
// container) and keeps the walk for the trace checks.
func (a *ofrAdapter) Cleanup() error {
	a.walks = append(a.walks, ofrWalk{id: a.id, steps: a.trace})
	a.trace = nil
	return a.sup.Kill(a.id)
}

// sessionView is GET /api/sessions/{id}: the row and the transcript.
type sessionView struct {
	Session serve.Row    `json:"session"`
	Entries []serve.Line `json:"entries"`
}

// observe reads the role's fields off the ground truth.
func (a *ofrAdapter) observe() (ofrFields, error) {
	o := ofrFields{opened: a.opened, resuming: a.resuming, fail: a.fail, blamed: a.blamed, view: a.view, retry: a.retry}
	st, err := orb.ReadState(a.home, a.id)
	if err != nil {
		return o, err
	}
	o.file, o.phase = "none", ""
	if st.Session != "" {
		o.file, o.phase = string(st.Status), orbPhase(st.Phase)
		// The spec folds the image build into the start: orb.Start
		// writes "building" for exactly its build step.
		if st.Status == orb.StatusBuilding {
			o.file = "starting"
		}
	}
	cs, err := a.rt.Inspect(context.Background(), container.OrbName(a.id))
	if err != nil {
		return o, err
	}
	o.ctr = string(cs)
	o.owner = a.sup.Live(a.id)
	o.idle = st.Session != "" && !a.idleAt.IsZero() && a.lastActivity(st).Equal(a.idleAt)
	// serve writes state.json only to mark it stopped: a stopped file the
	// child did not write is serve's stop.
	switch {
	case st.Status == orb.StatusStopped && st.UpdatedAt.Equal(a.wrote):
		o.stopper = "child"
	case st.Status == orb.StatusStopped:
		o.stopper = a.serveStopper
	}
	b, err := os.ReadFile(a.resumePath())
	if err != nil {
		return o, err
	}
	o.scriptBad = string(b) != ofrGoodResume
	entries, err := history.Read(filepath.Join(a.hist, a.id+".jsonl"))
	if err != nil {
		return o, err
	}
	o.job, o.jobend = "none", ""
	open := map[any]bool{}
	for _, e := range entries {
		if e.Kind != "job" {
			continue
		}
		switch e.Data["event"] {
		case "started":
			open[e.Data["id"]] = true
			o.jobend = "" // the last job has not ended
		case "finished":
			delete(open, e.Data["id"])
			o.jobend = "exit"
			if e.Data["stopped"] == true {
				o.jobend = a.killedBy
			}
		}
	}
	if len(open) > 0 {
		o.job = "running"
	}
	return o, nil
}

// lastActivity is what the reaper measures quiet from: the later of
// state.json's write and the session's history.
func (a *ofrAdapter) lastActivity(st orb.State) time.Time {
	last := st.UpdatedAt
	if fi, err := os.Stat(filepath.Join(a.hist, a.id+".jsonl")); err == nil && fi.ModTime().After(last) {
		last = fi.ModTime()
	}
	return last
}

// GetState is the role's state, after checking that every view serve
// gives of it is the one the spec derives.
func (a *ofrAdapter) GetState() (map[string]any, error) {
	o, err := a.observe()
	if err != nil {
		return nil, err
	}
	if err := a.checkViews(o); err != nil {
		return nil, err
	}
	return o.state(), nil
}

// checkViews compares what serve shows — the row's orb status and up,
// its running jobs, the failure log endpoint the Why view reads, and the
// transcript's note for a job a stop killed — with the spec's views.
func (a *ofrAdapter) checkViews(o ofrFields) error {
	// The spec takes the runtime's snapshot as fresh (orb_up); serve's
	// runningTTL is orb_lifecycle's Refresh, not this flow's.
	a.api.ForgetRunning()
	var sv sessionView
	if err := a.call(http.MethodGet, "/api/sessions/"+a.id, "", http.StatusOK, &sv); err != nil {
		return err
	}
	rStatus, rUp := "none", false
	if ro := sv.Session.Orb; ro != nil && ro.Status != "" {
		rStatus, rUp = string(ro.Status), ro.Up
		if rStatus == string(orb.StatusBuilding) {
			rStatus = "starting"
		}
	}
	var fl struct {
		Status string `json:"status"`
		Phase  string `json:"phase"`
		Log    string `json:"log"`
	}
	if err := a.call(http.MethodGet, "/api/sessions/"+a.id+"/orb/log", "", http.StatusOK, &fl); err != nil {
		return err
	}
	stoppedNotes := 0
	for _, l := range sv.Entries {
		if l.Kind == "job" && strings.Contains(l.Text, "[stopped with the orb]") {
			stoppedNotes++
		}
	}
	var bad []string
	want := func(what string, got, exp any) {
		if got != exp {
			bad = append(bad, fmt.Sprintf("%s = %v, the model says %v", what, got, exp))
		}
	}
	want("row status", rStatus, o.status())
	want("row up", rUp, o.orbUp())
	want("row running jobs", len(sv.Session.Jobs), map[bool]int{true: 1}[o.job == "running"])
	want("failure view phase", fl.Phase, o.failedAt())
	want("failure view log", fl.Log, o.logFor())
	if o.jobend == "user" || o.jobend == "reaper" {
		want("the last job's transcript note says stopped with the orb", stoppedNotes > 0, true)
	}
	if len(bad) > 0 {
		return fmt.Errorf("serve's view of %s disagrees with the model in state %v: %s", a.id, o.state(), strings.Join(bad, "; "))
	}
	return nil
}

// step opens an action: it reads the state and requires the spec's
// `require` of it.
func (a *ofrAdapter) step(require func(ofrFields) bool) (ofrFields, error) {
	o, err := a.observe()
	if err != nil {
		return o, err
	}
	if !require(o) {
		return o, errOfrDisabled
	}
	return o, nil
}

// --- the child ---

func (a *ofrAdapter) ownerPID() (int, error) {
	b, err := os.ReadFile(filepath.Join(a.pids, a.id))
	if err != nil {
		return 0, err
	}
	var pid int
	_, err = fmt.Sscan(string(b), &pid)
	return pid, err
}

// write is one state.json write by the child: edit applies to what is on
// disk now (serve may have marked it stopped; the child's own copy
// differs from it only in that status, which every edit here sets), and
// the write moves updatedAt, like orb's writeState.
func (a *ofrAdapter) write(fresh bool, edit func(*orb.State)) error {
	pid, err := a.ownerPID()
	if err != nil {
		return err
	}
	st, err := orb.ReadState(a.home, a.id)
	if err != nil {
		return err
	}
	if fresh {
		st = orb.State{Session: a.id, Project: a.slug, Image: "img", Container: container.OrbName(a.id)}
	}
	edit(&st)
	st.PID = pid
	st.UpdatedAt = time.Now().UTC()
	timePhases(&st, st.UpdatedAt)
	if !st.UpdatedAt.After(a.wrote) {
		st.UpdatedAt = a.wrote.Add(time.Microsecond)
	}
	b, err := json.Marshal(st)
	if err != nil {
		return err
	}
	if err := writeFileAtomic(filepath.Join(orb.Dir(a.home, a.id), "state.json"), b); err != nil {
		return err
	}
	a.wrote = st.UpdatedAt
	return nil
}

// starting is every child write that begins an attempt: the last
// attempt's failure is no longer the truth (child_wrote).
func (a *ofrAdapter) starting() { a.fail, a.blamed = "", false }

// Prompt is a message to a session with no live child: serve starts one
// (the stub) and its orb row runs a fresh start, Prepare's first write.
func (a *ofrAdapter) Prompt() error {
	if _, err := a.step(func(o ofrFields) bool { return !o.owner }); err != nil {
		return err
	}
	pidfile := filepath.Join(a.pids, a.id)
	os.Remove(pidfile)
	if err := a.call(http.MethodPost, "/api/sessions/"+a.id+"/prompt", `{"text":"go on"}`, http.StatusOK, nil); err != nil {
		return err
	}
	if err := waitUntil("the session child's pid", func() bool { _, err := os.Stat(pidfile); return err == nil }); err != nil {
		return err
	}
	a.opened, a.jobs = false, 0
	a.starting()
	if err := a.write(true, func(s *orb.State) { s.Status, s.Phase = orb.StatusStarting, orb.PhaseSync }); err != nil {
		return err
	}
	// serve counts the child as the owner once its start settled.
	return waitUntil("serve to see the child own the orb", func() bool {
		var sv sessionView
		err := a.call(http.MethodGet, "/api/sessions/"+a.id, "", http.StatusOK, &sv)
		return err == nil && sv.Session.Orb != nil && sv.Session.Orb.Status == orb.StatusStarting
	})
}

// Activity is a turn writing history.
func (a *ofrAdapter) Activity() error {
	if _, err := a.step(func(o ofrFields) bool { return o.owner && o.idle }); err != nil {
		return err
	}
	return a.appendHistory("assistant", map[string]any{"text": "working"})
}

// Command is the agent's next command with the container down:
// ensureRunningLocked starts a new attempt at the container step.
func (a *ofrAdapter) Command() error {
	if _, err := a.step(func(o ofrFields) bool { return o.owner && o.opened && !o.inFlight() && o.ctr != "running" }); err != nil {
		return err
	}
	a.starting()
	return a.write(false, func(s *orb.State) {
		s.Phases = nil
		s.Status, s.Phase = orb.StatusStarting, orb.PhaseContainer
	})
}

// JobStarts and JobExits write what tools' recordJob writes: a job's
// start and its end, nothing of its output.
func (a *ofrAdapter) JobStarts() error {
	if _, err := a.step(func(o ofrFields) bool {
		return o.owner && o.opened && !o.inFlight() && o.ctr == "running" && o.job == "none"
	}); err != nil {
		return err
	}
	a.jobs++
	a.jobAt = time.Now()
	return a.appendHistory("job", map[string]any{"id": a.jobs, "event": "started", "cmd": "npm run dev"})
}

func (a *ofrAdapter) JobExits() error {
	if _, err := a.step(func(o ofrFields) bool { return o.job == "running" }); err != nil {
		return err
	}
	return a.jobEnds(0, false)
}

func (a *ofrAdapter) jobEnds(exit int, stopped bool) error {
	data := map[string]any{"id": a.jobs, "event": "finished", "cmd": "npm run dev", "exit": exit}
	if stopped {
		data["stopped"] = true
	}
	a.jobAt = time.Time{}
	return a.appendHistory("job", data)
}

// OwnerExits is the child exiting cleanly: its jobs die "killed when
// bough quit" (recorded with their exit code, never as stopped), and an
// open orb is stopped by the child itself, Orb.Stop writing stopped
// before it stops the container.
func (a *ofrAdapter) OwnerExits() error {
	o, err := a.step(func(o ofrFields) bool { return o.owner && !o.inFlight() })
	if err != nil {
		return err
	}
	if o.job == "running" {
		if err := a.jobEnds(-1, false); err != nil {
			return err
		}
	}
	if a.opened {
		if err := a.write(false, func(s *orb.State) { s.Status = orb.StatusStopped }); err != nil {
			return err
		}
		if err := a.rt.Stop(context.Background(), container.OrbName(a.id)); err != nil {
			return err
		}
		a.opened = false
	}
	pid, err := a.ownerPID()
	if err != nil {
		return err
	}
	if err := syscall.Kill(pid, syscall.SIGTERM); err != nil {
		return err
	}
	return waitUntil("serve's child to be reaped", func() bool { return !a.sup.Live(a.id) })
}

// EditResumeSh is the project page's editor saving a fixed resume.sh.
func (a *ofrAdapter) EditResumeSh() error {
	if _, err := a.step(func(o ofrFields) bool { return o.scriptBad }); err != nil {
		return err
	}
	return a.putResume(ofrGoodResume)
}

// Prepared is Prepare done and Start's image step begun.
func (a *ofrAdapter) Prepared() error {
	if _, err := a.step(func(o ofrFields) bool { return o.owner && o.file == "starting" && o.phase == "prepare" }); err != nil {
		return err
	}
	return a.write(false, func(s *orb.State) { s.Status, s.Phase = orb.StatusBuilding, orb.PhaseBuild })
}

func (a *ofrAdapter) Built() error {
	if _, err := a.step(func(o ofrFields) bool { return o.owner && o.file == "starting" && o.phase == "build" }); err != nil {
		return err
	}
	return a.write(false, func(s *orb.State) { s.Status, s.Phase = orb.StatusStarting, orb.PhaseContainer })
}

// StartFails is Orb.fail (or ensureRunningLocked's failed Start) at the
// step in progress.
func (a *ofrAdapter) StartFails() error {
	o, err := a.step(func(o ofrFields) bool {
		return o.owner && o.file == "starting" && slices.Contains([]string{"prepare", "build", "container"}, o.phase)
	})
	if err != nil {
		return err
	}
	a.fail = o.phase
	return a.write(false, func(s *orb.State) { s.Status, s.Error = orb.StatusFailed, "model: "+o.phase+" failed" })
}

// ContainerUp starts the container and enters resume.sh.
func (a *ofrAdapter) ContainerUp() error {
	if _, err := a.step(func(o ofrFields) bool { return o.owner && o.file == "starting" && o.phase == "container" }); err != nil {
		return err
	}
	if err := a.rt.Start(context.Background(), container.RunSpec{Name: container.OrbName(a.id), Image: "img"}); err != nil {
		return err
	}
	a.resuming, a.retry = true, false
	a.starting()
	return a.write(false, func(s *orb.State) { s.Phase = orb.PhaseResume })
}

// runResume runs the project's resume.sh through the runtime's exec
// seam, as Orb.runResume does: a bad script exits 1, and a container
// stopped under it refuses it.
func (a *ofrAdapter) runResume() error {
	ctx, cancel := actionCtx()
	defer cancel()
	return a.rt.Command(ctx, container.OrbName(a.id), container.ExecOptions{}, "sh", a.resumePath()).Run()
}

func (a *ofrAdapter) ResumeOk() error {
	if _, err := a.step(func(o ofrFields) bool { return o.resuming && !o.scriptBad && o.ctr == "running" }); err != nil {
		return err
	}
	if err := a.runResume(); err != nil {
		return fmt.Errorf("resume.sh failed where the model has it pass: %w", err)
	}
	a.resuming, a.opened = false, true
	return a.write(false, func(s *orb.State) { s.Status, s.Phase, s.Error = orb.StatusRunning, orb.PhaseReady, "" })
}

// ResumeFails writes the child's failed-at-resume.sh over whatever serve
// wrote meanwhile, as resumeLocked does.
func (a *ofrAdapter) ResumeFails() error {
	o, err := a.step(func(o ofrFields) bool { return o.resuming && (o.scriptBad || o.ctr != "running") })
	if err != nil {
		return err
	}
	rerr := a.runResume()
	if rerr == nil {
		return errors.New("resume.sh passed where the model has it fail")
	}
	a.resuming, a.opened, a.fail, a.blamed = false, true, "resume", o.ctr != "running"
	return a.write(false, func(s *orb.State) {
		s.Status, s.Phase, s.Error = orb.StatusFailed, orb.PhaseResume, "resume.sh: "+rerr.Error()
	})
}

// --- serve ---

// GoIdle lets the idle limit pass with nothing written: the adapter's
// clock, which Reap hands the reaper.
func (a *ofrAdapter) GoIdle() error {
	if _, err := a.step(func(o ofrFields) bool { return !o.idle && (o.file == "running" || o.file == "failed") }); err != nil {
		return err
	}
	st, err := orb.ReadState(a.home, a.id)
	if err != nil {
		return err
	}
	a.idleAt = a.lastActivity(st)
	return nil
}

// Reap is a reaper tick once the idle limit has passed since GoIdle.
func (a *ofrAdapter) Reap() error {
	if _, err := a.step(func(o ofrFields) bool { return o.idle && (o.file == "running" || o.file == "failed") }); err != nil {
		return err
	}
	a.serveStopper = "reaper"
	ctx, cancel := actionCtx()
	defer cancel()
	if got := a.api.ReapIdleOrbs(ctx, orbIdle, a.idleAt.Add(orbIdle)); !slices.Contains(got, a.id) {
		return fmt.Errorf("the reaper stopped %v, not %s", got, a.id)
	}
	return a.jobDies("reaper")
}

// StopOrb is the header's Stop orb (after its confirm when jobs run).
func (a *ofrAdapter) StopOrb() error {
	if _, err := a.step(ofrFields.orbUp); err != nil {
		return err
	}
	return a.stop("user")
}

func (a *ofrAdapter) stop(who string) error {
	a.serveStopper = who
	if err := a.call(http.MethodPost, "/api/sessions/"+a.id+"/orb/stop", "", http.StatusOK, nil); err != nil {
		return err
	}
	return a.jobDies(who)
}

// jobDies is a running job's exec ending because serve stopped its
// container (the fake stops no process, so the adapter ends it). The
// child's job goroutine then asks Orb.StoppedSince, which answers true
// whenever the container is not running, so the job is recorded
// "stopped with the orb", whoever stopped it, and wakes nobody.
func (a *ofrAdapter) jobDies(who string) error {
	if a.jobAt.IsZero() {
		return nil
	}
	cs, err := a.rt.Inspect(context.Background(), container.OrbName(a.id))
	if err != nil || cs == container.StateRunning {
		return err
	}
	a.killedBy = who
	return a.jobEnds(137, true)
}

// --- the page's failure view ---

// failureView is what the Why view loads: GET .../orb/log.
func (a *ofrAdapter) failureView() (string, error) {
	var fl struct {
		Phase string `json:"phase"`
	}
	err := a.call(http.MethodGet, "/api/sessions/"+a.id+"/orb/log", "", http.StatusOK, &fl)
	return fl.Phase, err
}

func (a *ofrAdapter) OpenWhy() error {
	if _, err := a.step(func(o ofrFields) bool { return o.status() == "failed" && !o.view }); err != nil {
		return err
	}
	a.view = true
	return nil
}

func (a *ofrAdapter) CloseWhy() error {
	if _, err := a.step(func(o ofrFields) bool { return o.view }); err != nil {
		return err
	}
	a.view = false
	return nil
}

// ClickRebuild is the view's Rebuild image: a project build (a 409
// means one runs already, which the page accepts), waited out so the
// next step does not race serve's build goroutine.
func (a *ofrAdapter) ClickRebuild() error {
	o, err := a.step(func(o ofrFields) bool { return o.shown() })
	if err != nil {
		return err
	}
	if p, err := a.failureView(); err != nil || p != "build" {
		if err == nil {
			err = fmt.Errorf("the view offers Rebuild only for a build failure; serve says %q (model: %s)", p, o.failedAt())
		}
		return err
	}
	if err := a.call(http.MethodPost, "/api/projects/"+a.slug+"/orb/build", "{}", http.StatusAccepted, nil); err != nil {
		return err
	}
	return waitUntil("the rebuild to end", func() bool {
		var bl struct {
			State string `json:"state"`
		}
		err := a.call(http.MethodGet, "/api/projects/"+a.slug+"/orb/build/log?offset=0", "", http.StatusOK, &bl)
		return err == nil && bl.State != "building"
	})
}

// ClickRetry is the view's Retry for a setup failure: Stop orb.
func (a *ofrAdapter) ClickRetry() error {
	o, err := a.step(func(o ofrFields) bool { return o.shown() })
	if err != nil {
		return err
	}
	if p, err := a.failureView(); err != nil || p != "setup" {
		if err == nil {
			err = fmt.Errorf("the view offers Retry only for a setup failure; serve says %q (model: %s)", p, o.failedAt())
		}
		return err
	}
	a.retry = true
	if a.retryDoesNotStop {
		return nil
	}
	return a.stop("user")
}

func (a *ofrAdapter) appendHistory(kind string, data map[string]any) error {
	path := filepath.Join(a.hist, a.id+".jsonl")
	entries, _ := history.Read(path)
	b, _ := json.Marshal(history.Entry{Seq: int64(len(entries) + 1), At: time.Now(), Kind: kind, Data: data})
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return err
	}
	defer f.Close()
	_, err = f.Write(append(b, '\n'))
	return err
}

var ofrActions = map[string]func(*ofrAdapter) error{
	"Prompt":       (*ofrAdapter).Prompt,
	"Activity":     (*ofrAdapter).Activity,
	"Command":      (*ofrAdapter).Command,
	"JobStarts":    (*ofrAdapter).JobStarts,
	"JobExits":     (*ofrAdapter).JobExits,
	"OwnerExits":   (*ofrAdapter).OwnerExits,
	"EditResumeSh": (*ofrAdapter).EditResumeSh,
	"Prepared":     (*ofrAdapter).Prepared,
	"Built":        (*ofrAdapter).Built,
	"StartFails":   (*ofrAdapter).StartFails,
	"ContainerUp":  (*ofrAdapter).ContainerUp,
	"ResumeOk":     (*ofrAdapter).ResumeOk,
	"ResumeFails":  (*ofrAdapter).ResumeFails,
	"GoIdle":       (*ofrAdapter).GoIdle,
	"Reap":         (*ofrAdapter).Reap,
	"StopOrb":      (*ofrAdapter).StopOrb,
	"OpenWhy":      (*ofrAdapter).OpenWhy,
	"CloseWhy":     (*ofrAdapter).CloseWhy,
	"ClickRebuild": (*ofrAdapter).ClickRebuild,
	"ClickRetry":   (*ofrAdapter).ClickRetry,
}

// ofrDiff compares the spec's role fields with the adapter's, through
// JSON so numbers and bools compare as the spec has them.
func ofrDiff(want map[string]any, got map[string]any) string {
	var diffs []string
	for k, w := range want {
		f, ok := strings.CutPrefix(k, "Orb#0.")
		if !ok {
			continue // "orb": the role reference itself
		}
		wj, _ := json.Marshal(w)
		gj, _ := json.Marshal(got[f])
		if string(wj) != string(gj) {
			diffs = append(diffs, fmt.Sprintf("%s: spec %s, server %s", f, wj, gj))
		}
	}
	slices.Sort(diffs)
	return strings.Join(diffs, "; ")
}

func qualifyOrb(st map[string]any) map[string]any {
	q := make(map[string]any, len(st))
	for k, v := range st {
		q["Orb#0."+k] = v
	}
	return q
}

// walkOrbFailureReaperPaths drives the adapter down every walk the graph
// gives and returns the first step whose state is not the spec's.
func walkOrbFailureReaperPaths(t *testing.T, a *ofrAdapter, cover tracecheck.Cover) error {
	t.Helper()
	b, err := pathsJSONCover("orb_failure_reaper", cover)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		t.Fatal(err)
	}
	if len(doc.Paths) == 0 {
		t.Fatal("no walks over testdata/orb_failure_reaper")
	}
	for pi, p := range doc.Paths {
		if err := a.Init(); err != nil {
			return fmt.Errorf("walk %d: Init: %w", pi, err)
		}
		for si, s := range p.Trace {
			name := strings.TrimPrefix(s.Action, "Orb#0.")
			if si > 0 {
				act, ok := ofrActions[name]
				if !ok {
					t.Fatalf("walk %d step %d: no adapter action for %s", pi, si, s.Action)
				}
				if err := act(a); err != nil {
					a.Cleanup()
					return fmt.Errorf("walk %d step %d (%s): %w", pi, si, name, err)
				}
			}
			got, err := a.GetState()
			if err != nil {
				a.Cleanup()
				return fmt.Errorf("walk %d step %d (%s): %w", pi, si, name, err)
			}
			a.trace = append(a.trace, tracecheck.Step{Action: s.Action, State: qualifyOrb(got)})
			if diff := ofrDiff(s.State, got); diff != "" {
				a.Cleanup()
				return fmt.Errorf("walk %d step %d (%s): %s", pi, si, name, diff)
			}
		}
		if err := a.Cleanup(); err != nil {
			return fmt.Errorf("walk %d: Cleanup: %w", pi, err)
		}
	}
	return nil
}

// ofrJobStory is the jobs a walk's trace says ran and how each ended,
// in order: "started", then "finished" or "stopped" (by a stop of the
// orb, the only end the transcript records as such).
func ofrJobStory(steps []tracecheck.Step) []string {
	var out []string
	prevJob := "none"
	for _, s := range steps {
		job, _ := s.State["Orb#0.job"].(string)
		end, _ := s.State["Orb#0.jobend"].(string)
		switch {
		case prevJob == "none" && job == "running":
			out = append(out, "started")
		case prevJob == "running" && job == "none" && end == "exit":
			out = append(out, "finished")
		case prevJob == "running" && job == "none":
			out = append(out, "stopped")
		}
		prevJob = job
	}
	return out
}

// ofrHistoryJobs is the same story read off the session's transcript:
// tools' typed job entries, a finished one marked stopped when a stop of
// the orb killed it.
func ofrHistoryJobs(entries []history.Entry) []string {
	var out []string
	for _, e := range entries {
		if e.Kind != "job" {
			continue
		}
		switch e.Data["event"] {
		case "started":
			out = append(out, "started")
		case "finished":
			if e.Data["stopped"] == true {
				out = append(out, "stopped")
			} else {
				out = append(out, "finished")
			}
		}
	}
	return out
}

// checkOfrWalks is the trace check: every walk as the server lived it is
// a path in the graph, and every walk's session history tells the same
// job story as the walk.
func checkOfrWalks(t *testing.T, a *ofrAdapter) {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("orb_failure_reaper")), "..", "testdata", "orb_failure_reaper"))
	if err != nil {
		t.Fatal(err)
	}
	jobs := 0
	for i, w := range a.walks {
		if v := g.Check(w.steps); v != nil {
			b, _ := json.Marshal(w.steps)
			t.Fatalf("walk %d is not a path in the model: %v\ntrace: %s", i, v, b)
		}
		want := ofrJobStory(w.steps)
		got := ofrHistoryJobs(sessionHistory(t, a.home, w.id))
		if !slices.Equal(got, want) {
			t.Fatalf("walk %d: session %s's history records jobs %v, the walk %v", i, w.id, got, want)
		}
		jobs += len(want)
	}
	t.Logf("%d walks replayed on the graph; %d job events matched their histories", len(a.walks), jobs)
}

// Every settled state of the spec against serve (every transition under
// MODEL_COVER=transitions), then the trace check of what was walked.
func TestOrbFailureReaperPaths(t *testing.T) {
	t.Parallel()
	a := newOfrAdapter(t)
	if err := walkOrbFailureReaperPaths(t, a, envCover()); err != nil {
		t.Fatal(err)
	}
	checkOfrWalks(t, a)
}

// The walk proves nothing unless a wrong wiring fails it: here the
// view's Retry sends nothing, which shows on the ClickRetry transition
// alone (the orb stays failed where the spec has it stopped).
func TestOrbFailureReaperPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newOfrAdapter(t)
	a.retryDoesNotStop = true
	err := walkOrbFailureReaperPaths(t, a, tracecheck.CoverTransitions)
	if err == nil {
		t.Fatal("walks whose Retry does not stop the orb passed; the walk is not checking state")
	}
	if !strings.Contains(err.Error(), "ClickRetry") {
		t.Fatalf("caught, but not at ClickRetry: %v", err)
	}
	t.Logf("caught: %v", err)
}
