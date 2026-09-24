//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"maps"
	"math/rand/v2"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/orb_lifecycle.fizz against serve's orb side: the session rows'
// orb status and up, the portal and build-log endpoints, Stop orb,
// Remove orb, Kill and the idle reaper.
//
// Not through servetest: a `bough serve` process always uses the host's
// container runtime (container.Default), and this suite may never touch
// a real one. So serve runs in process — the same Supervisor and API
// `bough serve` builds, behind an httptest server — with container.Fake
// as its runtime. A spawned project child would open its own runtime
// too, so the child is played by the adapter: it writes state.json and
// build.json the way orb.Prepare/Start do and starts the fake
// container, while the process that owns the orb is real — a stub
// session child the supervisor spawns (owner "serve") or a plain process
// it did not (owner "cli"). serve sees only files, pids and the runtime,
// which is all it sees of a real child.
//
// The spec's fields are read off the ground truth (state.json, the fake
// runtime, the supervisor's lease, the worktree's git status, serve's
// running snapshot); what serve SHOWS is derived from them by the spec's
// status/up/portal_live/build_log, and GetState fails when the API says
// otherwise. The fields the walk checks and the views the API shows are
// both the product.

// orbFields is the Orb role's state.
type orbFields struct {
	file, phase, ctr, owner               string
	mark, snap, idle, dirty, portal, lost bool
}

func (o orbFields) state() map[string]any {
	return map[string]any{
		"file": o.file, "phase": o.phase, "ctr": o.ctr, "owner": o.owner,
		"mark": o.mark, "snap": o.snap, "idle": o.idle, "dirty": o.dirty, "portal": o.portal, "lost": o.lost,
	}
}

// The spec's derived views, line for line.

func (o orbFields) status() string {
	if (o.file == "running" || o.file == "starting" || o.file == "building") && (o.owner == "none" || o.mark) {
		return "stopped"
	}
	return o.file
}

func (o orbFields) up() bool {
	switch o.file {
	case "failed":
		return o.snap
	case "running", "starting":
	default:
		return false
	}
	u := o.file == "running" || o.snap
	if o.owner == "none" {
		return u && o.snap
	}
	return u && !o.mark
}

func (o orbFields) portalLive() bool { return o.portal && o.owner != "none" }

func (o orbFields) buildLog() string {
	if o.status() == "building" {
		return "building"
	}
	return "done"
}

// orbPhase maps state.json's phase onto the spec's: sync and worktree
// are Prepare's, ready (or nothing) is no phase.
func orbPhase(p string) string {
	switch p {
	case orb.PhaseSync, orb.PhaseWorktree:
		return "prepare"
	case orb.PhaseBuild, orb.PhaseContainer:
		return p
	case orb.PhaseResume:
		return "resume"
	}
	return ""
}

// orbChild is the stub a serve-owned session runs: it records its pid
// for the adapter (which writes state.json as that child would) and
// lives until its stdin closes or it is signalled.
const orbChild = `#!/bin/sh
id="$BOUGH_SESSION_ID"
while [ $# -gt 0 ]; do
  if [ "$1" = "-r" ]; then id="$2"; fi
  shift
done
echo $$ > "$MODEL_PIDS/$id.tmp" && mv "$MODEL_PIDS/$id.tmp" "$MODEL_PIDS/$id"
exec cat > /dev/null
`

const (
	orbIdle  = time.Hour // the reaper's limit; the adapter owns the clock
	orbGuest = 3000
)

type orbAdapter struct {
	t    *testing.T
	home string
	hist string
	pids string
	rt   *container.Fake
	sup  *serve.Supervisor
	api  *serve.API
	srv  *httptest.Server

	n     int
	id    string
	slug  string
	wt    string       // the session's one worktree, a real git repo
	cli   *exec.Cmd    // the owner when it is another bough
	ln    net.Listener // the portal's listener, in the owner's stead
	wrote time.Time    // UpdatedAt of the child's last state.json write
	// idleAt is the last activity when GoIdle let time pass; the orb is
	// idle while nothing has been written since.
	idleAt time.Time
	lost   bool

	action   string // the step GetState records next
	trace    []tracecheck.Step
	traces   [][]tracecheck.Step
	failures []error             // GetState's, in order
	stutters [][]tracecheck.Step // walks so far ending in a step that changed nothing

	// exitKeepsContainer is the deliberate bug the wrong-adapter test
	// injects: a child that exits without stopping its container.
	exitKeepsContainer bool

	probe   *orbFields // see step
	enabled []string
}

