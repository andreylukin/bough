//go:build !windows

package mbt

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
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

// specs/stop_interrupt.fizz against a real serve: Stop on a turn the
// child has not taken yet (held), on a running turn (SIGINT), and on a
// turn that finished before the SIGINT landed, with the child's exit
// and respawn that every Stop costs.
//
// The spec splits what the real system does in one breath into steps,
// and names server internals no API shows (held, timer, stale, sigint,
// tail). The adapter drives what it can make happen one step at a time
// and keeps the rest as its own record of the spec's fields:
//
//   - Take is the child reading its prompt. The adapter SIGSTOPs the
//     child before every prompt and SIGCONTs it at Take, so "the prompt
//     is written and unread" is a state it can hold and Stop in. A
//     session with no child is given one first through /effort, which
//     reaches it as a "/think" line and starts no turn.
//   - Stop on a running turn is the page's POST, but its SIGINT is "on
//     its way" until ChildSignal: the adapter makes the POST there, so a
//     Finish in between is a real finish that the SIGINT then reaches
//     idle.
//   - Where the server does several steps at once (a held Stop is
//     released the moment the child takes its prompt; a cancel is
//     followed by the bookkeeping done and the exit), the adapter checks
//     the real outcome at the first step, within stopBound, and reports
//     the spec's in-between states from its record until the last step
//     (`ahead`).
//   - Reply has no real counterpart: llm-control has no tool calls, so a
//     reply always ends its turn. `replied` is the adapter's record, and
//     only the browser layer sees a stopped turn that had replied.
//   - An expired hold is not driven: after holdLimit the SIGINT and the
//     unread line reach the child in the same instant on SIGCONT, and
//     which wins is a race (probed: 9 of 10 the child took the line and
//     then hung in unmount with the SIGINT spent, 1 of 10 it cancelled).
//     A walk that takes HoldExpires, or a Finish of a turn the server
//     already cancelled (a held Stop released at Take), follows the
//     spec only from there on (`detached`) and is counted in the log:
//     of the 43 generated paths, 12 and 8.
//
// The generated paths (testdata/stop_interrupt/paths.json) are walked
// first, then the runner's random walks; see walkStopInterruptPaths
// for why the paths carry the coverage.
//
// status, alive, pending and cancelled are read off the server (row,
// live flag, transcript) whenever the real system is at the spec's
// step; stopping and draft are the page's, computed the way app.tsx
// does from the real transcript.

// stopBound is how long a SIGINT may take to end a turn. It sits well
// under holdLimit (20 s): an interrupt that is held when it should have
// gone straight through fails here rather than passing late.
const stopBound = 5 * time.Second

// stopShadow is the spec's Session role, field for field.
type stopShadow struct {
	status                               string
	pending, alive, replied, held, timer bool
	stale, cancelled, tail               bool
	sigint, stopping, draft              string
}

func (s stopShadow) state() map[string]any {
	return map[string]any{
		"status": s.status, "pending": s.pending, "alive": s.alive, "replied": s.replied,
		"held": s.held, "timer": s.timer, "stale": s.stale, "sigint": s.sigint,
		"cancelled": s.cancelled, "tail": s.tail, "stopping": s.stopping, "draft": s.draft,
	}
}

type stopInterruptAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string
	gate gate

	id   string
	ids  []string
	sh   stopShadow
	turn int
	name string // the llm turn the current prompt will take

	pid     int   // the session's child, 0 when it has none
	frozen  bool  // pid is SIGSTOPped
	sent    bool  // a prompt was sent this walk
	sentSeq int64 // the transcript's last seq when it was

	ahead      bool           // the server has run past the spec's step; see above
	detached   bool           // this walk left what the adapter can drive
	detachedBy map[string]int // walks that left, by the step that left
	why        string
	depth      int         // actions this walk has driven
	depths     map[int]int // walks by how many actions they drove

	// finishAsError is TestStopInterruptCatchesWrongAdapter's bug:
	// Finish fails the turn instead of finishing it.
	finishAsError bool
}

func newStopInterruptAdapter(t *testing.T) *stopInterruptAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &stopInterruptAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

