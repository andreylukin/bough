//go:build !windows

package mbt

import (
	"fmt"
	"os"
	"path/filepath"
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/background_job_survives_restart.fizz against a real serve: one
// session with one background bash job, taken through a restart that
// SIGKILLs its headless child (as `bough update`/`bough restart` does)
// while the job is still running.
//
// The adapter starts the job with a real "bash" tool call (background:
// true), reads its own pid off a file it writes as its first line, and
// checks `running` by signalling that pid directly: the job's real OS
// process outlives the session's child (jobs.go's start() never calls
// Setpgid, so nothing in the restart path can reach it by the child's
// death alone), regardless of whether serve itself, or the session, is
// still around to know about it.
//
// RestartKillsChild and ReapWritesHistory are one breath in the real
// system: Supervisor.Close (which servetest.Server.Shutdown's SIGTERM
// reaches, the same path restartServe's SIGINT does) SIGKILLs the
// child and its own reap goroutine appends the orphan-job "finished,
// stopped:true" entry before the kill call returns. The adapter does
// the real work — the actual Shutdown, and a real read of the on-disk
// history confirming that entry landed — inside RestartKillsChild, but
// holds typed/reachable back (`deferred`) until ReapWritesHistory asks
// for them, so a walk that picks the two actions in the spec's order
// sees the spec's two states rather than the real system's one.

// bjsrJobScript is the background job: it writes its own pid first so
// the adapter can check it is alive from the test process directly (no
// serve, no session, needed), then waits for an "exit" file a real OS
// process, not the JSON in any history file, is the ground truth for.
func bjsrJobScript(dir string) string {
	return fmt.Sprintf(`D=%q
echo $$ > "$D/pid"
while [ ! -e "$D/exit" ]; do sleep 0.02; done
exit 0`, dir)
}

type bjsrAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	walk   int
	id     string
	ids    []string
	jobDir string
	seq    int
	held   string // the llm-control turn holding the open request, "" when none

	pid int // the job's real OS pid, once started

	// The spec's Session role, field for field.
	child     string // alive | killed | gone | resumed
	job       string // running | orphaned | exited
	typed     string // none | started | closed
	reachable bool
	delivered bool

	// deferredReap is set by RestartKillsChild once the real Shutdown
	// and its reap have already happened, and cleared by
	// ReapWritesHistory, which only then reveals typed/reachable/child
	// as "closed"/"false"/"gone": see the file comment.
	deferredReap bool

	// reportClosedEarly is TestBackgroundJobSurvivesRestartCatchesWrongAdapter's
	// bug: it reveals "closed" right after Init, before anything real
	// closed the job.
	reportClosedEarly bool
}

func newBJSRAdapter(t *testing.T) *bjsrAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &bjsrAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

// Init starts a fresh session and a real background job on it: the
// model's first response is a bash tool call, its second holds the turn
// open (never released — a restart, not a finished turn, is the point).
func (a *bjsrAdapter) Init() error {
	a.gate.reset()
	a.walk++
	// A walk that reached RestartKillsChild but not NewChildStarts leaves
	// serve down for good otherwise: Shutdown/Resume, as
	// serveRestartResumeAdapter's Init does, so every walk starts on a
	// serve that is definitely up, whatever the last one did to it.
	// Shutdown on an already-exited serve is a no-op.
	a.s.Shutdown()
	if err := a.s.Resume(""); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	a.jobDir = filepath.Join(a.s.Root, fmt.Sprintf("bjsr-job-%04d", a.walk))
	if err := os.MkdirAll(a.jobDir, 0o755); err != nil {
		return err
	}
	a.pid = 0
	a.child, a.job, a.typed = "alive", "running", "started"
	a.reachable, a.delivered = true, false
	a.deferredReap = false

	call := a.nextTurn()
	control.Queue(a.t, a.dir, call, control.Turn{Mode: "call", Tool: "bash", Args: map[string]any{
		"command": bjsrJobScript(a.jobDir), "background": true, "timeout": "300s",
	}})
	hold := a.nextTurn()
	control.Queue(a.t, a.dir, hold, control.Turn{Mode: "block", Text: "holding"})
	if err := a.s.Prompt(ctx, a.id, "start a background job"); err != nil {
		return err
	}
	control.WaitTaken(a.t, a.dir, call, actionTimeout)
	control.WaitTaken(a.t, a.dir, hold, actionTimeout)
	a.held = hold

	if _, err := a.waitHistory("the job's started entry", func(e history.Entry) bool {
		return e.Kind == "job" && e.Data["event"] == "started"
	}); err != nil {
		return err
	}
	return a.readPID()
}

func (a *bjsrAdapter) nextTurn() string {
	a.seq++
	return fmt.Sprintf("t%06d", a.seq)
}