func newOrbAdapter(t *testing.T) *orbAdapter {
	// A short root, not t.TempDir: git and the stub child are fine with
	// long paths, but keep it under the system temp dir like servetest.
	root, err := os.MkdirTemp("", "borb-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(root) })
	root, _ = filepath.EvalSymlinks(root)
	a := &orbAdapter{t: t, home: filepath.Join(root, "home"), pids: filepath.Join(root, "pids"), rt: container.NewFake()}
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
	a.rt.AddImage("img")
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
		a.endCLI(syscall.SIGKILL)
	})
	return a
}

// Init starts each walk on a fresh project session with no orb, and a
// snapshot taken now: the spec starts with snap false and ctr missing.
func (a *orbAdapter) Init() error {
	a.n++
	a.id, a.slug = fmt.Sprintf("orb%03d", a.n), fmt.Sprintf("p%03d", a.n)
	a.wt = filepath.Join(a.home, "wt", a.id)
	a.wrote, a.idleAt, a.lost = time.Time{}, time.Time{}, false
	if err := a.appendHistory("meta", map[string]any{"cwd": a.home, "mode": "project", "project": a.slug}); err != nil {
		return err
	}
	if err := git(a.wt, "init", "-q"); err != nil {
		return err
	}
	a.api.ForgetRunning()
	a.api.ContainerUp(a.id)
	a.trace = nil
	a.action = "Init"
	return nil
}

// Cleanup ends whatever owns this walk's orb, so no stub child or
// listener outlives it.
func (a *orbAdapter) Cleanup() error {
	a.closePortal()
	a.endCLI(syscall.SIGKILL)
	if err := a.sup.Kill(a.id); err != nil {
		return err
	}
	if len(a.trace) > 0 {
		a.traces = append(a.traces, a.trace)
		a.trace = nil
	}
	return nil
}

func (a *orbAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Orb", Index: 0}: a}, nil
}

// observe reads the role's fields off the ground truth.
func (a *orbAdapter) observe() (orbFields, error) {
	var o orbFields
	st, err := orb.ReadState(a.home, a.id)
	if err != nil {
		return o, err
	}
	o.file, o.phase = "none", ""
	if st.Session != "" {
		o.file, o.phase = string(st.Status), orbPhase(st.Phase)
	}
	cs, err := a.rt.Inspect(context.Background(), container.OrbName(a.id))
	if err != nil {
		return o, err
	}
	o.ctr = string(cs)
	serveOwns, cliOwns := a.sup.Live(a.id), a.cli != nil
	switch {
	case serveOwns && cliOwns:
		return o, errors.New("orb: both serve's child and another process own the orb")
	case serveOwns:
		o.owner = "serve"
	case cliOwns:
		o.owner = "cli"
	default:
		o.owner = "none"
	}
	// serve writes state.json only to mark it stopped: any write that is
	// not the child's is serve's stop since the child last wrote.
	o.mark = st.Session != "" && !st.UpdatedAt.Equal(a.wrote)
	o.snap = a.api.ContainerUp(a.id)
	o.idle = st.Session != "" && !a.idleAt.IsZero() && a.lastActivity(st).Equal(a.idleAt)
	o.portal = len(st.Portals) > 0
	o.lost = a.lost
	if _, err := os.Stat(a.wt); err == nil {
		out, err := exec.Command("git", "-C", a.wt, "status", "--porcelain").Output()
		if err != nil {
			return o, fmt.Errorf("git status %s: %w", a.wt, err)
		}
		o.dirty = strings.TrimSpace(string(out)) != ""
	}
	return o, nil
}

// lastActivity is what the reaper measures quiet from: the later of
// state.json's write and the session's history.
func (a *orbAdapter) lastActivity(st orb.State) time.Time {
	last := st.UpdatedAt
	if fi, err := os.Stat(filepath.Join(a.hist, a.id+".jsonl")); err == nil && fi.ModTime().After(last) {
		last = fi.ModTime()
	}
	return last
}

// GetState is the role's state, after checking that every view serve
// gives of it is the one the spec derives.
//
// Its errors are kept as well as returned: fizzbee-mbt-runner 0.2.0
// logs a failed GetState as the step's response and still passes the
// run, so orbRun fails on them itself.
func (a *orbAdapter) GetState() (map[string]any, error) {
	o, err := a.observe()
	if err != nil {
		a.failures = append(a.failures, err)
		return nil, err
	}
	a.record(o)
	if err := a.checkViews(o); err != nil {
		a.failures = append(a.failures, err)
		return nil, err
	}
	return o.state(), nil
}