// Init is a fresh idle session; Create leaves its child running.
func (a *stopInterruptAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	a.sh = stopShadow{status: "idle", alive: true}
	a.pid, a.frozen, a.sent, a.sentSeq, a.ahead, a.detached = 0, false, false, 0, false, false
	a.gate.reset()
	return nil
}

// Cleanup ends the walk's child, frozen or not, and empties the llm
// queue: a turn this walk queued and never took would otherwise answer
// the next walk's first prompt.
func (a *stopInterruptAdapter) Cleanup() error {
	if a.detached {
		if a.detachedBy == nil {
			a.detachedBy = map[string]int{}
		}
		a.detachedBy[a.why]++
	}
	if a.depths == nil {
		a.depths = map[int]int{}
	}
	a.depths[a.depth]++
	a.depth = 0
	ctx, cancel := actionCtx()
	defer cancel()
	if _, err := a.s.Archive(ctx, a.id); err != nil {
		return err
	}
	a.pid, a.frozen = 0, false
	ents, _ := os.ReadDir(a.dir)
	for _, e := range ents {
		n := e.Name()
		switch {
		case strings.HasSuffix(n, ".json"):
			os.Remove(filepath.Join(a.dir, n))
		case strings.HasSuffix(n, ".taken"):
			if _, err := os.Stat(filepath.Join(a.dir, strings.TrimSuffix(n, ".taken")+".release")); err != nil {
				control.Release(a.t, a.dir, strings.TrimSuffix(n, ".taken"))
			}
		}
	}
	return nil
}

