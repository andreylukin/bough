//go:build !windows

package mbt

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/orphan_jobs_on_reap.fizz against a real serve: a background
// job's typed history entries as its child dies and the session's file
// passes to whatever process holds it next
// (go/internal/serve/orphan_jobs.go endOrphanJobs, called from
// supervisor.go's reap goroutine after cmd.Wait returns).
//
// The session under test (ojrAdapter.id) is spawned as an agent of a
// throwaway parent (ojrAdapter.parent): archiving the parent with
// stopChildren kills the agent through Supervisor.EndChild, which ends
// a live child (Kill, a SIGKILL) without archiving it, so the same
// session id can be re-adopted afterward the way a new bough process
// would. That archive call blocks in killChild's `<-ch.done>` until the
// reap goroutine has run, so BOUGH_SERVE_TEST_HOLD's "reap" point (the
// window between the reap's "leased" and "held" checks, supervisor.go's
// two lease reads around endOrphanJobs) is held open for the walk to
// act in.
//
// NaturalFinish and LeaseStolen describe interleavings this harness
// cannot force for real: a SIGKILL gives the job's own goroutine no
// chance to flush (there is no window between the kill and the
// process's death for it to write in), and nothing but a test-only
// hook can make the lease move to a new child before the reap's own
// "held" check runs. Once a walk takes either, the adapter stops
// driving the real system (adapts only its own record from there,
// `real` false) and reports a state the graph still allows, the way
// background_bash_jobs's adapter detaches at an interleaving it cannot
// steer into. The reachable part of the graph — a plain SIGKILL orphan,
// scanned and closed, or naturally flushed first, then re-adopted — is
// driven and checked against the real serve every time.

const ojrConfig = controlConfig

// ojrScript is the background job: a loop that runs until its dir gets
// an "exit" file, so ChildADies can decide whether the job outlives the
// kill (open when the child dies) or has already finished
// (finishFlushed, before ChildADies real work even runs — see
// NaturalFinish).
func ojrScript(dir string) string {
	return fmt.Sprintf(`while :; do [ -e %q ] && exit 0; sleep 0.05; done`, filepath.Join(dir, "exit"))
}

// ojrShadow is the spec's Session role, field for field.
type ojrShadow struct {
	open, dying, leaseStolen, reapScanned, reapWritten bool
	scanOpen, finishFlushed, adopted, gone             bool
}

func (s ojrShadow) state() map[string]any {
	return map[string]any{
		"open": s.open, "dying": s.dying, "leaseStolen": s.leaseStolen,
		"reapScanned": s.reapScanned, "reapWritten": s.reapWritten, "scanOpen": s.scanOpen,
		"finishFlushed": s.finishFlushed, "adopted": s.adopted, "gone": s.gone,
	}
}

type ojrAdapter struct {
	t     *testing.T
	s     *servetest.Server
	dir   string // llm-control's queue
	holds string // BOUGH_SERVE_TEST_HOLD
	gate  gate
	sh    ojrShadow

	walk    int
	parent  string
	id      string
	ids     []string
	jobDir  string
	seq     int
	held    string // the llm-control turn in flight, "" when none
	archErr chan error

	detached bool // NaturalFinish or LeaseStolen: shadow bookkeeping only from here on
	why      string

	// noVerify is TestOrphanJobsOnReapCatchesWrongAdapter's bug: the
	// adapter believes a job is open when the real server has already
	// closed it.
	noVerify bool
}

func newOJRAdapter(t *testing.T) *ojrAdapter {
	holds := t.TempDir()
	s := servetest.Start(t, servetest.Options{Config: ojrConfig, Env: []string{"BOUGH_SERVE_TEST_HOLD=" + holds}})
	return &ojrAdapter{t: t, s: s, dir: control.Dir(s.Home), holds: holds}
}

func (a *ojrAdapter) holdPath(point string) string { return filepath.Join(a.holds, point) }

func (a *ojrAdapter) setHold(point string) error { return os.WriteFile(a.holdPath(point), nil, 0o644) }

func (a *ojrAdapter) clearHold(point string) error {
	if err := os.Remove(a.holdPath(point)); err != nil && !os.IsNotExist(err) {
		return err
	}
	return nil
}