// record adds the step just taken, and the state after it, to the
// walk's trace.
func (a *orbAdapter) record(o orbFields) {
	if a.action != "" {
		name := a.action
		if name != "Init" {
			name = "Orb#0." + name
		}
		q := map[string]any{}
		for k, v := range o.state() {
			q["Orb#0."+k] = v
		}
		step := tracecheck.Step{Action: name, State: q}
		a.action = ""
		// fizz writes no link for an action that leaves the state as it
		// was (a Remove refused as dirty), so such a step is kept aside
		// with the walk so far and checked on its own in orbRun.
		if n := len(a.trace); n > 0 && reflect.DeepEqual(a.trace[n-1].State, q) && name != "Init" {
			a.stutters = append(a.stutters, append(slices.Clone(a.trace), step))
			return
		}
		a.trace = append(a.trace, step)
	}
}

// checkViews compares the row, the orb detail and the build log with
// the spec's status, up, portal_live and build_log of o.
func (a *orbAdapter) checkViews(o orbFields) error {
	ctx, cancel := actionCtx()
	defer cancel()
	var row struct {
		Session serve.Row `json:"session"`
	}
	if err := a.get(ctx, "/api/sessions/"+a.id, &row); err != nil {
		return err
	}
	ro := row.Session.Orb
	if ro == nil {
		return fmt.Errorf("session %s row has no orb: %+v", a.id, row.Session)
	}
	var detail struct {
		Orb *serve.OrbState `json:"orb"`
	}
	if err := a.get(ctx, "/api/sessions/"+a.id+"/orb", &detail); err != nil {
		return err
	}
	dStatus, dUp := "none", false
	if detail.Orb != nil {
		dStatus, dUp = string(detail.Orb.Status), detail.Orb.Up
	}
	var bl struct {
		State string `json:"state"`
	}
	if err := a.get(ctx, "/api/sessions/"+a.id+"/orb/build/log", &bl); err != nil {
		return err
	}
	logState := "done"
	if bl.State == "building" {
		logState = "building"
	}
	rStatus := string(ro.Status)
	if rStatus == "" {
		rStatus = "none"
	}
	var bad []string
	want := func(what string, got, exp any) {
		if got != exp {
			bad = append(bad, fmt.Sprintf("%s = %v, the model says %v", what, got, exp))
		}
	}
	want("row status", rStatus, o.status())
	want("row up", ro.Up, o.up())
	want("row portal live", len(ro.Portals) > 0, o.portalLive())
	want("orb detail status", dStatus, o.status())
	want("orb detail up", dUp, o.up())
	want("build log", logState, o.buildLog())
	if len(bad) > 0 {
		return fmt.Errorf("serve's view of %s disagrees with the model in state %v: %s", a.id, o.state(), strings.Join(bad, "; "))
	}
	return nil
}

func (a *orbAdapter) get(ctx context.Context, path string, out any) error {
	return a.call(ctx, http.MethodGet, path, http.StatusOK, out)
}

// call sends one request and requires code back.
func (a *orbAdapter) call(ctx context.Context, method, path string, code int, out any) error {
	req, err := http.NewRequestWithContext(ctx, method, a.srv.URL+path, nil)
	if err != nil {
		return err
	}
	resp, err := a.srv.Client().Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != code {
		var e struct {
			Error string `json:"error"`
		}
		json.NewDecoder(resp.Body).Decode(&e)
		return fmt.Errorf("%s %s = %d (%s), want %d", method, path, resp.StatusCode, e.Error, code)
	}
	if out == nil {
		return nil
	}
	return json.NewDecoder(resp.Body).Decode(out)
}

