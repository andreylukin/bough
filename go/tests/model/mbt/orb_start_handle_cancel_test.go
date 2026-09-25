//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	"github.com/andreylukin/bough/internal/container"
	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
	porb "github.com/andreylukin/bough/plugins/orb"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/orb_start_handle_cancel.fizz against the orb row itself: the
// plugins/orb row mounted on a kernel.Context in this process, as a
// project session's child mounts it, over a container.Fake the adapter
// holds at each step the spec names. A serve child would open the host's
// container runtime (orb_lifecycle says why that is never allowed here),
// and the handle this spec is about lives inside that child, where no
// API reaches it; so the adapter plays the child: the rows around the
// orb row are context-level stand-ins (history's path, the scratch dir,
// prompt-sections, /orb's registry, ui-mode), the orb row is real.
//
// Every walk runs in its own synctest bubble: close()'s one-minute wait
// is the bubble's clock (CloseTimeout sleeps past it), and
// synctest.Wait after each action lets every goroutine the action woke
// (the start, the Apply's watcher, a woken caller) settle before the
// state is read.
//
// Where the adapter holds the real thing (runtime.Available for an
// Apply, the chdir after Prepare, the start's container start) the
// walk is at the spec's step. Four of the spec's steps are the real
// system's next breath, with nothing to hold between them: the Apply's
// watcher runs as soon as the start settles (Notify), a Prepare failure
// settles the start at once (StartFail), close() stops what it opened
// as soon as the start settles (CloseSettled), and a headless Apply
// goes on with its waited start (HeadlessNonWebRun). The adapter keeps
// the spec ported to Go (oshShadow, checked against the path at every
// step) and compares the real state with the shadow after those forced
// steps. A path that takes another action between a step and its
// forced one follows the real system only when that action commutes
// with the forced step in the spec; otherwise the walk has asked for
// an interleaving the real system cannot be steered into (a reload in
// the instant between a start settling and its watcher running), and
// it is counted, not failed.
//
// What each field is read off:
//   - row: the kernel's row status, and the adapter's own in-flight
//     Reconcile/Remount (an Apply held at Available is "applying" on
//     the first mount, "stashed" on a reload; an unmount waiting in
//     close() is "closing").
//   - start, res, closes, cur: the handle (Starting, Orb, Ready, and
//     whether a reload's Apply provided the same one); before it is
//     provided, where the adapter holds the Apply.
//   - cancelled: the ctx the start runs on (the one Prepare got).
//   - ctr: the fake runtime; file: state.json; sec (and pending, which
//     the spec keeps equal to sec == "starting"): the orb prompt
//     section; stash: porb.Stashed.
//   - said: the "orb-notice" lines under the tui (async walks); a
//     headless run's failure is its row error. Counted once seen.
//   - waiter, exec_on, late: one caller, a goroutine that waits in the
//     handle's Ready and runs the handle's Command when WaiterWakes lets
//     it; exec_on is the fake runtime's exec of its argv.
//   - reply: what /orb stop answered, and whether the runtime saw a
//     stop; timedout: the unmount returned while the start had not
//     settled; mode, prep_err: the adapter's choice.

const oshSpec = "orb_start_handle_cancel"

// oshProbe is the caller's argv: its exec in the runtime's log is the
// command having run in the container.
const oshProbe = "osh-probe"

// ---- the spec, ported ----

// oshShadow is the spec's Handle role, field for field.
type oshShadow struct {
	Row, Mode, Start, Res, Ctr, File, Cur, Sec, Waiter, ExecOn, Reply string
	PrepErr, Cancelled, Pending, Timedout, Stash, Late                bool
	Said, Closes                                                      int
}

func newOshShadow() oshShadow {
	return oshShadow{Row: "init", Mode: "async", Start: "none", Ctr: "missing", File: "none", Cur: "fresh", Waiter: "none"}
}

func (s oshShadow) state() map[string]any {
	return map[string]any{
		"row": s.Row, "mode": s.Mode, "start": s.Start, "prep_err": s.PrepErr, "res": s.Res,
		"cancelled": s.Cancelled, "ctr": s.Ctr, "file": s.File, "cur": s.Cur, "pending": s.Pending,
		"sec": s.Sec, "said": s.Said, "waiter": s.Waiter, "exec_on": s.ExecOn, "reply": s.Reply,
		"closes": s.Closes, "timedout": s.Timedout, "stash": s.Stash, "late": s.Late,
	}
}

func (s *oshShadow) hasOrb() bool { return s.Start == "settled" && (s.Res == "ok" || s.Res == "setup") }

func (s *oshShadow) finishApply(reused bool) {
	s.Row = "mounted"
	s.Cur = "fresh"
	if reused {
		s.Cur = "reused"
	}
	if s.Start == "settled" {
		s.Pending = false
		s.runSettled()
	} else {
		s.Pending = true
		s.Sec = "starting"
	}
}

func (s *oshShadow) runSettled() {
	if s.hasOrb() {
		s.Sec = "ready"
		if s.Res == "setup" && s.Cur == "fresh" {
			s.Said++
		}
	} else {
		s.Sec = "failed"
		if s.Cur == "fresh" {
			s.Said++
		}
	}
}

func (s *oshShadow) settle(res, ctr, file string) {
	s.Start, s.Res, s.Ctr, s.File = "settled", res, ctr, file
	s.Closes++
}

func (s *oshShadow) runCmd() {
	s.Reply = ""
	s.Waiter = "none"
	if s.hasOrb() {
		if s.Ctr != "running" {
			s.Ctr, s.File = "running", "running"
		}
		s.ExecOn = "container"
		if s.Row != "mounted" {
			s.Late = true
		}
	}
}

func (s *oshShadow) dispose() { s.Pending, s.Sec = false, "" }