func (a *stopInterruptAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

func (a *stopInterruptAdapter) GetState() (map[string]any, error) {
	st := a.sh.state()
	if a.detached {
		return st, nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, lines, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return nil, err
	}
	st["pending"] = a.sent && !inputAfter(lines, a.sentSeq)
	if !a.ahead {
		st["status"] = string(row.Status)
		st["alive"] = row.Live
		st["cancelled"] = lastCloseCancelled(lines)
	}
	return st, nil
}

// wait polls the row and transcript until ok holds, for at most d.
func (a *stopInterruptAdapter) wait(what string, d time.Duration, ok func(serve.Row, []serve.Line) bool) (serve.Row, error) {
	deadline := time.Now().Add(d)
	var row serve.Row
	var lines []serve.Line
	var err error
	for {
		ctx, cancel := actionCtx()
		row, lines, err = a.s.GetSession(ctx, a.id)
		cancel()
		if err == nil && ok(row, lines) {
			return row, nil
		}
		if time.Now().After(deadline) {
			return row, fmt.Errorf("waiting %s for %s: row %s live=%v, transcript %v (err %v)", d, what, row.Status, row.Live, lineKinds(lines), err)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// pass is the gate, counting how deep each walk gets: the runner picks
// disabled actions too, so a spec with many actions is walked shallowly
// unless the run is long.
func (a *stopInterruptAdapter) pass(enabled bool) bool {
	ok := a.gate.pass(enabled)
	if ok {
		a.depth++
	}
	return ok
}

// detach leaves the real system: the rest of the walk is the spec's.
func (a *stopInterruptAdapter) detach(why string) {
	if !a.detached {
		a.detached, a.why = true, why
	}
}

func (a *stopInterruptAdapter) Prompt() error {
	sh := &a.sh
	if !a.pass(!sh.pending && sh.status != "running" && sh.sigint == "" && !sh.tail && sh.stopping == "") {
		return nil
	}
	if !a.detached {
		if err := a.promptReal(); err != nil {
			return err
		}
	}
	if !sh.alive {
		sh.stale = false
	}
	sh.alive, sh.pending, sh.replied, sh.draft = true, true, false, ""
	return nil
}

func (a *stopInterruptAdapter) promptReal() error {
	ctx, cancel := actionCtx()
	defer cancel()
	if !a.sh.alive {
		// Supervisor.ensure spawns the child on any line; a "/think" line
		// is not a prompt, so it leaves nothing unread and no turn.
		if err := a.s.Effort(ctx, a.id, "medium"); err != nil {
			return fmt.Errorf("spawn a child: %w", err)
		}
	}
	if a.pid == 0 {
		pid, err := a.childPID()
		if err != nil {
			return err
		}
		a.pid = pid
	}
	if err := syscall.Kill(a.pid, syscall.SIGSTOP); err != nil {
		return fmt.Errorf("pause child %d: %w", a.pid, err)
	}
	a.frozen = true
	a.turn++
	a.name = fmt.Sprintf("s%04d", a.turn)
	control.Queue(a.t, a.dir, a.name, control.Turn{Mode: "block", Text: "finished " + a.name})
	_, lines, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return err
	}
	a.sent, a.sentSeq = true, lastSeq(lines)
	return a.s.Prompt(ctx, a.id, "turn "+a.name)
}

// childPID finds the session's child among serve's: the walk's is the
// only one, since Cleanup archives (kills) every earlier walk's.
func (a *stopInterruptAdapter) childPID() (int, error) {
	deadline := time.Now().Add(stopBound)
	for {
		out, _ := exec.Command("pgrep", "-P", strconv.Itoa(a.s.PID()), "-f", "--", "--headless").Output()
		f := strings.Fields(string(out))
		if len(f) == 1 {
			return strconv.Atoi(f[0])
		}
		if time.Now().After(deadline) {
			return 0, fmt.Errorf("serve %d has %d headless children, want 1", a.s.PID(), len(f))
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// Take lets the child read its prompt.
func (a *stopInterruptAdapter) Take() error {
	sh := &a.sh
	if !a.pass(sh.pending && sh.alive) {
		return nil
	}
	if !a.detached {
		if sh.sigint != "" || !a.frozen {
			return fmt.Errorf("take: sigint %q frozen %v: a state the adapter does not drive", sh.sigint, a.frozen)
		}
		if err := syscall.Kill(a.pid, syscall.SIGCONT); err != nil {
			return fmt.Errorf("resume child %d: %w", a.pid, err)
		}
		a.frozen = false
		if sh.held {
			// The held Stop goes out as the child takes the prompt: the
			// turn opens and is cancelled, it does not run, and the child
			// exits. That is Take, ChildSignal and Wrapup in one.
			if _, err := a.wait("the held stop to cancel the taken prompt", stopBound, func(r serve.Row, l []serve.Line) bool {
				return inputAfter(l, a.sentSeq) && lastCloseCancelled(l) && r.Status == serve.StatusStopped && !r.Live
			}); err != nil {
				return err
			}
			a.pid = 0
			a.ahead = true
		} else {
			if _, err := a.wait("the prompt to become a running turn", actionTimeout, func(r serve.Row, l []serve.Line) bool {
				return inputAfter(l, a.sentSeq) && r.Status == serve.StatusRunning
			}); err != nil {
				return err
			}
			control.WaitTaken(a.t, a.dir, a.name, actionTimeout)
		}
	}
	sh.pending, sh.status, sh.cancelled, sh.replied = false, "running", false, false
	if sh.held {
		sh.held, sh.sigint = false, "sent"
	}
	if sh.timer {
		sh.timer, sh.stale = false, true
	}
	return nil
}

// Reply is the adapter's record only (see the file comment).
func (a *stopInterruptAdapter) Reply() error {
	sh := &a.sh
	if !a.pass(sh.status == "running" && sh.alive && !sh.tail && !sh.replied) {
		return nil
	}
	// Nothing real to do, so a turn the server already cancelled
	// (ahead) does not end the walk here; a Finish after it does.
	sh.replied = true
	return nil
}

func (a *stopInterruptAdapter) Finish() error {
	sh := &a.sh
	if !a.pass(sh.status == "running" && sh.alive && !sh.tail) {
		return nil
	}
	if a.ahead {
		a.detach("Finish after the server cancelled")
	}
	if !a.detached {
		if a.finishAsError {
			control.ReleaseWith(a.t, a.dir, a.name, control.Turn{Mode: "error", Error: "model says no"})
		} else {
			control.Release(a.t, a.dir, a.name)
		}
		if _, err := a.wait("the turn to finish", actionTimeout, func(r serve.Row, _ []serve.Line) bool { return r.Status != serve.StatusRunning }); err != nil {
			return err
		}
	}
	sh.status = "done"
	return nil
}

// Stop is the composer's Stop (Esc or the button).
func (a *stopInterruptAdapter) Stop() error {
	sh := &a.sh
	if !a.pass((sh.pending || sh.status == "running") && sh.stopping != "stopping") {
		return nil
	}
	sh.stopping = "stopping"
	switch {
	case !sh.alive:
		if !a.detached {
			ctx, cancel := actionCtx()
			defer cancel()
			var e *servetest.APIError
			if err := a.s.Interrupt(ctx, a.id); !errors.As(err, &e) || e.Status != http.StatusNotFound {
				return fmt.Errorf("stop with no child: got %v, want 404", err)
			}
		}
		sh.stopping = "failed"
	case sh.pending:
		// The child is paused with its prompt unread: the server must
		// hold this interrupt, which Take checks.
		if !a.detached {
			ctx, cancel := actionCtx()
			defer cancel()
			if err := a.s.Interrupt(ctx, a.id); err != nil {
				return fmt.Errorf("stop an unread prompt: %w", err)
			}
		}
		sh.held, sh.timer = true, true
	default:
		sh.sigint = "sent" // POSTed at ChildSignal
	}
	return nil
}

// HoldExpires is not driven (see the file comment).
func (a *stopInterruptAdapter) HoldExpires() error {
	sh := &a.sh
	if !a.pass(sh.timer) {
		return nil
	}
	a.detach("HoldExpires")
	sh.timer, sh.held, sh.sigint = false, false, "expired"
	return nil
}

// StaleFires: a timer of a released hold does nothing, so there is
// nothing to drive or see.
func (a *stopInterruptAdapter) StaleFires() error {
	if a.pass(a.sh.stale) {
		a.sh.stale = false
	}
	return nil
}

// Resend is only reachable after an expired hold, which detaches.
func (a *stopInterruptAdapter) Resend() error {
	sh := &a.sh
	if !a.pass(sh.pending && !sh.alive) {
		return nil
	}
	if !a.detached {
		return fmt.Errorf("resend: reached without an expired hold")
	}
	sh.alive = true
	return nil
}

func (a *stopInterruptAdapter) ChildSignal() error {
	sh := &a.sh
	if !a.pass(sh.sigint != "" && sh.alive && !sh.tail) {
		return nil
	}
	if !a.detached && !a.ahead {
		ctx, cancel := actionCtx()
		defer cancel()
		if err := a.s.Interrupt(ctx, a.id); err != nil {
			return fmt.Errorf("stop: %w", err)
		}
		if sh.status == "running" {
			// Cancelled, then the bookkeeping done, then the exit:
			// ChildSignal and Wrapup in one.
			if _, err := a.wait("the stop to cancel the running turn", stopBound, func(r serve.Row, l []serve.Line) bool {
				return lastCloseCancelled(l) && r.Status == serve.StatusStopped && !r.Live
			}); err != nil {
				return err
			}
			a.ahead = true
		} else {
			if _, err := a.wait("the idle child to exit", stopBound, func(r serve.Row, _ []serve.Line) bool { return !r.Live }); err != nil {
				return err
			}
		}
		a.pid = 0
	}
	sh.sigint = ""
	if sh.status == "running" {
		sh.status, sh.cancelled, sh.tail = "stopped", true, true
	} else {
		sh.alive, sh.stale = false, false
	}
	return nil
}

func (a *stopInterruptAdapter) Wrapup() error {
	sh := &a.sh
	if !a.pass(sh.tail) {
		return nil
	}
	// The server did this with ChildSignal; from here its state is the
	// spec's again, which GetState reads.
	a.ahead = false
	sh.tail, sh.alive, sh.stale = false, false, false
	return nil
}

// Settle is the page once nothing is live: stopping clears, and a turn
// stopped before any reply puts its prompt back (stoppedPrompt, read
// off the real transcript).
func (a *stopInterruptAdapter) Settle() error {
	sh := &a.sh
	if !a.pass(!sh.pending && sh.status != "running" && sh.stopping != "") {
		return nil
	}
	sh.stopping = ""
	restore := sh.status == "stopped"
	if !a.detached {
		ctx, cancel := actionCtx()
		defer cancel()
		_, lines, err := a.s.GetSession(ctx, a.id)
		if err != nil {
			return err
		}
		restore = stoppedPrompt(lines) != ""
	}
	if restore && !sh.replied && sh.draft == "" {
		sh.draft = "restored"
	}
	return nil
}

var stopInterruptActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Prompt":      action((*stopInterruptAdapter).Prompt),
	"Take":        action((*stopInterruptAdapter).Take),
	"Reply":       action((*stopInterruptAdapter).Reply),
	"Finish":      action((*stopInterruptAdapter).Finish),
	"Stop":        action((*stopInterruptAdapter).Stop),
	"HoldExpires": action((*stopInterruptAdapter).HoldExpires),
	"StaleFires":  action((*stopInterruptAdapter).StaleFires),
	"Resend":      action((*stopInterruptAdapter).Resend),
	"ChildSignal": action((*stopInterruptAdapter).ChildSignal),
	"Wrapup":      action((*stopInterruptAdapter).Wrapup),
	"Settle":      action((*stopInterruptAdapter).Settle),
}}

func stopInterruptOptions() map[string]any {
	return map[string]any{"max-seq-runs": 60, "max-actions": 8, "max-parallel-runs": 0}
}

// inputAfter says whether a prompt (not a steer) was taken after seq.
func inputAfter(lines []serve.Line, seq int64) bool {
	for _, l := range lines {
		if l.Kind == "input" && l.Seq > seq && l.Data["steer"] == nil {
			return true
		}
	}
	return false
}

func lastSeq(lines []serve.Line) int64 {
	if len(lines) == 0 {
		return 0
	}
	return lines[len(lines)-1].Seq
}

// lastCloseCancelled mirrors serve.StatusOf's scan: the last turn is
// closed, and by a cancel (the done after it is bookkeeping).
func lastCloseCancelled(lines []serve.Line) bool {
	open, last := false, ""
	for _, l := range lines {
		switch l.Kind {
		case "input":
			open, last = true, ""
		case "done", "cancelled":
			if l.Kind == "done" && !open && last == "cancelled" {
				continue
			}
			open, last = false, l.Kind
		}
	}
	return !open && last == "cancelled"
}

func lineKinds(lines []serve.Line) []string {
	var k []string
	for _, l := range lines {
		k = append(k, l.Kind)
	}
	return k
}

// stopInterruptHistory reads the abstract trace off a transcript. The
// page's Stop and Settle and the server's hold leave nothing there, so
// they are inferred: a cancel was a Stop and its ChildSignal, the done
// after it the Wrapup, and the next prompt's page had settled first. A
// Stop that reached an idle child, or a held one, reads as a path that
// never pressed it, which the graph also has.
func stopInterruptHistory(entries []history.Entry) []tracecheck.Step {
	status := func(s string) map[string]any { return map[string]any{"Session#0.status": s} }
	step := func(action string, state map[string]any) tracecheck.Step {
		return tracecheck.Step{Action: "Session#0." + action, State: state}
	}
	steps := []tracecheck.Step{{Action: "Init", State: status("idle")}}
	open, last := false, ""
	for _, e := range entries {
		switch e.Kind {
		case "input":
			if e.Data["steer"] != nil {
				continue
			}
			if strings.HasPrefix(last, "cancelled") {
				steps = append(steps, step("Settle", nil))
			}
			steps = append(steps, step("Prompt", nil), step("Take", status("running")))
			open, last = true, ""
		case "cancelled":
			if open {
				steps = append(steps, step("Stop", nil), step("ChildSignal", map[string]any{"Session#0.status": "stopped", "Session#0.cancelled": true}))
				open, last = false, "cancelled"
			}
		case "done":
			switch {
			case open:
				steps = append(steps, step("Finish", status("done")))
				open, last = false, "done"
			case last == "cancelled":
				steps = append(steps, step("Wrapup", map[string]any{"Session#0.status": "stopped", "Session#0.alive": false}))
				last = "cancelled-done"
			}
		}
	}
	return steps
}

func init() { historyProjections["stop_interrupt"] = stopInterruptHistory }

// runStopInterrupt is the runner's random walks over the same adapter.
func runStopInterrupt(t *testing.T, a *stopInterruptAdapter) error {
	a.detachedBy, a.depths = nil, nil
	err := runMBT(t, "stop_interrupt", a, stopInterruptActions, stopInterruptOptions())
	t.Logf("stop_interrupt: runner walks by how many actions they drove: %v; left what the adapter drives: %v", a.depths, a.detachedBy)
	return err
}

// stopInterruptPath is one path of testdata/stop_interrupt/paths.json:
// the generator's cover of every transition, first step Init.
type stopInterruptPath struct {
	Trace []struct {
		Action string         `json:"action"`
		State  map[string]any `json:"state"`
	} `json:"trace"`
}

// walkStopInterruptPaths drives every generated path through the
// adapter and compares its state with the path's after every step. The
// runner's random walks cannot reach most of this spec: it picks among
// all eleven actions, disabled ones included, and a walk ends at its
// first disabled pick, so a four-step prefix like Prompt, Take, Stop,
// ChildSignal is about one walk in a thousand (observed: 52 of 60 walks
// drove nothing, none more than two). The paths cover every transition.
func walkStopInterruptPaths(t *testing.T, a *stopInterruptAdapter) (mismatches []string) {
	t.Helper()
	b, err := pathsJSON("stop_interrupt")
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []stopInterruptPath `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		t.Fatal(err)
	}
	a.detachedBy = nil
	for pi, p := range doc.Paths {
		var names []string
		for si, step := range p.Trace {
			name := strings.TrimPrefix(step.Action, "Session#0.")
			names = append(names, name)
			var err error
			if si == 0 {
				err = a.Init()
			} else {
				f, ok := stopInterruptActions["Session"][name]
				if !ok {
					t.Fatalf("path %d: no adapter action %q", pi, name)
				}
				_, err = f(a, nil)
			}
			if err == nil {
				err = a.compare(step.State)
			}
			if err != nil {
				mismatches = append(mismatches, fmt.Sprintf("path %d step %d (%s): %v", pi, si, strings.Join(names, " "), err))
				break
			}
		}
		if err := a.Cleanup(); err != nil {
			t.Fatalf("path %d: cleanup: %v", pi, err)
		}
	}
	t.Logf("stop_interrupt: %d paths, left what the adapter drives: %v", len(doc.Paths), a.detachedBy)
	return mismatches
}

// compare checks the adapter's state against a path's qualified state.
func (a *stopInterruptAdapter) compare(want map[string]any) error {
	got, err := a.GetState()
	if err != nil {
		return err
	}
	var diff []string
	for k, v := range want {
		f, ok := strings.CutPrefix(k, "Session#0.")
		if !ok {
			continue
		}
		if fmt.Sprint(got[f]) != fmt.Sprint(v) {
			diff = append(diff, fmt.Sprintf("%s: spec %v, got %v", f, v, got[f]))
		}
	}
	if len(diff) > 0 {
		sort.Strings(diff)
		return fmt.Errorf("state: %s", strings.Join(diff, "; "))
	}
	return nil
}

func TestStopInterrupt(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newStopInterruptAdapter(t)
	for _, m := range walkStopInterruptPaths(t, a) {
		t.Error(m)
	}
	if err := runStopInterrupt(t, a); err != nil {
		t.Errorf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "stop_interrupt"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), stopInterruptHistory)
	}
}

// A Finish wired to fail the turn must fail the paths: the row says
// error where the spec says done. The runner's walks reach Finish too
// rarely (Prompt, Take, Finish in a row) to be the check here.
func TestStopInterruptCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newStopInterruptAdapter(t)
	a.finishAsError = true
	if len(walkStopInterruptPaths(t, a)) == 0 {
		t.Fatal("paths whose Finish fails the turn passed; the adapter is not checking state")
	}
}