// step opens an action: the spec's require, the field view it is
// judged on, and the name GetState records.
//
// A disabled action is skipped and the walk goes on, unlike the shared
// gate, which ends the walk there. The runner picks among all 21 actions
// at random and only a few are enabled in any state, so with the gate
// nearly every walk ended after its first step (300 walks took 36 steps
// in all). The runner still stops validating at the first disabled
// pick; everything after it is checked by GetState's views and by the
// walk's own trace, which records only the actions taken and is
// replayed on the graph whole (orbRun).
//
// With probe set, step only notes whether the action is enabled in that
// state and acts on nothing: orbGuided asks all 21 at one observation.
func (a *orbAdapter) step(name string, require func(orbFields) bool) (orbFields, bool, error) {
	if a.probe != nil {
		if require(*a.probe) {
			a.enabled = append(a.enabled, name)
		}
		return *a.probe, false, nil
	}
	o, err := a.observe()
	if err != nil || !require(o) {
		return o, false, err
	}
	a.action = name
	return o, true, nil
}

// --- the child ---

// ownerPID is the pid state.json names: the process that owns the orb.
func (a *orbAdapter) ownerPID(o orbFields) (int, error) {
	if o.owner == "cli" {
		return a.cli.Process.Pid, nil
	}
	b, err := os.ReadFile(filepath.Join(a.pids, a.id))
	if err != nil {
		return 0, err
	}
	var pid int
	_, err = fmt.Sscan(string(b), &pid)
	return pid, err
}

// write is one state.json write by the owning child: edit applies to
// what is on disk now (serve may have marked it stopped), and the write
// moves updatedAt, like orb's writeState.
func (a *orbAdapter) write(fresh bool, edit func(*orb.State)) error {
	o, err := a.observe()
	if err != nil {
		return err
	}
	pid, err := a.ownerPID(o)
	if err != nil {
		return err
	}
	st, err := orb.ReadState(a.home, a.id)
	if err != nil {
		return err
	}
	if fresh {
		st = orb.State{Session: a.id, Project: a.slug, Image: "img", Container: container.OrbName(a.id),
			Worktrees: map[string]string{"app": a.wt}}
	}
	edit(&st)
	st.PID = pid
	// Past both the last write and serve's last stop: updatedAt only moves on.
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

// timePhases keeps state.json's timed phases as orb.Start does: the
// step in progress is open, every other one ended. The row's chip names
// the open one, which is how the page shows a start's phase.
func timePhases(st *orb.State, now time.Time) {
	busy := st.Status == orb.StatusStarting || st.Status == orb.StatusBuilding
	n := len(st.Phases)
	if n > 0 && st.Phases[n-1].EndedAt.IsZero() && (!busy || st.Phases[n-1].Name != st.Phase) {
		st.Phases[n-1].EndedAt = now
	}
	if busy && (n == 0 || st.Phases[n-1].Name != st.Phase || !st.Phases[n-1].EndedAt.IsZero()) {
		st.Phases = append(st.Phases, orb.Phase{Name: st.Phase, StartedAt: now})
	}
}

// build writes the project's build.json as EnsureImage does.
func (a *orbAdapter) build(state string) error {
	b, _ := json.Marshal(orb.Build{Tag: "img", State: state, StartedAt: time.Now()})
	return writeFileAtomic(filepath.Join(filepath.Dir(orb.ImageLogPath(a.home, a.slug)), "build.json"), b)
}

func writeFileAtomic(path string, b []byte) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, b, 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}

// childStart is Prepare's first write: a fresh State, no portals.
func (a *orbAdapter) childStart() error {
	return a.write(true, func(s *orb.State) { s.Status, s.Phase = orb.StatusStarting, orb.PhaseSync })
}

func (a *orbAdapter) StartByServe() error {
	_, ok, err := a.step("StartByServe", func(o orbFields) bool { return o.owner == "none" })
	if !ok || err != nil {
		return err
	}
	pidfile := filepath.Join(a.pids, a.id)
	os.Remove(pidfile)
	if err := a.sup.Adopt(a.id); err != nil {
		return err
	}
	if err := waitUntil("the session child's pid", func() bool { _, err := os.Stat(pidfile); return err == nil }); err != nil {
		return err
	}
	return a.childStart()
}

func (a *orbAdapter) StartByCli() error {
	_, ok, err := a.step("StartByCli", func(o orbFields) bool { return o.owner == "none" })
	if !ok || err != nil {
		return err
	}
	a.cli = exec.Command("sleep", "3600")
	if err := a.cli.Start(); err != nil {
		a.cli = nil
		return err
	}
	return a.childStart()
}

func (a *orbAdapter) Prepared() error {
	_, ok, err := a.step("Prepared", func(o orbFields) bool {
		return o.owner != "none" && o.file == "starting" && o.phase == "prepare"
	})
	if !ok || err != nil {
		return err
	}
	if err := a.build("building"); err != nil {
		return err
	}
	return a.write(false, func(s *orb.State) { s.Status, s.Phase = orb.StatusBuilding, orb.PhaseBuild })
}