// apply takes one action; false when the spec does not enable it. mode
// is Mount's choice.
func (s *oshShadow) apply(name, mode string) bool {
	running := s.Start == "running"
	switch name {
	case "end":
		return true
	case "Mount":
		if s.Row != "init" {
			return false
		}
		s.Mode, s.Row = mode, "applying"
	case "RuntimeUnavailable":
		if s.Row != "applying" || s.Start != "none" {
			return false
		}
		s.Row = "failed"
	case "RuntimeOk":
		if s.Row != "applying" || s.Start != "none" {
			return false
		}
		s.Start, s.File = "preparing", "starting"
	case "PrepareOk", "PrepareFail":
		if s.Row != "applying" || s.Start != "preparing" {
			return false
		}
		s.Start = "running"
		if name == "PrepareFail" {
			s.PrepErr, s.File = true, "failed"
		}
		if s.Mode == "async" {
			s.finishApply(false)
		}
	case "StartOk":
		if !running || s.PrepErr || s.Cancelled {
			return false
		}
		s.settle("ok", "running", "running")
	case "ResumeScriptFails":
		if !running || s.PrepErr || s.Cancelled {
			return false
		}
		s.settle("setup", "running", "failed")
	case "StartFail":
		if !running || !(s.PrepErr || !s.Cancelled) {
			return false
		}
		s.settle("fail", "missing", "failed")
	case "CancelHonored":
		if !running || s.PrepErr || !s.Cancelled {
			return false
		}
		s.settle("cancel", "missing", "failed")
	case "RuntimeIgnoresCancel":
		if !running || s.PrepErr || !s.Cancelled {
			return false
		}
		s.settle("setup", "running", "failed")
	case "HeadlessNonWebRun":
		if s.Row != "applying" || s.Mode != "headless" || s.Start != "settled" {
			return false
		}
		if s.Res == "ok" {
			s.finishApply(false)
		} else {
			s.Said++
			s.Cancelled = true
			if s.hasOrb() {
				s.Ctr, s.File = "stopped", "stopped"
			}
			s.Row = "failed"
		}
	case "Notify":
		if s.Row != "mounted" || !s.Pending || s.Start != "settled" {
			return false
		}
		s.Pending = false
		s.runSettled()
	case "CallerExec":
		if s.Row != "mounted" || s.Waiter == "waiting" {
			return false
		}
		if s.Start == "settled" {
			s.runCmd()
		} else {
			s.Waiter, s.Reply = "waiting", ""
		}
	case "WaiterWakes":
		if s.Waiter != "waiting" || s.Start != "settled" {
			return false
		}
		s.runCmd()
	case "CallerCtxTimeout":
		if s.Waiter != "waiting" {
			return false
		}
		s.Waiter = "none"
	case "SlashOrbStopWhileStarting":
		if s.Row != "mounted" || s.Start == "settled" {
			return false
		}
		s.Cancelled, s.Reply = true, "err_starting"
	case "SlashOrbStopWhenReady":
		if s.Row != "mounted" || s.Start != "settled" {
			return false
		}
		if s.hasOrb() {
			s.Ctr, s.File, s.Reply = "stopped", "stopped", "ok"
		} else {
			s.Reply = "ok_no_orb"
		}
	case "HotReloadSameCfg":
		if s.Row != "mounted" {
			return false
		}
		s.dispose()
		s.Row, s.Stash = "stashed", true
	case "ReApplyUnavailable":
		if s.Row != "stashed" {
			return false
		}
		s.Row = "failed"
	case "ReloadReused":
		if !(s.Row == "stashed" || s.Row == "failed" && s.Stash) {
			return false
		}
		s.Stash = false
		s.finishApply(true)
	case "Unmount":
		switch s.Row {
		case "mounted":
			s.dispose()
			s.Cancelled, s.Row = true, "closing"
		case "failed":
			s.Row = "gone"
		default:
			return false
		}
	case "CloseSettled":
		if s.Row != "closing" || s.Start != "settled" {
			return false
		}
		if s.hasOrb() {
			s.Ctr, s.File = "stopped", "stopped"
		}
		s.Row = "gone"
	case "CloseTimeout":
		if s.Row != "closing" || s.Start == "settled" {
			return false
		}
		s.Timedout, s.Row = true, "gone"
	default:
		return false
	}
	return true
}

// forced is the step the real system takes in the same breath as the
// one that led to s ("" when none): see the file comment.
func (s oshShadow) forced() string {
	switch {
	case s.PrepErr && s.Start == "running":
		return "StartFail"
	case s.Row == "mounted" && s.Pending && s.Start == "settled":
		return "Notify"
	case s.Row == "closing" && s.Start == "settled":
		return "CloseSettled"
	case s.Row == "applying" && s.Mode == "headless" && s.Start == "settled":
		return "HeadlessNonWebRun"
	}
	return ""
}

// force takes the forced steps until none is left, and names them.
func (s oshShadow) force() (oshShadow, []string) {
	var took []string
	for f := s.forced(); f != ""; f = s.forced() {
		s.apply(f, "")
		took = append(took, f)
	}
	return s, took
}

// ---- the real system ----

// oshHandle is what the adapter uses of the orb row's handle.
type oshHandle interface {
	Starting() bool
	Orb() *iorb.Orb
	Ready(context.Context) error
	Command(context.Context, ...string) *exec.Cmd
}

// oshSections is prompt-sections: only "orb" matters.
type oshSections struct {
	mu sync.Mutex
	m  map[string]string
}

func (f *oshSections) Set(name, text string) { f.mu.Lock(); defer f.mu.Unlock(); f.m[name] = text }

func (f *oshSections) orb() string {
	f.mu.Lock()
	defer f.mu.Unlock()
	t := f.m["orb"]
	switch {
	case t == "":
		return ""
	case strings.Contains(t, "still starting"):
		return "starting"
	case strings.Contains(t, "failed to start"):
		return "failed"
	case strings.Contains(t, "Your shell runs in a Linux container ("):
		return "ready"
	}
	return "unknown: " + t
}