func (a *ojrAdapter) holding(point string) bool {
	_, err := os.Stat(a.holdPath(point))
	return err == nil
}

func (a *ojrAdapter) real() bool { return !a.detached }

func (a *ojrAdapter) detach(why string) {
	if !a.detached {
		a.detached, a.why = true, why
	}
}

func (a *ojrAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	a.walk++
	prow, err := a.s.CreateSession(ctx, a.s.Dir(a.t, fmt.Sprintf("parent%04d", a.walk)), "")
	if err != nil {
		return err
	}
	a.parent = prow.ID
	row, queued, err := a.s.CreateAgent(ctx, a.parent, "idle", 10, 10)
	if err != nil {
		return err
	}
	if queued {
		return fmt.Errorf("Init: the agent queued instead of starting")
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	// CreateAgent answers as soon as the row is pending: its history
	// file (and so GetSession/Prompt) may not exist yet.
	deadline := time.Now().Add(actionTimeout)
	for {
		if _, _, err := a.s.GetSession(ctx, a.id); err == nil {
			break
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("Init: the agent's session never became visible")
		}
		time.Sleep(10 * time.Millisecond)
	}
	a.jobDir = filepath.Join(a.s.Root, fmt.Sprintf("job%04d", a.walk))
	if err := os.MkdirAll(a.jobDir, 0o755); err != nil {
		return err
	}
	a.sh = ojrShadow{}
	a.held, a.archErr = "", nil
	a.detached, a.why = false, ""
	a.gate.reset()
	return nil
}

// Cleanup ends whatever the walk left running: the parent's archive
// (stopping the child too) if the child is still leased, and llm-control's
// queue emptied so an un-taken turn does not answer the next walk's first
// request.
func (a *ojrAdapter) Cleanup() error {
	a.clearHold("reap")
	if a.archErr != nil {
		select {
		case <-a.archErr:
		case <-time.After(actionTimeout):
		}
		a.archErr = nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	var out any
	_ = a.s.API(ctx, "POST", "/api/sessions/"+a.parent+"/archive", map[string]any{"stopChildren": true}, &out)
	if a.held != "" {
		control.Release(a.t, a.dir, a.held)
		a.held = ""
	}
	ents, _ := os.ReadDir(a.dir)
	for _, e := range ents {
		if n, ok := cutSuffix(e.Name(), ".json"); ok {
			os.Remove(filepath.Join(a.dir, n+".json"))
		}
	}
	return nil
}

func cutSuffix(s, suf string) (string, bool) {
	if len(s) >= len(suf) && s[len(s)-len(suf):] == suf {
		return s[:len(s)-len(suf)], true
	}
	return "", false
}

func (a *ojrAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

func (a *ojrAdapter) GetState() (map[string]any, error) {
	if a.noVerify {
		sh := a.sh
		sh.open = false // the wrong adapter believes every job it started is closed
		return sh.state(), nil
	}
	return a.sh.state(), nil
}

// entries reads the session's history file straight off disk: the typed
// job entries orphan_jobs.go reads and writes are not in the API's
// transcript.
func (a *ojrAdapter) entries() ([]history.Entry, error) {
	return history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
}

// jobOpen mirrors endOrphanJobs' own bookkeeping: whether the history
// file, read now, lists an id with a "started" typed entry and no
// matching "finished" one.
func jobOpen(entries []history.Entry) bool {
	open := map[float64]bool{}
	for _, e := range entries {
		if e.Kind != "job" {
			continue
		}
		event, ok := e.Data["event"].(string)
		if !ok {
			continue
		}
		id, _ := e.Data["id"].(float64)
		switch event {
		case "started":
			open[id] = true
		case "finished":
			delete(open, id)
		}
	}
	return len(open) > 0
}

func (a *ojrAdapter) waitJob(what string, want bool) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		ents, err := a.entries()
		if err == nil && jobOpen(ents) == want {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting %s: job open=%v after %s", what, !want, actionTimeout)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func (a *ojrAdapter) queue(turn control.Turn) string {
	a.seq++
	name := fmt.Sprintf("t%06d", a.seq)
	control.Queue(a.t, a.dir, name, turn)
	return name
}

// promptRetry sends the session's first (or re-adopted) prompt,
// retrying a 404 until the row is claimable: CreateAgent (or a lease
// freed by ChildAGone) answers before the child that will own it is
// necessarily ready to be sent to.
func (a *ojrAdapter) promptRetry(text string) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		ctx, cancel := actionCtx()
		err := a.s.Prompt(ctx, a.id, text)
		cancel()
		if err == nil {
			return nil
		}
		if time.Now().After(deadline) {
			return err
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func (a *ojrAdapter) taken(name string) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		if _, err := os.Stat(filepath.Join(a.dir, name+".taken")); err == nil {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("llm-control turn %s not taken after %s", name, actionTimeout)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

// startJob opens a turn on the session (Prompt if none is open yet, a
// fresh one otherwise) and has the model answer it with a background
// bash call running the loop script, then waits for its typed
// "started" entry.
func (a *ojrAdapter) startJob(fresh bool) error {
	name := a.queue(control.Turn{Mode: "block", Text: "reply"})
	if fresh {
		// A just-created (or just-adopted) session's child may still be
		// mid-claim: Prompt 404s ("unknown session") until it is.
		if err := a.promptRetry("start a job"); err != nil {
			return err
		}
	} else {
		control.Release(a.t, a.dir, a.held)
	}
	if err := a.taken(name); err != nil {
		return err
	}
	a.held = name
	args := map[string]any{"command": ojrScript(a.jobDir), "background": true, "timeout": "300s"}
	next := a.queue(control.Turn{Mode: "block", Text: "reply"})
	control.ReleaseWith(a.t, a.dir, a.held, control.Turn{Mode: "call", Tool: "bash", Args: args})
	a.held = next
	if err := a.taken(next); err != nil {
		return err
	}
	return a.waitJob("the job's started entry", true)
}

// ---- the spec's actions ----

func (a *ojrAdapter) StartJob() error {
	sh := &a.sh
	if !a.gate.pass(!sh.dying && !sh.open) {
		return nil
	}
	if a.real() {
		if err := a.startJob(a.held == ""); err != nil {
			return err
		}
	}
	sh.open = true
	return nil
}

// ChildADies kills the session's child with a SIGKILL, through the
// throwaway parent's archive (stopChildren), which reaches it via
// Supervisor.EndChild and never marks it archived. The "reap" hold is
// set first, so the reap goroutine parks between its "leased" and
// "held" checks (the window BeginReap/FinishReapWrite/LeaseStolen are
// about) until the walk releases it.
func (a *ojrAdapter) ChildADies() error {
	sh := &a.sh
	if !a.gate.pass(!sh.dying) {
		return nil
	}
	if a.real() {
		if err := a.setHold("reap"); err != nil {
			return err
		}
		a.archErr = make(chan error, 1)
		go func(parent string) {
			ctx, cancel := context.WithTimeout(context.Background(), 2*actionTimeout)
			defer cancel()
			var out any
			a.archErr <- a.s.API(ctx, "POST", "/api/sessions/"+parent+"/archive", map[string]any{"stopChildren": true}, &out)
		}(a.parent)
		deadline := time.Now().Add(actionTimeout)
		for !fileExists(a.holdPath("reap.at")) {
			if time.Now().After(deadline) {
				return fmt.Errorf("ChildADies: the reap never reached its hold")
			}
			time.Sleep(10 * time.Millisecond)
		}
	}
	sh.dying = true
	return nil
}

func fileExists(p string) bool {
	_, err := os.Stat(p)
	return err == nil
}

// NaturalFinish cannot be forced for real: the SIGKILL that makes
// ChildADies real gives the job's own goroutine no window to flush a
// "finished" entry before the process is gone. A walk that takes it
// detaches and keeps only its own record from here on.
func (a *ojrAdapter) NaturalFinish() error {
	sh := &a.sh
	if !a.gate.pass(sh.dying && sh.open && !sh.finishFlushed) {
		return nil
	}
	a.detach("NaturalFinish: no window to flush a job's own finish under a SIGKILL")
	sh.finishFlushed = true
	sh.open = false
	return nil
}

// LeaseStolen cannot be forced for real: nothing but a test-only hook
// (not wired to this flow) can move a session's lease to a new child
// before the reap's own "held" check runs. A walk that takes it
// detaches, like NaturalFinish.
func (a *ojrAdapter) LeaseStolen() error {
	sh := &a.sh
	if !a.gate.pass(sh.dying && !sh.leaseStolen && !sh.reapScanned) {
		return nil
	}
	a.detach("LeaseStolen: no lever to move the lease before the reap's held check")
	sh.leaseStolen = true
	return nil
}

// BeginReap is supervisor.go's "held" check, still paused behind the
// "reap" hold: nothing more to observe on the real system until the
// hold is released (FinishReapWrite), so this step is bookkeeping —
// exactly the snapshot the real check is about to take.
func (a *ojrAdapter) BeginReap() error {
	sh := &a.sh
	if !a.gate.pass(sh.dying && !sh.reapScanned) {
		return nil
	}
	sh.reapScanned = true
	if sh.leaseStolen {
		sh.reapWritten = true
	} else {
		sh.scanOpen = sh.open
	}
	return nil
}

// FinishReapWrite releases the "reap" hold, letting endOrphanJobs run
// (or not, on a stolen lease — but BeginReap already closed that
// action's gate by setting reapWritten there), and waits for the
// child's archive to return, then checks the job's typed entries
// against what the scan should have done.
func (a *ojrAdapter) FinishReapWrite() error {
	sh := &a.sh
	if !a.gate.pass(sh.reapScanned && !sh.reapWritten) {
		return nil
	}
	if a.real() {
		if err := a.releaseReap(); err != nil {
			return err
		}
		want := !sh.scanOpen // scanOpen true: endOrphanJobs must have closed it
		if sh.scanOpen {
			if err := a.waitJob("endOrphanJobs to close the orphaned job", false); err != nil {
				return err
			}
		}
		ents, err := a.entries()
		if err != nil {
			return err
		}
		if got := jobOpen(ents); !a.noVerify && got == sh.scanOpen && sh.scanOpen {
			return fmt.Errorf("FinishReapWrite: job still open after the reap scanned it open, want %v", want)
		}
	}
	sh.reapWritten = true
	if sh.scanOpen {
		sh.open = false
	}
	return nil
}

// releaseReap clears the "reap" hold (if still set) and waits for the
// archive call ChildADies started to return.
func (a *ojrAdapter) releaseReap() error {
	if a.holding("reap") {
		if err := a.clearHold("reap"); err != nil {
			return err
		}
	}
	if a.archErr == nil {
		return nil
	}
	select {
	case err := <-a.archErr:
		a.archErr = nil
		return err
	case <-time.After(actionTimeout):
		return fmt.Errorf("releaseReap: the archive call did not return")
	}
}

// ChildAGone is the old child's own s.drop, which supervisor.go's reap
// runs right after endOrphanJobs (or, on a stolen lease, never — the
// lease already moved). Either way, by the time reapWritten is true the
// drop has happened: this step confirms it (releasing the "reap" hold
// first, on the stolen branch, where FinishReapWrite never touched the
// real system).
func (a *ojrAdapter) ChildAGone() error {
	sh := &a.sh
	if !a.gate.pass(sh.dying && sh.reapWritten && !sh.gone) {
		return nil
	}
	if a.real() {
		if err := a.releaseReap(); err != nil {
			return err
		}
		if _, err := waitRow(a.s, a.id, "the old child's lease dropped", func(r serve.Row) bool { return !r.Live }); err != nil {
			return err
		}
	}
	sh.gone = true
	return nil
}

// ChildBAdopts sends the session a fresh prompt: with no live lease, Send
// spawns a new child under the same id (job ids restart at 1 there), and
// the model answers it with a background bash call the way StartJob's
// does — the spec's "adopted" and "open" both flip at once.
func (a *ojrAdapter) ChildBAdopts() error {
	sh := &a.sh
	if !a.gate.pass(sh.dying && !sh.adopted && (sh.gone || sh.leaseStolen)) {
		return nil
	}
	if a.real() {
		a.held = ""
		if err := a.startJob(true); err != nil {
			return err
		}
	}
	sh.adopted = true
	sh.open = true
	return nil
}

var ojrActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"StartJob":        action((*ojrAdapter).StartJob),
	"ChildADies":      action((*ojrAdapter).ChildADies),
	"NaturalFinish":   action((*ojrAdapter).NaturalFinish),
	"LeaseStolen":     action((*ojrAdapter).LeaseStolen),
	"BeginReap":       action((*ojrAdapter).BeginReap),
	"FinishReapWrite": action((*ojrAdapter).FinishReapWrite),
	"ChildBAdopts":    action((*ojrAdapter).ChildBAdopts),
	"ChildAGone":      action((*ojrAdapter).ChildAGone),
}, "": {
	// The spec turns deadlock detection off, so fizz links a state with
	// nothing enabled to itself as a role-less "end" (see
	// background_bash_jobs_test.go's bbjActions for the same pattern).
	"end": func(m any, _ []fmbt.Arg) (any, error) {
		m.(*ojrAdapter).gate.pass(false)
		return nil, errDisabled
	},
}}

func ojrOptions() map[string]any {
	return map[string]any{"max-seq-runs": 150, "max-actions": 8, "max-parallel-runs": 0}
}

// ojrHistory reads the abstract trace off a transcript: a typed
// "started" entry is StartJob (or ChildBAdopts, once one has already
// been seen), a typed "finished" without "stopped" is NaturalFinish,
// and one with "stopped" is FinishReapWrite's close. The other actions
// (ChildADies, BeginReap, LeaseStolen, ChildAGone) leave no history
// entry of their own.
func ojrHistory(entries []history.Entry) []tracecheck.Step {
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Session#0.open": false}}}
	seen := 0
	step := func(action string, state map[string]any) {
		steps = append(steps, tracecheck.Step{Action: "Session#0." + action, State: state})
	}
	for _, e := range entries {
		if e.Kind != "job" {
			continue
		}
		event, ok := e.Data["event"].(string)
		if !ok {
			continue
		}
		switch event {
		case "started":
			seen++
			if seen == 1 {
				step("StartJob", map[string]any{"Session#0.open": true})
				continue
			}
			// Adoption: the id's previous job already went through the
			// whole reap chain at its "finished" entry, below.
			step("ChildBAdopts", map[string]any{"Session#0.open": true, "Session#0.adopted": true})
		case "finished":
			// Every entry ends the walk's driven child, whether the job
			// was still open (a SIGKILL orphan endOrphanJobs closes,
			// "stopped") or had already flushed on its own (a plain
			// "finished" the harness never really drives — see
			// NaturalFinish in ChildADies's doc comment).
			step("ChildADies", map[string]any{"Session#0.dying": true})
			step("BeginReap", nil)
			if e.Data["stopped"] == true {
				step("FinishReapWrite", map[string]any{"Session#0.open": false})
			} else {
				step("NaturalFinish", map[string]any{"Session#0.open": false})
			}
			step("ChildAGone", map[string]any{"Session#0.gone": true})
		}
	}
	return steps
}

func init() { historyProjections["orphan_jobs_on_reap"] = ojrHistory }

func TestOrphanJobsOnReap(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newOJRAdapter(t)
	if err := runMBT(t, "orphan_jobs_on_reap", a, ojrActions, ojrOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "orphan_jobs_on_reap"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), ojrHistory)
	}
}

// The run above proves nothing unless a server that disagrees with the
// model fails it. This adapter reports every job it started as closed
// at once, so a walk that reaches a state where the graph and the real
// server would actually disagree (FinishReapWrite closing a job the
// scan really did find open) never gets the chance to catch it — the
// real system does close it, so nothing there fails either. The failure
// this bug produces is a state mismatch: GetState says open=false right
// after StartJob, where the graph says true.
func TestOrphanJobsOnReapCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newOJRAdapter(t)
	a.noVerify = true
	if err := runMBT(t, "orphan_jobs_on_reap", a, ojrActions, ojrOptions()); err == nil {
		t.Fatal("a run whose adapter reports every started job as closed passed; the runner is not checking state")
	}
}