func (a *orbAdapter) BuildOk() error {
	_, ok, err := a.step("BuildOk", func(o orbFields) bool { return o.owner != "none" && o.file == "building" })
	if !ok || err != nil {
		return err
	}
	if err := a.build("ok"); err != nil {
		return err
	}
	return a.write(false, func(s *orb.State) { s.Status, s.Phase = orb.StatusStarting, orb.PhaseContainer })
}

func (a *orbAdapter) StartFails() error {
	o, ok, err := a.step("StartFails", func(o orbFields) bool {
		return o.owner != "none" && (o.file == "starting" || o.file == "building")
	})
	if !ok || err != nil {
		return err
	}
	if o.file == "building" {
		if err := a.build("failed"); err != nil {
			return err
		}
	}
	return a.write(false, func(s *orb.State) { s.Status, s.Error = orb.StatusFailed, "model: start failed" })
}

func (a *orbAdapter) ContainerUp() error {
	_, ok, err := a.step("ContainerUp", func(o orbFields) bool {
		return o.owner != "none" && o.file == "starting" && o.phase == "container"
	})
	if !ok || err != nil {
		return err
	}
	if err := a.rt.Start(context.Background(), container.RunSpec{Name: container.OrbName(a.id), Image: "img"}); err != nil {
		return err
	}
	return a.write(false, func(s *orb.State) { s.Phase, s.IP = orb.PhaseResume, "192.168.64.9" })
}

func (a *orbAdapter) Ready() error {
	_, ok, err := a.step("Ready", func(o orbFields) bool {
		return o.owner != "none" && o.file == "starting" && o.phase == "resume"
	})
	if !ok || err != nil {
		return err
	}
	return a.write(false, func(s *orb.State) { s.Status, s.Phase, s.Error = orb.StatusRunning, orb.PhaseReady, "" })
}

func (a *orbAdapter) Exec() error {
	_, ok, err := a.step("Exec", func(o orbFields) bool {
		return o.owner != "none" && o.ctr != "running" &&
			(o.file == "running" || o.file == "stopped" || (o.file == "failed" && (o.phase == "container" || o.phase == "resume")))
	})
	if !ok || err != nil {
		return err
	}
	return a.write(false, func(s *orb.State) { s.Status, s.Phase = orb.StatusStarting, orb.PhaseContainer })
}

// Activity is a turn writing history.
func (a *orbAdapter) Activity() error {
	_, ok, err := a.step("Activity", func(o orbFields) bool { return o.owner != "none" && o.idle })
	if !ok || err != nil {
		return err
	}
	return a.appendHistory("assistant", map[string]any{"text": "working"})
}

func (a *orbAdapter) Edit() error {
	_, ok, err := a.step("Edit", func(o orbFields) bool { return o.owner != "none" && o.file == "running" && !o.dirty })
	if !ok || err != nil {
		return err
	}
	return os.WriteFile(filepath.Join(a.wt, fmt.Sprintf("edit-%d.txt", time.Now().UnixNano())), []byte("work\n"), 0o644)
}

func (a *orbAdapter) Commit() error {
	_, ok, err := a.step("Commit", func(o orbFields) bool { return o.owner != "none" && o.file == "running" && o.dirty })
	if !ok || err != nil {
		return err
	}
	if err := git(a.wt, "add", "-A"); err != nil {
		return err
	}
	return git(a.wt, "-c", "user.name=model", "-c", "user.email=model@example.invalid", "commit", "-q", "-m", "work")
}

// OpenPortal listens on loopback in the owner's stead: the listener is
// the owning process's, so it goes when that process does.
func (a *orbAdapter) OpenPortal() error {
	_, ok, err := a.step("OpenPortal", func(o orbFields) bool { return o.owner != "none" && o.file == "running" && !o.portal })
	if !ok || err != nil {
		return err
	}
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return err
	}
	a.ln = ln
	go func() {
		for {
			c, err := ln.Accept()
			if err != nil {
				return
			}
			c.Close()
		}
	}()
	host := ln.Addr().(*net.TCPAddr).Port
	return a.write(false, func(s *orb.State) {
		s.Portals = []orb.PortalState{{Guest: orbGuest, Host: host, Name: "web"}}
	})
}