type oshHist string

func (p oshHist) Path() string { return string(p) }

type oshDir string

func (d oshDir) Dir() string { return string(d) }

// errHonorCancel makes a held call return its ctx's error.
var errHonorCancel = errors.New("honor the cancel")

type oshHeld struct {
	ctx context.Context
	ch  chan error
}

// oshRuntime is container.Fake with the adapter's holds: an Apply's
// Available while armed, the handle's own container start, and
// resume.sh failing on ResumeScriptFails. The proxy's resolv.conf probe
// fails, so no host proxy starts.
type oshRuntime struct {
	*container.Fake
	a *oshAdapter
}

func (r *oshRuntime) Available(ctx context.Context) error {
	a := r.a
	a.mu.Lock()
	armed := a.availArmed
	a.availArmed = false
	if !armed && a.prepCtx == nil {
		a.prepCtx = ctx // Prepare's: the ctx the start runs on
	}
	a.mu.Unlock()
	if armed {
		return a.hold("available", ctx)
	}
	return nil
}

func (r *oshRuntime) Start(ctx context.Context, spec container.RunSpec) error {
	a := r.a
	a.mu.Lock()
	first := !a.startSeen
	a.startSeen = true
	a.mu.Unlock()
	if first {
		if err := a.hold("start", ctx); err != nil {
			return err
		}
	}
	return r.Fake.Start(ctx, spec)
}

func (r *oshRuntime) Command(ctx context.Context, name string, opt container.ExecOptions, argv ...string) *exec.Cmd {
	cmd := r.Fake.Command(ctx, name, opt, argv...)
	a := r.a
	switch {
	case len(argv) > 2 && argv[0] == "sh" && argv[1] == "-c" && strings.Contains(argv[2], "resolv.conf"):
		cmd.Err = errors.New("fake: no resolv.conf")
	case len(argv) > 0 && argv[len(argv)-1] == a.resumePath():
		a.mu.Lock()
		fail := a.resumeFail
		a.mu.Unlock()
		if fail {
			cmd.Err = errors.New("fake: resume.sh exited 3")
		}
	}
	return cmd
}

// oshCur is the walk the process-global hooks serve. The walks run one
// at a time: StubHost's hooks and opened are the process's.
var (
	oshMu   sync.Mutex
	oshCur  *oshAdapter
	oshOnce sync.Once
)

func oshStubHost() {
	oshOnce.Do(func() {
		iorb.StubHost(func(string, ...string) string { return "" }, func() map[string]string { return nil })
		porb.StubHost(
			func() (string, error) { return oshCur.home, nil },
			func(string) error {
				a := oshCur
				a.mu.Lock()
				a.chdirCalled = true
				a.mu.Unlock()
				return a.hold("chdir", nil)
			},
			func() container.Runtime { return oshCur.rt },
		)
	})
}

// oshWaiter is one tools call on the handle.
type oshWaiter struct {
	ctx    context.Context
	cancel context.CancelFunc
	gate   chan struct{}
	done   chan struct{}
	ran    bool // its command ran in the container
	host   bool // its command would have run on the host
}

type oshAdapter struct {
	t    *testing.T
	home string
	n    int
	sess string
	slug string
	rt   *oshRuntime
	ctx  *kernel.Context
	secs *oshSections
	reg  *commands.Registry
	base []kernel.Row
	orb  kernel.Row

	// wrong is the deliberate bug the wrong-adapter test injects:
	// CancelHonored lets the start through as if nothing cancelled it.
	wrong bool

	mu          sync.Mutex
	holds       map[string]*oshHeld
	availArmed  bool
	startSeen   bool
	chdirCalled bool
	resumeFail  bool
	prepCtx     context.Context

	mode     string
	prepErr  bool
	inflight string // "mount", "reload", "retry" while that Reconcile runs
	flight   chan struct{}
	closing  chan struct{} // the unmount while it runs
	desired  bool          // the orb row is in the desired set
	unmount  bool          // Unmount removed it
	rowErr   string        // the orb row's failure, once seen
	h        oshHandle
	cur      string
	res      string
	said     map[string]bool
	waiter   *oshWaiter
	execOn   string
	reply    string
	timedout bool
	late     bool
}

func (a *oshAdapter) resumePath() string {
	return filepath.Join(projectdef.Root(a.home), a.slug, projectdef.FileResume)
}

// hold parks the calling goroutine until the adapter releases name.
func (a *oshAdapter) hold(name string, ctx context.Context) error {
	h := &oshHeld{ctx: ctx, ch: make(chan error)}
	a.mu.Lock()
	if a.holds[name] != nil {
		a.mu.Unlock()
		return fmt.Errorf("osh: %s held twice", name)
	}
	a.holds[name] = h
	a.mu.Unlock()
	err := <-h.ch
	if err == errHonorCancel {
		return ctx.Err()
	}
	return err
}

func (a *oshAdapter) held(name string) bool {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.holds[name] != nil
}

func (a *oshAdapter) release(name string, err error) error {
	a.mu.Lock()
	h := a.holds[name]
	delete(a.holds, name)
	a.mu.Unlock()
	if h == nil {
		return fmt.Errorf("nothing is held at %s", name)
	}
	h.ch <- err
	a.settle()
	return nil
}