// readPID waits for the job to have written its pid file: it is the
// job's first line, before it does anything else.
func (a *bjsrAdapter) readPID() error {
	deadline := time.Now().Add(actionTimeout)
	path := filepath.Join(a.jobDir, "pid")
	for {
		b, err := os.ReadFile(path)
		if err == nil {
			var pid int
			if _, err := fmt.Sscanf(string(b), "%d", &pid); err == nil && pid > 0 {
				a.pid = pid
				return nil
			}
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("job pid file %s never appeared", path)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// Cleanup ends whatever is left running: the job (so no walk leaks a
// real process), then the session's child, whether or not it survived
// the walk's restart.
func (a *bjsrAdapter) Cleanup() error {
	os.WriteFile(filepath.Join(a.jobDir, "exit"), nil, 0o644)
	if a.pid > 0 {
		deadline := time.Now().Add(actionTimeout)
		for processAlive(a.pid) && time.Now().Before(deadline) {
			time.Sleep(20 * time.Millisecond)
		}
	}
	if a.child == "alive" {
		ctx, cancel := actionCtx()
		defer cancel()
		a.s.Archive(ctx, a.id)
	}
	a.held = ""
	return nil
}

func (a *bjsrAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

func (a *bjsrAdapter) GetState() (map[string]any, error) {
	typed := a.typed
	if a.reportClosedEarly {
		typed = "closed"
	}
	return map[string]any{
		"child":     a.child,
		"job":       a.job,
		"typed":     typed,
		"running":   processAlive(a.pid),
		"reachable": a.reachable,
		"delivered": a.delivered,
	}, nil
}

func processAlive(pid int) bool {
	if pid <= 0 {
		return false
	}
	return syscall.Kill(pid, 0) == nil
}

// waitHistory polls the session's on-disk history (real regardless of
// whether the child answering the API is up) for a new entry ok holds.
func (a *bjsrAdapter) waitHistory(what string, ok func(history.Entry) bool) ([]history.Entry, error) {
	deadline := time.Now().Add(actionTimeout)
	path := filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl")
	for {
		entries, err := history.Read(path)
		if err == nil {
			for _, e := range entries {
				if ok(e) {
					return entries, nil
				}
			}
		}
		if time.Now().After(deadline) {
			return nil, fmt.Errorf("waiting %s for %s", actionTimeout, what)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// JobFinishesNaturally: the job exits on its own while the child is
// still up, so Jobs delivers its notice in-process, live — the ordinary
// path background_bash_jobs.fizz covers.
func (a *bjsrAdapter) JobFinishesNaturally() error {
	if !a.gate.pass(a.job == "running" && a.child == "alive") {
		return nil
	}
	if err := os.WriteFile(filepath.Join(a.jobDir, "exit"), nil, 0o644); err != nil {
		return err
	}
	if _, err := a.waitHistory("the job's natural finish", func(e history.Entry) bool {
		return e.Kind == "job" && e.Data["event"] == "finished" && e.Data["stopped"] != true
	}); err != nil {
		return err
	}
	deadline := time.Now().Add(actionTimeout)
	for processAlive(a.pid) {
		if time.Now().After(deadline) {
			return fmt.Errorf("job pid %d still alive after its finish was recorded", a.pid)
		}
		time.Sleep(20 * time.Millisecond)
	}
	a.job, a.typed, a.reachable, a.delivered = "exited", "closed", false, true
	return nil
}

// RestartKillsChild: `bough update`/`bough restart`'s path — Shutdown
// is the real SIGTERM Supervisor.Close answers, which SIGKILLs the
// child directly and, before it returns, reaps it (endOrphanJobs writes
// the orphan job's forced "finished" unconditionally). The adapter
// confirms that on disk here but defers revealing it (see file comment)
// until ReapWritesHistory.
func (a *bjsrAdapter) RestartKillsChild() error {
	if !a.gate.pass(a.child == "alive") {
		return nil
	}
	wasRunning := a.job == "running"
	a.s.Shutdown()
	if wasRunning {
		if _, err := a.waitHistory("the reaper's forced finish", func(e history.Entry) bool {
			return e.Kind == "job" && e.Data["event"] == "finished" && e.Data["stopped"] == true
		}); err != nil {
			return err
		}
		a.job = "orphaned"
	}
	a.child = "killed"
	a.deferredReap = true
	return nil
}

// ReapWritesHistory: nothing left to do to the real system — the reap
// already ran, inside RestartKillsChild's Shutdown call — only to the
// adapter's own record, now that the spec is ready to see it.
func (a *bjsrAdapter) ReapWritesHistory() error {
	if !a.gate.pass(a.child == "killed") {
		return nil
	}
	if !a.deferredReap {
		return fmt.Errorf("ReapWritesHistory reached with nothing deferred from RestartKillsChild")
	}
	a.child = "gone"
	a.typed = "closed"
	a.reachable = false
	a.deferredReap = false
	return nil
}

// NewChildStarts: serve comes back up (a.s.Resume, the part of a
// restart outside this one session's role) and the session's next
// input is what actually spawns its fresh, empty-registry child.
func (a *bjsrAdapter) NewChildStarts() error {
	if !a.gate.pass(a.child == "gone") {
		return nil
	}
	if err := a.s.Resume(""); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	name := a.nextTurn()
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "ok", Text: "hi again"})
	if err := a.s.Prompt(ctx, a.id, "are you there"); err != nil {
		return err
	}
	if _, err := waitRow(a.s, a.id, "the resumed child to answer", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		return err
	}
	a.child = "resumed"
	a.held = ""
	return nil
}

// OrphanProcessExits: the orphaned job is not immortal. Nothing in
// bough is watching it any more (delivered stays false forever, per
// the spec's NoDeliveryForOrphanedJob), but the process itself ends.
func (a *bjsrAdapter) OrphanProcessExits() error {
	if !a.gate.pass(a.job == "orphaned" && processAlive(a.pid)) {
		return nil
	}
	if err := os.WriteFile(filepath.Join(a.jobDir, "exit"), nil, 0o644); err != nil {
		return err
	}
	deadline := time.Now().Add(actionTimeout)
	for processAlive(a.pid) {
		if time.Now().After(deadline) {
			return fmt.Errorf("orphaned job pid %d never exited", a.pid)
		}
		time.Sleep(20 * time.Millisecond)
	}
	return nil
}

var bjsrActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"JobFinishesNaturally": action((*bjsrAdapter).JobFinishesNaturally),
	"RestartKillsChild":    action((*bjsrAdapter).RestartKillsChild),
	"ReapWritesHistory":    action((*bjsrAdapter).ReapWritesHistory),
	"NewChildStarts":       action((*bjsrAdapter).NewChildStarts),
	"OrphanProcessExits":   action((*bjsrAdapter).OrphanProcessExits),
}, "": {
	// deadlock_detection: false (Settles is only reached eventually, not
	// on every path) makes fizz link a settled state with nothing left
	// enabled to itself as a role-less "end", same as
	// background_bash_jobs.fizz's: the runner offers it everywhere, and
	// a taken "end" matches no real link, so it closes the gate like any
	// disabled pick.
	"end": func(m any, _ []fmbt.Arg) (any, error) {
		m.(*bjsrAdapter).gate.pass(false)
		return nil, errDisabled
	},
}}

// Each walk restarts serve at most once and starts one real background
// job, so it costs real wall time; the default keeps the random run
// short while still reaching every state (the spec has few of them).
func bjsrOptions() map[string]any {
	return map[string]any{"max-seq-runs": 40, "max-actions": 6, "max-parallel-runs": 0}
}

// bjsrHistory projects a session's transcript to the spec's steps. The
// job's "started" entry is Init; a natural "finished" (no stopped) is
// JobFinishesNaturally; the reaper's forced "finished" (stopped: true)
// is the one entry that records what are two of the spec's actions,
// RestartKillsChild and ReapWritesHistory, at once (see the file
// comment) so it projects as both, in order. Nothing else the flow does
// — the restart's own resume, the new child, the orphan's own exit —
// ever lands as an entry in this session's history.
func bjsrHistory(entries []history.Entry) []tracecheck.Step {
	state := func(child, job, typed string, running, reachable, delivered bool) map[string]any {
		return map[string]any{
			"Session#0.child": child, "Session#0.job": job, "Session#0.typed": typed,
			"Session#0.running": running, "Session#0.reachable": reachable, "Session#0.delivered": delivered,
		}
	}
	steps := []tracecheck.Step{{Action: "Init", State: state("alive", "running", "started", true, true, false)}}
	for _, e := range entries {
		if e.Kind != "job" || e.Data["event"] != "finished" {
			continue
		}
		if e.Data["stopped"] == true {
			steps = append(steps,
				tracecheck.Step{Action: "Session#0.RestartKillsChild", State: state("killed", "orphaned", "started", true, true, false)},
				tracecheck.Step{Action: "Session#0.ReapWritesHistory", State: state("gone", "orphaned", "closed", true, false, false)})
		} else {
			steps = append(steps, tracecheck.Step{Action: "Session#0.JobFinishesNaturally", State: state("alive", "exited", "closed", false, false, true)})
		}
	}
	return steps
}

func init() { historyProjections["background_job_survives_restart"] = bjsrHistory }

func TestBackgroundJobSurvivesRestart(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newBJSRAdapter(t)
	if err := runMBT(t, "background_job_survives_restart", a, bjsrActions, bjsrOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "background_job_survives_restart"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), bjsrHistory)
	}
}

// The run above proves nothing unless a wrongly wired adapter fails it:
// this one reports the job closed right after Init, before anything
// real closed it.
func TestBackgroundJobSurvivesRestartCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newBJSRAdapter(t)
	a.reportClosedEarly = true
	if err := runMBT(t, "background_job_survives_restart", a, bjsrActions, bjsrOptions()); err == nil {
		t.Fatal("a run that reports the job closed right after Init passed; the runner is not checking state")
	}
}