func (a *orbAdapter) ClosePortal() error {
	_, ok, err := a.step("ClosePortal", func(o orbFields) bool { return o.owner != "none" && o.portal })
	if !ok || err != nil {
		return err
	}
	a.closePortal()
	return a.write(false, func(s *orb.State) { s.Portals = nil })
}

func (a *orbAdapter) closePortal() {
	if a.ln != nil {
		a.ln.Close()
		a.ln = nil
	}
}

// Exit is the session ending cleanly: Orb.Stop, the portals closed,
// then the process goes.
func (a *orbAdapter) Exit() error {
	o, ok, err := a.step("Exit", func(o orbFields) bool {
		return o.owner != "none" && (o.file == "running" || o.file == "failed" || o.file == "stopped")
	})
	if !ok || err != nil {
		return err
	}
	if o.ctr == "running" && !a.exitKeepsContainer {
		if err := a.rt.Stop(context.Background(), container.OrbName(a.id)); err != nil {
			return err
		}
	}
	a.closePortal()
	if err := a.write(false, func(s *orb.State) { s.Status, s.Portals = orb.StatusStopped, nil }); err != nil {
		return err
	}
	return a.endOwner(o, syscall.SIGTERM)
}

// OwnerDies is the owner killed with no cleanup: state.json stays.
func (a *orbAdapter) OwnerDies() error {
	o, ok, err := a.step("OwnerDies", func(o orbFields) bool { return o.owner != "none" })
	if !ok || err != nil {
		return err
	}
	a.closePortal()
	return a.endOwner(o, syscall.SIGKILL)
}

// endOwner signals the owning process and waits until it is gone as far
// as serve can tell: reaped, and its lease dropped.
func (a *orbAdapter) endOwner(o orbFields, sig syscall.Signal) error {
	if o.owner == "cli" {
		a.endCLI(sig)
		return nil
	}
	pid, err := a.ownerPID(o)
	if err != nil {
		return err
	}
	if err := syscall.Kill(pid, sig); err != nil {
		return err
	}
	return waitUntil("serve's child to be reaped", func() bool { return !a.sup.Live(a.id) })
}

// endCLI ends the other bough and reaps it: an unreaped zombie still
// answers kill(pid, 0), which is not a live owner.
func (a *orbAdapter) endCLI(sig syscall.Signal) {
	if a.cli == nil {
		return
	}
	a.cli.Process.Signal(sig)
	a.cli.Wait()
	a.cli = nil
}

// --- serve ---

func (a *orbAdapter) StopOrb() error {
	_, ok, err := a.step("StopOrb", orbFields.up)
	if !ok || err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	return a.call(ctx, http.MethodPost, "/api/sessions/"+a.id+"/orb/stop", http.StatusOK, nil)
}

// Kill is the supervisor's hard stop of its child, which archive, Stop
// on a background agent and EndProject all come down to.
func (a *orbAdapter) Kill() error {
	_, ok, err := a.step("Kill", func(o orbFields) bool { return o.owner == "serve" })
	if !ok || err != nil {
		return err
	}
	// The portal's listener is the killed child's, so it goes with it.
	a.closePortal()
	return a.sup.Kill(a.id)
}

// Reap is a reaper tick once the idle limit has passed since the last
// activity GoIdle saw.
func (a *orbAdapter) Reap() error {
	_, ok, err := a.step("Reap", func(o orbFields) bool { return o.idle && (o.file == "running" || o.file == "failed") })
	if !ok || err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	a.api.ReapIdleOrbs(ctx, orbIdle, a.idleAt.Add(orbIdle))
	return nil
}

// GoIdle lets the idle limit pass with nothing written: the adapter's
// clock, which Reap hands the reaper.
func (a *orbAdapter) GoIdle() error {
	_, ok, err := a.step("GoIdle", func(o orbFields) bool { return !o.idle && (o.file == "running" || o.file == "failed") })
	if !ok || err != nil {
		return err
	}
	st, err := orb.ReadState(a.home, a.id)
	if err != nil {
		return err
	}
	a.idleAt = a.lastActivity(st)
	return nil
}

// Refresh is runningTTL passing: the next question re-reads the runtime.
func (a *orbAdapter) Refresh() error {
	_, ok, err := a.step("Refresh", func(o orbFields) bool { return o.snap != (o.ctr == "running") })
	if !ok || err != nil {
		return err
	}
	a.api.ForgetRunning()
	a.api.ContainerUp(a.id)
	return nil
}