// settle lets everything the last action woke run, then notes what
// finished: a Reconcile, an unmount, the handle a mount provided.
func (a *oshAdapter) settle() {
	synctest.Wait()
	if a.flight != nil {
		select {
		case <-a.flight:
			a.flight, a.inflight = nil, ""
		default:
		}
	}
	if a.closing != nil {
		select {
		case <-a.closing:
			a.closing = nil
			if a.h != nil && a.h.Starting() {
				a.timedout = true
			}
		default:
		}
	}
	if h, err := kernel.Get[oshHandle](a.ctx, "orb"); err == nil && h != a.h {
		if a.h != nil {
			a.cur = "fresh" // a second handle: the reload did not reuse
		}
		a.h = h
	}
	for _, r := range a.ctx.Rows() {
		if r.ID == "orb" && r.State == kernel.StateFailed && a.rowErr == "" {
			a.rowErr = r.Err.Error()
		}
	}
	if n, err := kernel.Get[string](a.ctx, "orb-notice"); err == nil {
		for _, l := range strings.Split(n, "\n") {
			if strings.HasPrefix(l, "orb for project ") {
				a.said[l] = true
			}
		}
	}
	if strings.Contains(a.rowErr, "orb for project ") {
		a.said["row: "+a.rowErr] = true
	}
	if a.res == "" && a.settled() {
		a.res = a.readRes()
	}
}

// reconcile runs a Reconcile of the desired rows on its own goroutine,
// as kind: it may stop in a hold.
func (a *oshAdapter) reconcile(kind string, f func() error) {
	done := make(chan struct{})
	a.inflight, a.flight = kind, done
	go func() {
		defer close(done)
		if err := f(); err != nil {
			a.t.Errorf("reconcile %s: %v", kind, err)
		}
	}()
	a.settle()
}

func (a *oshAdapter) rows(orb bool) []kernel.Row {
	rows := slices.Clone(a.base)
	if orb {
		rows = append(rows, a.orb)
	}
	return rows
}

func (a *oshAdapter) Init() error {
	a.n++
	a.slug = fmt.Sprintf("w%04d", a.n)
	a.sess = "osh" + a.slug
	p, err := projectdef.CreateEmpty(a.home, a.slug, "")
	if err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(p.Dir, projectdef.FileResume), []byte("#!/bin/sh\nexit 0\n"), 0o755); err != nil {
		return err
	}
	if p, err = projectdef.Load(a.home, a.slug); err != nil {
		return err
	}
	hash, err := projectdef.ImageHash(a.home, p)
	if err != nil {
		return err
	}
	a.rt = &oshRuntime{Fake: container.NewFake(), a: a}
	a.rt.AddImage(projectdef.ImageTag(a.slug, hash))
	a.holds = map[string]*oshHeld{}
	a.availArmed, a.startSeen, a.chdirCalled, a.resumeFail, a.prepCtx = false, false, false, false, nil
	a.mode, a.prepErr, a.inflight, a.flight, a.closing = "async", false, "", nil, nil
	a.desired, a.unmount, a.rowErr, a.h, a.cur, a.res = false, false, "", nil, "fresh", ""
	a.said, a.waiter, a.execOn, a.reply, a.timedout, a.late = map[string]bool{}, nil, "", "", false, false

	a.ctx = kernel.NewContext()
	a.secs = &oshSections{m: map[string]string{}}
	a.reg = commands.NewRegistry()
	a.ctx.Provide("session-mode", "project")
	a.ctx.Provide("session-project", a.slug)
	a.ctx.Provide("prompt-sections", a.secs)
	a.ctx.Provide("commands", a.reg)
	a.ctx.Provide("history", oshHist(filepath.Join(a.home, ".bough", "history", a.sess+".jsonl")))
	a.ctx.Provide("scratch", oshDir(filepath.Join(a.home, "scratch", a.sess)))
	a.base = nil
	a.orb = kernel.Row{ID: "orb", Plugin: "orb", Config: map[string]any{"runtime": "fake"}}
	return a.ctx.Mount(a.base)
}

// Cleanup lets everything the walk left parked go, so the bubble ends
// with no goroutine: held calls fail, a waiting caller gives up.
func (a *oshAdapter) Cleanup() error {
	if w := a.waiter; w != nil {
		w.cancel()
		select {
		case <-w.gate:
		default:
			close(w.gate)
		}
	}
	stop := errors.New("walk over")
	for {
		a.mu.Lock()
		var name string
		for n := range a.holds {
			name = n
		}
		a.mu.Unlock()
		if name == "" {
			break
		}
		a.release(name, stop)
	}
	synctest.Wait()
	a.ctx.Unmount()
	synctest.Wait()
	a.mu.Lock()
	left := len(a.holds)
	a.mu.Unlock()
	if left > 0 {
		return fmt.Errorf("%d calls still held after cleanup", left)
	}
	return nil
}

// ---- observation ----

func (a *oshAdapter) row() string {
	switch {
	case a.inflight == "mount":
		return "applying"
	case a.inflight == "reload":
		return "stashed"
	case a.inflight != "":
		return "retrying"
	case a.closing != nil:
		return "closing"
	case a.unmount:
		return "gone"
	case !a.desired:
		return "init"
	}
	for _, r := range a.ctx.Rows() {
		if r.ID == "orb" {
			switch r.State {
			case kernel.StateActive:
				return "mounted"
			case kernel.StateFailed:
				return "failed"
			}
			return "row " + string(r.State)
		}
	}
	return "no orb row"
}

// settled: the start has settled, read off the handle, or (a headless
// Apply that failed provides none) off the Apply having returned.
func (a *oshAdapter) settled() bool {
	if a.h != nil {
		return !a.h.Starting()
	}
	return a.chdirCalled && !a.held("chdir") && a.inflight == "" && a.rowErr != ""
}

func (a *oshAdapter) start() string {
	switch {
	case a.settled():
		return "settled"
	case a.h != nil:
		return "running"
	case !a.chdirCalled:
		return "none"
	case a.held("chdir"):
		return "preparing"
	}
	return "running"
}

// readRes is how the start settled.
func (a *oshAdapter) readRes() string {
	if a.h == nil {
		if strings.Contains(a.rowErr, "resume.sh") {
			return "setup"
		}
		return "fail"
	}
	if o := a.h.Orb(); o != nil {
		if strings.HasPrefix(o.State().Error, "resume.sh") {
			return "setup"
		}
		return "ok"
	}
	err := a.h.Ready(context.Background())
	if errors.Is(err, context.Canceled) || err != nil && strings.Contains(err.Error(), context.Canceled.Error()) {
		return "cancel"
	}
	return "fail"
}

func (a *oshAdapter) GetState() map[string]any {
	a.mu.Lock()
	pctx := a.prepCtx
	a.mu.Unlock()
	st := map[string]any{"mode": a.mode, "prep_err": a.prepErr, "cur": a.cur}
	st["row"] = a.row()
	st["start"] = a.start()
	closes := 0
	if a.settled() {
		closes = 1
	}
	st["closes"] = closes
	st["res"] = a.res
	st["cancelled"] = pctx != nil && errors.Is(pctx.Err(), context.Canceled)
	cs, _ := a.rt.Inspect(context.Background(), container.OrbName(a.sess))
	st["ctr"] = string(cs)
	file := "none"
	if s, err := iorb.ReadState(a.home, a.sess); err == nil && s.Status != "" {
		file = string(s.Status)
	}
	st["file"] = file
	sec := a.secs.orb()
	st["sec"] = sec
	st["pending"] = sec == "starting"
	st["said"] = len(a.said)
	waiter := "none"
	if a.waiter != nil {
		waiter = "waiting"
	}
	st["waiter"] = waiter
	st["exec_on"] = a.execOn
	st["reply"] = a.reply
	st["timedout"] = a.timedout
	st["stash"] = porb.Stashed(a.sess)
	st["late"] = a.late
	return st
}

// ---- actions ----

func (a *oshAdapter) Mount(mode string) error {
	a.mode = mode
	if mode == "headless" {
		a.ctx.Provide("ui-mode", "headless")
		a.ctx.Provide("origin", "cli")
	} else {
		a.ctx.Provide("ui-mode", "tui")
		a.ctx.Provide("origin", "tui")
	}
	a.desired = true
	a.availArmed = true
	a.reconcile("mount", func() error { return a.ctx.Reconcile(a.rows(true)) })
	if !a.held("available") {
		return errors.New("Mount: the Apply did not reach runtime.Available")
	}
	return nil
}

func (a *oshAdapter) startRelease(err error) error {
	if !a.held("start") {
		return errors.New("the start is not held at the container start")
	}
	return a.release("start", err)
}

func (a *oshAdapter) CallerExec() error {
	h, err := kernel.Get[oshHandle](a.ctx, "orb")
	if err != nil {
		return fmt.Errorf("CallerExec: %w", err)
	}
	a.reply = ""
	ctx, cancel := context.WithCancel(context.Background())
	w := &oshWaiter{ctx: ctx, cancel: cancel, gate: make(chan struct{}), done: make(chan struct{})}
	if !h.Starting() {
		close(w.gate)
	}
	before := a.execs()
	go func() {
		defer close(w.done)
		h.Ready(ctx) // the wait itself; Command below asks again
		<-w.gate
		if ctx.Err() != nil {
			return
		}
		cmd := h.Command(ctx, oshProbe)
		w.ran = a.execs() > before
		w.host = !w.ran && cmd.Err == nil
	}()
	a.waiter = w
	a.settle()
	if !h.Starting() {
		return a.finishWaiter()
	}
	return nil
}

func (a *oshAdapter) execs() int {
	n := 0
	for _, c := range a.rt.CallList() {
		if strings.HasPrefix(c, "exec ") && strings.HasSuffix(c, " "+oshProbe) {
			n++
		}
	}
	return n
}

// finishWaiter reads what the caller's command did once it returned.
func (a *oshAdapter) finishWaiter() error {
	w := a.waiter
	select {
	case <-w.done:
	default:
		return errors.New("the caller did not return")
	}
	a.waiter = nil
	a.reply = ""
	switch {
	case w.ran:
		a.execOn = "container"
		if a.row() != "mounted" {
			a.late = true
		}
	case w.host:
		a.execOn = "host"
	}
	return nil
}

func (a *oshAdapter) WaiterWakes() error {
	if a.waiter == nil {
		return errors.New("no caller is waiting")
	}
	close(a.waiter.gate)
	a.settle()
	return a.finishWaiter()
}

func (a *oshAdapter) CallerCtxTimeout() error {
	w := a.waiter
	if w == nil {
		return errors.New("no caller is waiting")
	}
	w.cancel()
	close(w.gate)
	a.settle()
	select {
	case <-w.done:
	default:
		return errors.New("the caller did not give up")
	}
	a.waiter = nil
	return nil
}

func (a *oshAdapter) slashOrbStop() error {
	stops := func() int {
		n := 0
		for _, c := range a.rt.CallList() {
			if strings.HasPrefix(c, "stop ") {
				n++
			}
		}
		return n
	}
	before := stops()
	out, err := a.reg.Run("orb", "stop")
	a.settle()
	switch {
	case err != nil && strings.Contains(err.Error(), "orb still starting"):
		a.reply = "err_starting"
	case err != nil:
		return fmt.Errorf("/orb stop: %w", err)
	case !strings.Contains(out, "orb stopped"):
		return fmt.Errorf("/orb stop said %q", out)
	case stops() > before:
		a.reply = "ok"
	default:
		a.reply = "ok_no_orb"
	}
	return nil
}

func (a *oshAdapter) HotReloadSameCfg() error {
	a.availArmed = true
	a.reconcile("reload", func() error { return a.ctx.Remount("orb") })
	if !a.held("available") {
		return errors.New("HotReloadSameCfg: the reload's Apply did not reach runtime.Available")
	}
	return nil
}