// Remove is the page's Remove orb after its confirm: DELETE, which
// answers 409 while a worktree is dirty. Whether uncommitted work was
// lost is read off the worktree afterwards.
func (a *orbAdapter) Remove() error {
	o, ok, err := a.step("Remove", func(o orbFields) bool { return o.file != "none" && !o.up() && o.owner != "cli" })
	if !ok || err != nil {
		return err
	}
	// serve kills its own child first, and the portal's listener with it.
	if o.owner == "serve" {
		a.closePortal()
	}
	ctx, cancel := actionCtx()
	defer cancel()
	code := http.StatusOK
	if o.dirty {
		code = http.StatusConflict
	}
	if err := a.call(ctx, http.MethodDelete, "/api/sessions/"+url.PathEscape(a.id)+"/orb", code, nil); err != nil {
		return err
	}
	if _, err := os.Stat(a.wt); o.dirty && errors.Is(err, os.ErrNotExist) {
		a.lost = true
	}
	return nil
}

func (a *orbAdapter) appendHistory(kind string, data map[string]any) error {
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

func git(dir string, args ...string) error {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	out, err := exec.Command("git", append([]string{"-C", dir}, args...)...).CombinedOutput()
	if err != nil {
		return fmt.Errorf("git %v: %w: %s", args, err, out)
	}
	return nil
}

// waitUntil polls ok for up to actionTimeout.
func waitUntil(what string, ok func() bool) error {
	deadline := time.Now().Add(actionTimeout)
	for !ok() {
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for %s: timed out", what)
		}
		time.Sleep(5 * time.Millisecond)
	}
	return nil
}

var orbActions = map[string]map[string]fmbt.ActionFunc{"Orb": {
	"StartByServe": action((*orbAdapter).StartByServe),
	"StartByCli":   action((*orbAdapter).StartByCli),
	"Prepared":     action((*orbAdapter).Prepared),
	"BuildOk":      action((*orbAdapter).BuildOk),
	"StartFails":   action((*orbAdapter).StartFails),
	"ContainerUp":  action((*orbAdapter).ContainerUp),
	"Ready":        action((*orbAdapter).Ready),
	"Exec":         action((*orbAdapter).Exec),
	"Activity":     action((*orbAdapter).Activity),
	"Edit":         action((*orbAdapter).Edit),
	"Commit":       action((*orbAdapter).Commit),
	"OpenPortal":   action((*orbAdapter).OpenPortal),
	"ClosePortal":  action((*orbAdapter).ClosePortal),
	"Exit":         action((*orbAdapter).Exit),
	"OwnerDies":    action((*orbAdapter).OwnerDies),
	"StopOrb":      action((*orbAdapter).StopOrb),
	"Kill":         action((*orbAdapter).Kill),
	"Reap":         action((*orbAdapter).Reap),
	"GoIdle":       action((*orbAdapter).GoIdle),
	"Refresh":      action((*orbAdapter).Refresh),
	"Remove":       action((*orbAdapter).Remove),
}}

// A walk is 40 random picks, of which a dozen or so are enabled; a
// step is a few file writes and at most one process start, so a walk
// stays well inside runningTTL (10 s), which the adapter does not
// control: a snapshot that expired mid-walk would be a Refresh nobody
// took.
func orbOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 40, "max-parallel-runs": 0}
}

// orbGuided walks the spec itself, picking at random only among the
// actions whose require holds. The runner's picks are uniform over all
// 21 actions, and a running orb needs five particular ones in a row
// with OwnerDies, Kill and StartFails enabled at every step: in 200
// runner walks no walk ever took Edit, Commit or ClosePortal. These
// walks go through the same adapter, views and trace check.
//
// The actions that end or undo a start are picked a quarter as often as
// the rest: uniformly, OwnerDies, Kill and StartFails compete with each
// step of a start and a walk reached running about once in 250 tries.
func orbGuided(a *orbAdapter, walks, steps int, seed uint64) error {
	r := rand.New(rand.NewPCG(seed, seed))
	names := slices.Sorted(maps.Keys(orbActions["Orb"]))
	rare := map[string]bool{"OwnerDies": true, "Kill": true, "StartFails": true, "Exit": true, "Remove": true}
	for range walks {
		if err := a.Init(); err != nil {
			return err
		}
		a.GetState() // records Init; a failure is kept in a.failures
		for range steps {
			o, err := a.observe()
			if err != nil {
				return err
			}
			a.probe, a.enabled = &o, nil
			for _, n := range names {
				orbActions["Orb"][n](a, nil)
			}
			a.probe = nil
			var pool []string
			for _, n := range a.enabled {
				w := 4
				if rare[n] {
					w = 1
				}
				for range w {
					pool = append(pool, n)
				}
			}
			if len(pool) == 0 {
				break
			}
			n := pool[r.IntN(len(pool))]
			if _, err := orbActions["Orb"][n](a, nil); err != nil {
				return fmt.Errorf("guided walk, %s: %w", n, err)
			}
			if a.action == "" {
				return fmt.Errorf("guided walk: %s was enabled and did not act", n)
			}
			a.GetState()
		}
		if err := a.Cleanup(); err != nil {
			return err
		}
	}
	return nil
}