// ReloadReused lets the reload's Apply on, or retries a failed row the
// way a config reload does: the row disabled, then enabled again (a
// failed row with an unchanged spec is never retried).
func (a *oshAdapter) ReloadReused() error {
	if a.inflight != "reload" {
		off := a.orb
		off.Disabled = true
		if err := a.ctx.Reconcile(append(slices.Clone(a.base), off)); err != nil {
			return err
		}
		a.rowErr = ""
		a.availArmed = true
		a.reconcile("retry", func() error { return a.ctx.Reconcile(a.rows(true)) })
	}
	if err := a.release("available", nil); err != nil {
		return err
	}
	if h, err := kernel.Get[oshHandle](a.ctx, "orb"); err == nil && h == a.h {
		a.cur = "reused"
	}
	return nil
}

func (a *oshAdapter) Unmount() error {
	failed := a.row() == "failed"
	a.desired, a.unmount = false, true
	if failed {
		return a.ctx.Reconcile(a.rows(false))
	}
	done := make(chan struct{})
	a.closing = done
	go func() {
		defer close(done)
		if err := a.ctx.Reconcile(a.rows(false)); err != nil {
			a.t.Errorf("unmount: %v", err)
		}
	}()
	a.settle()
	return nil
}

func (a *oshAdapter) CloseTimeout() error {
	if a.closing == nil {
		return errors.New("no unmount is waiting in close()")
	}
	time.Sleep(time.Minute + time.Second)
	a.settle()
	return nil
}

// do runs the real side of one of the spec's actions.
func (a *oshAdapter) do(name, mode string) error {
	switch name {
	case "end":
		return nil
	case "Mount":
		return a.Mount(mode)
	case "RuntimeUnavailable":
		return a.release("available", errors.New("fake: container system not running"))
	case "RuntimeOk":
		return a.release("available", nil)
	case "PrepareOk":
		return a.release("chdir", nil)
	case "PrepareFail":
		a.prepErr = true
		return a.release("chdir", errors.New("fake: chdir refused"))
	case "StartOk":
		return a.startRelease(nil)
	case "ResumeScriptFails":
		a.mu.Lock()
		a.resumeFail = true
		a.mu.Unlock()
		err := a.startRelease(nil)
		a.mu.Lock()
		a.resumeFail = false
		a.mu.Unlock()
		return err
	case "StartFail":
		return a.startRelease(errors.New("fake: container start failed"))
	case "CancelHonored":
		if a.wrong {
			return a.startRelease(nil)
		}
		return a.startRelease(errHonorCancel)
	case "RuntimeIgnoresCancel":
		return a.startRelease(nil)
	case "CallerExec":
		return a.CallerExec()
	case "WaiterWakes":
		return a.WaiterWakes()
	case "CallerCtxTimeout":
		return a.CallerCtxTimeout()
	case "SlashOrbStopWhileStarting", "SlashOrbStopWhenReady":
		return a.slashOrbStop()
	case "HotReloadSameCfg":
		return a.HotReloadSameCfg()
	case "ReApplyUnavailable":
		return a.release("available", errors.New("fake: container system not running"))
	case "ReloadReused":
		return a.ReloadReused()
	case "Unmount":
		return a.Unmount()
	case "CloseTimeout":
		return a.CloseTimeout()
	}
	return fmt.Errorf("no real side for %s (it is only ever a forced step)", name)
}

// ---- walks ----

var errOshInfeasible = errors.New("an interleaving the real system cannot be steered into")

type oshPath struct {
	Links []int             `json:"links"`
	Trace []tracecheck.Step `json:"trace"`
}

// oshRole is a path state's Handle#0 fields by bare name.
func oshRole(s map[string]any) map[string]any {
	out := map[string]any{}
	for k, v := range s {
		if f, ok := strings.CutPrefix(k, "Handle#0."); ok {
			out[f] = v
		}
	}
	return out
}

func oshQualify(s map[string]any) map[string]any {
	out := map[string]any{}
	for k, v := range s {
		out["Handle#0."+k] = v
	}
	return out
}

// walkOsh runs one path in a bubble of its own and returns the first
// step whose state is not the spec's, or errOshInfeasible, and the
// journal of what the real system did, in its own order.
func walkOsh(t *testing.T, a *oshAdapter, n int, p oshPath) (journal []tracecheck.Step, walked int, err error) {
	t.Helper()
	synctest.Test(t, func(bt *testing.T) {
		a.t = bt
		defer func() {
			if cerr := a.Cleanup(); cerr != nil && err == nil {
				err = fmt.Errorf("path %d: cleanup: %w", n, cerr)
			}
		}()
		if ierr := a.Init(); ierr != nil {
			err = fmt.Errorf("path %d: Init: %w", n, ierr)
			return
		}
		// sh follows the path; rs is where the real system is: sh with
		// the forced steps taken (see the file comment).
		sh, rs := newOshShadow(), newOshShadow()
		journal = []tracecheck.Step{{Action: "Init", State: oshQualify(a.GetState())}}
		var names []string
		for i, st := range p.Trace {
			want := oshRole(st.State)
			if i > 0 {
				name := strings.TrimPrefix(st.Action, "Handle#0.")
				names = append(names, name)
				mode, _ := want["mode"].(string)
				realNoop := name == sh.forced()
				if !sh.apply(name, mode) {
					err = fmt.Errorf("path %d %v: the Go port of the spec does not enable %s", n, names, name)
					return
				}
				if !realNoop {
					if !rs.apply(name, mode) {
						err = fmt.Errorf("path %d %v: %w", n, names, errOshInfeasible)
						return
					}
					var took []string
					rs, took = rs.force()
					if derr := a.do(name, mode); derr != nil {
						err = fmt.Errorf("path %d %v: %w", n, names, derr)
						return
					}
					journal = append(journal, tracecheck.Step{Action: st.Action})
					for _, f := range took {
						journal = append(journal, tracecheck.Step{Action: "Handle#0." + f})
					}
					journal[len(journal)-1].State = oshQualify(a.GetState())
				}
				if fs, _ := sh.force(); !reflect.DeepEqual(fs, rs) {
					err = fmt.Errorf("path %d %v: %w", n, names, errOshInfeasible)
					return
				}
			}
			if got := oshRound(sh.state()); !reflect.DeepEqual(got, oshRound(want)) {
				err = fmt.Errorf("path %d %v: the Go port of the spec drifted\n got %v\nwant %v", n, names, got, oshRound(want))
				return
			}
			if got, exp := oshRound(a.GetState()), oshRound(rs.state()); !reflect.DeepEqual(got, exp) {
				err = fmt.Errorf("path %d %v: state\n%s", n, names, oshDiff(exp, got))
				return
			}
			walked = i
		}
	})
	return journal, walked, err
}

func oshRound(v any) map[string]any {
	b, _ := json.Marshal(v)
	var out map[string]any
	json.Unmarshal(b, &out)
	return out
}

func oshDiff(want, got map[string]any) string {
	var b strings.Builder
	keys := make([]string, 0, len(want))
	for k := range want {
		keys = append(keys, k)
	}
	slices.Sort(keys)
	for _, k := range keys {
		if !reflect.DeepEqual(want[k], got[k]) {
			fmt.Fprintf(&b, "  %s: spec %v, real %v\n", k, want[k], got[k])
		}
	}
	return b.String()
}

func oshPaths(t *testing.T, cover tracecheck.Cover) []oshPath {
	t.Helper()
	b, err := pathsJSONCover(oshSpec, cover)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []oshPath `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		t.Fatal(err)
	}
	if len(doc.Paths) == 0 {
		t.Fatal("no paths")
	}
	return doc.Paths
}

func newOshAdapter(t *testing.T) *oshAdapter {
	oshMu.Lock()
	t.Cleanup(oshMu.Unlock)
	oshStubHost()
	a := &oshAdapter{t: t, home: t.TempDir()}
	oshCur = a
	return a
}

func oshGraph(t *testing.T) *tracecheck.Graph {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath(oshSpec)), "..", "testdata", oshSpec))
	if err != nil {
		t.Fatal(err)
	}
	return g
}

// Every path over the checked-in graph (every settled state; every
// transition under MODEL_COVER=transitions) against the orb row, the
// whole role state compared after each step, and the journal of each
// walk (what the row did, in the order it did it) replayed on the
// graph: this row writes nothing to a session's history, so the
// journal is its trace. A generated path that asks for an interleaving
// the row cannot be steered into stops there; what the paths then left
// unwalked of the steerable part of the graph is walked by paths made
// from it (oshSteer), so every settled state, or every transition, the
// real row can be driven to is walked.
func TestOrbStartHandleCancelPaths(t *testing.T) {
	t.Parallel()
	cover := envCover()
	g := oshGraph(t)
	st := newOshSteer(g)
	a := newOshAdapter(t)
	walk := func(i int, p oshPath) bool {
		journal, walked, err := walkOsh(t, a, i, p)
		st.mark(p.Links, walked)
		switch {
		case errors.Is(err, errOshInfeasible):
			return false
		case err != nil:
			t.Error(err)
			return true
		}
		if v := g.Check(journal); v != nil {
			b, _ := json.Marshal(journal)
			t.Errorf("path %d: the journal is not a path in the model: %v\ntrace: %s", i, v, b)
		}
		return true
	}
	paths := oshPaths(t, cover)
	infeasible := 0
	for i, p := range paths {
		if !walk(i, p) {
			infeasible++
		}
		if t.Failed() && testing.Short() {
			return
		}
	}
	more := st.walks(cover)
	for i, p := range more {
		if !walk(len(paths)+i, p) {
			t.Errorf("path %d, made of steerable steps only, was not steerable", len(paths)+i)
		}
		if t.Failed() && testing.Short() {
			return
		}
	}
	if left := st.left(cover); left > 0 && !t.Failed() {
		t.Errorf("%d steerable targets (%s) left unwalked", left, cover)
	}
	t.Logf("%d generated walks (%d stop at an interleaving the row cannot be steered into), %d more over the rest of the steerable graph; %d of the graph's %d transitions can be driven on the real row",
		len(paths), infeasible, len(more), st.feasible, st.total)
}

// A CancelHonored that lets the start through (as if the cancel never
// reached the runtime) shows res "ok" where the spec says "cancel": a
// walk through that transition must fail.
func TestOrbStartHandleCancelCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	paths := oshPaths(t, tracecheck.CoverTransitions)
	a := newOshAdapter(t)
	a.wrong = true
	for i, p := range paths {
		if !slices.ContainsFunc(p.Trace, func(s tracecheck.Step) bool { return s.Action == "Handle#0.CancelHonored" }) {
			continue
		}
		if _, _, err := walkOsh(t, a, i, p); err != nil && !errors.Is(err, errOshInfeasible) {
			t.Logf("caught: %.600s", err)
			return
		}
	}
	t.Fatal("walks whose CancelHonored lets the start through passed; the walks are not checking state")
}

// ---- the steerable graph ----

// oshStep is one action from a settled state: its link, the fork links
// after it, and the settled state it reaches.
type oshStep struct {
	links     []int
	src, dest int
	name      string
}

// oshSteer is the graph's steerable part: the steps the real row can be
// driven through from where it really is (the source state with its
// forced steps taken), and which of them the walks have taken.
type oshSteer struct {
	g        *tracecheck.Graph
	out      map[int][]oshStep // settled node -> its steps
	ok       map[int]bool      // action link -> steerable
	reach    map[int]bool      // settled nodes reachable by steerable steps
	done     map[int]bool      // action links walked
	seen     map[int]bool      // settled nodes walked to
	feasible int               // steerable action links out of reach
	total    int               // action links out of settled nodes
}

func oshShadowOf(s map[string]any) oshShadow {
	r := oshRole(s)
	str := func(k string) string { v, _ := r[k].(string); return v }
	bl := func(k string) bool { v, _ := r[k].(bool); return v }
	num := func(k string) int { v, _ := r[k].(float64); return int(v) }
	return oshShadow{
		Row: str("row"), Mode: str("mode"), Start: str("start"), Res: str("res"), Ctr: str("ctr"),
		File: str("file"), Cur: str("cur"), Sec: str("sec"), Waiter: str("waiter"), ExecOn: str("exec_on"),
		Reply: str("reply"), PrepErr: bl("prep_err"), Cancelled: bl("cancelled"), Pending: bl("pending"),
		Timedout: bl("timedout"), Stash: bl("stash"), Late: bl("late"), Said: num("said"), Closes: num("closes"),
	}
}

// oshSteerable: from the real row (src with its forced steps taken) the
// step leaves it where dest, forced, says; the test walkOsh makes.
func oshSteerable(src, dest map[string]any, name string) bool {
	sh := oshShadowOf(src)
	if name == sh.forced() {
		return true
	}
	mode, _ := oshRole(dest)["mode"].(string)
	rs, _ := sh.force()
	if !sh.apply(name, mode) || !rs.apply(name, mode) {
		return false
	}
	fs, _ := sh.force()
	rs, _ = rs.force()
	return reflect.DeepEqual(fs, rs)
}

func newOshSteer(g *tracecheck.Graph) *oshSteer {
	st := &oshSteer{g: g, out: map[int][]oshStep{}, ok: map[int]bool{}, done: map[int]bool{}, seen: map[int]bool{0: true}}
	from := map[int][]int{}
	for i, l := range g.Links {
		from[l.Src] = append(from[l.Src], i)
	}
	// forks follows the fork links from an action's intermediate node to
	// the settled states it reaches.
	var forks func(n int, via []int, emit func([]int, int))
	forks = func(n int, via []int, emit func([]int, int)) {
		if g.Nodes[n].Name == "yield" {
			emit(via, n)
			return
		}
		for _, i := range from[n] {
			if g.Links[i].Type != "action" {
				forks(g.Links[i].Dest, append(slices.Clone(via), i), emit)
			}
		}
	}
	for i, l := range g.Links {
		if l.Type != "action" || g.Nodes[l.Src].Name != "yield" {
			continue
		}
		st.total++
		name := strings.TrimPrefix(l.Name, "Handle#0.")
		steerable := true
		forks(l.Dest, []int{i}, func(via []int, dest int) {
			st.out[l.Src] = append(st.out[l.Src], oshStep{links: via, src: l.Src, dest: dest, name: name})
			if !oshSteerable(g.Nodes[l.Src].State, g.Nodes[dest].State, name) {
				steerable = false
			}
		})
		st.ok[i] = steerable
	}
	st.reach = map[int]bool{0: true}
	for frontier := []int{0}; len(frontier) > 0; {
		var next []int
		for _, n := range frontier {
			for _, s := range st.out[n] {
				if st.ok[s.links[0]] && !st.reach[s.dest] {
					st.reach[s.dest] = true
					next = append(next, s.dest)
				}
			}
		}
		frontier = next
	}
	for n := range st.reach {
		counted := map[int]bool{}
		for _, s := range st.out[n] {
			if !counted[s.links[0]] && st.ok[s.links[0]] {
				counted[s.links[0]] = true
				st.feasible++
			}
		}
	}
	return st
}

// mark records a path's first walked steps as walked.
func (st *oshSteer) mark(links []int, walked int) {
	n := 0
	for _, i := range links {
		l := st.g.Links[i]
		if l.Type == "action" {
			if n == walked {
				return
			}
			n++
			st.done[i] = true
		}
		st.seen[l.Dest] = true
	}
}

// target: a steerable step not walked yet (transitions), or one to a
// settled state not walked to (states).
func (st *oshSteer) target(s oshStep, cover tracecheck.Cover) bool {
	if !st.ok[s.links[0]] {
		return false
	}
	if cover == tracecheck.CoverTransitions {
		return !st.done[s.links[0]]
	}
	return !st.seen[s.dest]
}

func (st *oshSteer) left(cover tracecheck.Cover) int {
	n := 0
	for src := range st.reach {
		for _, s := range st.out[src] {
			if st.target(s, cover) {
				n++
			}
		}
	}
	return n
}

// walks are paths of steerable steps only that take every target the
// generated paths left: from where a walk is, the nearest target by
// BFS, until none is reachable or the walk is 50 steps long.
func (st *oshSteer) walks(cover tracecheck.Cover) []oshPath {
	var out []oshPath
	for {
		cur := 0
		var taken []oshStep
		for len(taken) < 50 {
			parent := map[int]oshStep{}
			seen := map[int]bool{cur: true}
			var hit *oshStep
			for frontier := []int{cur}; len(frontier) > 0 && hit == nil; {
				var next []int
				for _, n := range frontier {
					for _, s := range st.out[n] {
						switch {
						case !st.ok[s.links[0]]:
						case st.target(s, cover):
							if hit == nil {
								hit = &s
							}
						case !seen[s.dest]:
							seen[s.dest] = true
							parent[s.dest] = s
							next = append(next, s.dest)
						}
					}
				}
				frontier = next
			}
			if hit == nil {
				break
			}
			var chain []oshStep
			for n := hit.src; n != cur; n = parent[n].src {
				chain = append([]oshStep{parent[n]}, chain...)
			}
			chain = append(chain, *hit)
			for _, s := range chain {
				st.done[s.links[0]] = true
				st.seen[s.dest] = true
			}
			taken = append(taken, chain...)
			cur = hit.dest
		}
		if len(taken) == 0 {
			return out
		}
		p := oshPath{Trace: []tracecheck.Step{{Action: "Init", State: st.g.Nodes[0].State}}}
		for _, s := range taken {
			p.Links = append(p.Links, s.links...)
			p.Trace = append(p.Trace, tracecheck.Step{Action: st.g.Links[s.links[0]].Name, State: st.g.Nodes[s.dest].State})
		}
		out = append(out, p)
	}
}