// orbRun walks the spec against a — the runner's walks, then guided
// ones — and replays every walk's own record (each action taken and
// the state observed after it) on the graph: the runner stops checking
// a walk at its first disabled pick, the replay checks all of it. It
// returns the first failure.
func orbRun(t *testing.T, a *orbAdapter) error {
	t.Helper()
	g, err := tracecheck.Load(fizzCheck(t, "orb_lifecycle"))
	if err != nil {
		t.Fatal(err)
	}
	if err := runMBT(t, "orb_lifecycle", a, orbActions, orbOptions()); err != nil {
		return fmt.Errorf("model-based run: %w", err)
	}
	if err := a.checkWalks(g); err != nil {
		return fmt.Errorf("model-based run: %w", err)
	}
	seed := uint64(time.Now().UnixNano())
	if s := os.Getenv("ORB_WALK_SEED"); s != "" {
		fmt.Sscan(s, &seed)
	}
	if err := orbGuided(a, 60, 40, seed); err != nil {
		return fmt.Errorf("guided walks, ORB_WALK_SEED=%d: %w", seed, err)
	}
	if err := a.checkWalks(g); err != nil {
		return fmt.Errorf("guided walks, ORB_WALK_SEED=%d: %w", seed, err)
	}
	return nil
}

// checkWalks fails on the first view serve got wrong, then on the first
// walk that is not a path in g.
func (a *orbAdapter) checkWalks(g *tracecheck.Graph) error {
	if len(a.failures) > 0 {
		return fmt.Errorf("%d steps where serve disagreed with the model; the first: %w", len(a.failures), a.failures[0])
	}
	for i, tr := range a.traces {
		if v := g.Check(tr); v != nil {
			b, _ := json.Marshal(tr)
			return fmt.Errorf("walk %d is not a path in the model: %v\ntrace: %s", i, v, b)
		}
	}
	// A step that changed nothing is right only where the model's action
	// changes nothing either: then fizz has no link for it ("not
	// enabled"; the adapter already checked its require). A link to
	// another state means serve should have changed something.
	for _, tr := range a.stutters {
		v := g.Check(tr)
		if v != nil && (v.Index != len(tr)-1 || !strings.Contains(v.Reason, "is not enabled")) {
			b, _ := json.Marshal(tr)
			return fmt.Errorf("step %s left the state unchanged where the model changes it: %v\ntrace: %s", tr[len(tr)-1].Action, v, b)
		}
	}
	return nil
}

func TestOrbLifecycle(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newOrbAdapter(t)
	if err := orbRun(t, a); err != nil {
		t.Fatal(err)
	}
	// A walk that never takes an action checks nothing about it.
	taken, steps := map[string]int{}, 0
	for _, tr := range a.traces {
		steps += len(tr) - 1
		for _, s := range tr[1:] {
			taken[strings.TrimPrefix(s.Action, "Orb#0.")]++
		}
	}
	t.Logf("%d walks, %d actions taken: %v", len(a.traces), steps, taken)
	for name := range orbActions["Orb"] {
		if taken[name] == 0 {
			t.Errorf("no walk took %s", name)
		}
	}
}

// A child that exits leaving its container running must fail the run:
// otherwise a green TestOrbLifecycle proves nothing.
func TestOrbLifecycleCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newOrbAdapter(t)
	a.exitKeepsContainer = true
	err := orbRun(t, a)
	if err == nil {
		t.Fatal("a run whose Exit leaves the container running passed; the walks are not checking state")
	}
	t.Logf("caught, as it must be: %.400s", err)
}
