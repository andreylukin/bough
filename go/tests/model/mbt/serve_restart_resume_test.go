//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/serve_restart_resume.fizz against a real serve that the adapter
// stops and starts on one HOME and port: the Room role is the serve, the
// web session a person has open (P), the page's view of both, and one
// background agent (T) queued behind a running cap of 1 that another
// agent (H) holds with a blocked turn.
//
// The agents belong to a separate parent (Q) whose child died at the
// first restart: a finished agent's report is then appended to Q's file
// instead of waking a turn that would take one of P's queued model turns.

type serveRestartResumeAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	q     string // the agents' parent
	p     string // this walk's web session
	h     string // the agent holding the running slot
	task  string // the queued agent
	title string // what the person set on p
	walk  int
	turn  int
	ids   []string

	up     bool
	held   string // p's turn in flight, "" when none
	hHeld  string // h's turn, "" once released or killed
	asking bool   // p's turn is waiting on an ask

	// The page: the build it loaded (first answer, or the last Reload),
	// the build the serve answering now runs, and the banner.
	loaded, serving string
	banner          bool

	// taken counts the actions that ran (the gate let them through):
	// the run logs it, so a green run shows what it walked.
	taken map[string]int
	// dirty is set once a walk's action ran. A walk whose first action
	// was disabled left the room as Init made it, so the next Init
	// reuses it: most random walks end at once, and a restart plus three
	// children per walk would cost the run most of its budget.
	dirty bool

	// askAsFinish is TestServeRestartResumeCatchesWrongAdapter's bug:
	// Ask releases the turn as a plain reply.
	askAsFinish bool
}

func newServeRestartResumeAdapter(t *testing.T) *serveRestartResumeAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &serveRestartResumeAdapter{t: t, s: s, dir: control.Dir(s.Home), up: true, taken: map[string]int{}}
	ctx, cancel := actionCtx()
	defer cancel()
	// Nothing queued: the model answers at once and the turn is done.
	row, err := s.CreateSession(ctx, s.Dir(t, "parent"), "the agents' parent")
	if err != nil {
		t.Fatal(err)
	}
	a.q = row.ID
	if _, err := waitRow(s, a.q, "the parent's turn", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		t.Fatal(err)
	}
	return a
}

// Init restarts the serve (which ends every child a past walk left) and
// sets the room up: p running a held turn, titled; h holding the one
// running slot; the task queued behind it.
func (a *serveRestartResumeAdapter) Init() error {
	a.gate.reset()
	if a.p != "" && !a.dirty {
		return nil
	}
	a.walk++
	a.dirty = false
	a.s.Shutdown()
	if err := a.s.Resume(""); err != nil {
		return err
	}
	a.up, a.held, a.hHeld, a.asking = true, "", "", false
	// A task a past walk left queued starts now: let it finish before
	// anything is queued for the model, so it cannot take that turn.
	if err := a.settle(); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()

	name := a.nextTurn("t")
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "turn "+name)
	if err != nil {
		return err
	}
	a.p, a.held = row.ID, name
	a.ids = append(a.ids, row.ID)
	control.WaitTaken(a.t, a.dir, name, actionTimeout)
	if _, err := waitRow(a.s, a.p, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning }); err != nil {
		return err
	}
	a.title = fmt.Sprintf("room %d", a.walk)
	if err := a.s.Rename(ctx, a.p, a.title); err != nil {
		return err
	}

	hold := a.nextTurn("h")
	control.Queue(a.t, a.dir, hold, control.Turn{Mode: "block", Text: "held " + hold})
	h, queued, err := a.s.CreateAgent(ctx, a.q, "hold the slot "+hold, 1, 1<<20)
	if err != nil {
		return err
	}
	if queued {
		return fmt.Errorf("the slot holder %s was queued: something else holds the running slot", h.ID)
	}
	a.h, a.hHeld = h.ID, hold
	control.WaitTaken(a.t, a.dir, hold, actionTimeout)

	tk, queued, err := a.s.CreateAgent(ctx, a.q, fmt.Sprintf("the queued task of walk %d", a.walk), 1, 1<<20)
	if err != nil {
		return err
	}
	if !queued {
		return fmt.Errorf("task %s started past a running cap of 1", tk.ID)
	}
	a.task = tk.ID

	b, err := a.s.Build(ctx)
	if err != nil {
		return err
	}
	a.loaded, a.serving, a.banner = b, b, false
	return nil
}

func (a *serveRestartResumeAdapter) nextTurn(prefix string) string {
	a.turn++
	return fmt.Sprintf("%s%05d", prefix, a.turn)
}

// Cleanup has nothing to do: the next Init restarts the serve, which
// ends every child, and the test's end stops it.
func (a *serveRestartResumeAdapter) Cleanup() error { return nil }

func (a *serveRestartResumeAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Room", Index: 0}: a}, nil
}

// GetState reads the room: from the API while serve answers, from HOME
// (history, meta.json, the process group) while it does not.
func (a *serveRestartResumeAdapter) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	st := map[string]any{"banner": a.banner, "build": "same"}
	if a.serving != a.loaded {
		st["build"] = "new"
	}
	_, err := a.s.Build(ctx)
	if err == nil {
		st["page"] = "live"
	} else {
		st["page"] = "offline"
	}
	if a.up {
		st["serve"] = "up"
		row, _, err := a.s.GetSession(ctx, a.p)
		if err != nil {
			return nil, err
		}
		st["status"] = string(row.Status)
		st["kids"] = 0
		if row.Live {
			st["kids"] = 1
		}
		st["titled"] = row.Title == a.title
	} else {
		st["serve"] = "down"
		entries, err := history.Read(a.historyPath(a.p))
		if err != nil {
			return nil, err
		}
		status, _ := serve.StatusOf(entries, false)
		st["status"] = string(status)
		st["kids"] = 0
		if a.s.GroupAlive() {
			st["kids"] = 1
		}
		m, err := a.meta()
		if err != nil {
			return nil, err
		}
		st["titled"] = m[a.p].Title == a.title
	}
	task, err := a.taskState(ctx)
	if err != nil {
		return nil, err
	}
	st["task"] = task
	return st, nil
}

func (a *serveRestartResumeAdapter) historyPath(id string) string {
	return filepath.Join(a.s.Home, ".bough", "history", id+".jsonl")
}

func (a *serveRestartResumeAdapter) meta() (map[string]serve.SessionMeta, error) {
	b, err := os.ReadFile(filepath.Join(a.s.Home, ".bough", "serve", "meta.json"))
	if err != nil {
		return nil, err
	}
	var f struct {
		Sessions map[string]serve.SessionMeta `json:"sessions"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		return nil, err
	}
	return f.Sessions, nil
}

// taskState is "queued" while the task waits for a slot (the serve's
// queue while it is up, the persisted task while it is down), "started"
// once its child recorded the task, and "lost" when neither holds: no
// queue will ever start it again.
func (a *serveRestartResumeAdapter) taskState(ctx context.Context) (string, error) {
	if a.up {
		rows, err := a.s.ListSessions(ctx, true)
		if err != nil {
			return "", err
		}
		for _, r := range rows {
			if r.ID == a.task && r.Status == serve.StatusQueued {
				return "queued", nil
			}
		}
	} else {
		m, err := a.meta()
		if err != nil {
			return "", err
		}
		if m[a.task].Task != nil {
			return "queued", nil
		}
	}
	if a.taskRecorded() {
		return "started", nil
	}
	return "lost", nil
}

func (a *serveRestartResumeAdapter) taskRecorded() bool {
	entries, err := history.Read(a.historyPath(a.task))
	if err != nil {
		return false
	}
	for _, e := range entries {
		if e.Kind == "input" {
			return true
		}
	}
	return false
}

// settle waits until no session is running or queued: a task the
// queue starts answers at once (nothing is queued for it) and is done.
func (a *serveRestartResumeAdapter) settle() error {
	ctx, cancel := actionCtx()
	defer cancel()
	var last []serve.Row
	for {
		rows, err := a.s.ListSessions(ctx, true)
		if err == nil {
			busy := false
			for _, r := range rows {
				if r.ID != a.p && (r.Status == serve.StatusRunning || r.Status == serve.StatusQueued) {
					busy = true
				}
				// An agent's file says done a moment before serve reads
				// its done event and frees the running slot: the parent's
				// count is the slot itself.
				if r.ID == a.q && r.Agents != nil && r.Agents.Running+r.Agents.Queued > 0 {
					busy = true
				}
			}
			if !busy {
				return nil
			}
			last = rows
		}
		select {
		case <-ctx.Done():
			b, _ := json.Marshal(last)
			return fmt.Errorf("sessions still busy: %s", b)
		case <-time.After(100 * time.Millisecond):
		}
	}
}

// Stop is SIGTERM to serve, as launchd and `bough update` send it.
func (a *serveRestartResumeAdapter) Stop() error {
	if !a.gate.pass(a.up) {
		return nil
	}
	a.s.Shutdown()
	a.up, a.held, a.hHeld, a.asking = false, "", "", false
	// A child the shutdown missed dies on stdin EOF; give it that long
	// before GetState counts it.
	deadline := time.Now().Add(3 * time.Second)
	for a.s.GroupAlive() && time.Now().Before(deadline) {
		time.Sleep(20 * time.Millisecond)
	}
	return nil
}

func (a *serveRestartResumeAdapter) Start() error {
	if !a.gate.pass(!a.up) {
		return nil
	}
	return a.start("")
}

// StartNew is `bough update`: a new binary at another path, which is a
// new build (buildID is the executable's path and mtime).
func (a *serveRestartResumeAdapter) StartNew() error {
	if !a.gate.pass(!a.up) {
		return nil
	}
	bin := filepath.Join(a.s.Root, fmt.Sprintf("bough-%d-%d", a.walk, a.turn))
	a.turn++
	if err := os.Link(a.s.Bin(), bin); err != nil {
		return err
	}
	return a.start(bin)
}

// start brings serve back and does what the page does when it answers:
// the list poll names the build, and the open session's stream
// reconnects (which must not give it a child).
func (a *serveRestartResumeAdapter) start(bin string) error {
	if err := a.s.Resume(bin); err != nil {
		return err
	}
	a.up = true
	ctx, cancel := actionCtx()
	defer cancel()
	b, err := a.s.Build(ctx)
	if err != nil {
		return err
	}
	a.serving = b
	if b != a.loaded {
		a.banner = true
	}
	stream, err := a.s.Events(ctx, a.p)
	if err != nil {
		return err
	}
	stream.Close()
	return a.settle()
}

// Drain frees the running slot: h's turn ends, and the queue starts the
// task.
func (a *serveRestartResumeAdapter) Drain() error {
	ctx, cancel := actionCtx()
	defer cancel()
	task, err := a.taskState(ctx)
	if err != nil {
		return err
	}
	if !a.gate.pass(a.up && task == "queued") {
		return nil
	}
	if a.hHeld != "" {
		control.Release(a.t, a.dir, a.hHeld)
		a.hHeld = ""
	}
	for !a.taskRecorded() {
		select {
		case <-ctx.Done():
			return errors.New("the queued task never started")
		case <-time.After(50 * time.Millisecond):
		}
	}
	return a.settle()
}

func (a *serveRestartResumeAdapter) Ask() error {
	if !a.gate.pass(a.held != "") {
		return nil
	}
	name := a.held
	a.held, a.asking = "", true
	if a.askAsFinish {
		control.Release(a.t, a.dir, name)
		_, err := waitRow(a.s, a.p, "done", func(r serve.Row) bool { return r.Status == serve.StatusDone })
		return err
	}
	control.ReleaseWith(a.t, a.dir, name, control.Turn{Mode: "call", Tool: "ask", Args: map[string]any{"question": "go on?", "options": []string{"yes", "no"}}})
	_, err := waitRow(a.s, a.p, "needs-you", func(r serve.Row) bool { return r.Status == serve.StatusNeedsYou })
	return err
}

// Answer replies to the ask; the model's next request is a held turn.
func (a *serveRestartResumeAdapter) Answer() error {
	if !a.gate.pass(a.asking) {
		return nil
	}
	name := a.nextTurn("t")
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Answer(ctx, a.p, "yes"); err != nil {
		return err
	}
	a.asking, a.held = false, name
	control.WaitTaken(a.t, a.dir, name, actionTimeout)
	_, err := waitRow(a.s, a.p, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
	return err
}

func (a *serveRestartResumeAdapter) Finish() error {
	if !a.gate.pass(a.held != "") {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	_, err := waitRow(a.s, a.p, "done", func(r serve.Row) bool { return r.Status == serve.StatusDone })
	return err
}

// Send is the composer: after a restart it is what gives p a child
// again (ensure resumes it with -r).
func (a *serveRestartResumeAdapter) Send() error {
	if !a.gate.pass(a.up && a.held == "" && !a.asking) {
		return nil
	}
	name := a.nextTurn("t")
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.p, "turn "+name); err != nil {
		return err
	}
	a.held = name
	control.WaitTaken(a.t, a.dir, name, actionTimeout)
	_, err := waitRow(a.s, a.p, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
	return err
}

// Reload is the person answering the banner: the page now runs the
// build that serves it.
func (a *serveRestartResumeAdapter) Reload() error {
	if a.gate.pass(a.up && a.banner) {
		a.loaded, a.banner = a.serving, false
	}
	return nil
}

// srrAction counts an action that ran: the gate was open before it and
// still is after it.
func srrAction(name string, f func(*serveRestartResumeAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*serveRestartResumeAdapter)
		open := !a.gate.off
		err := f(a)
		if open && !a.gate.off {
			a.taken[name]++
			a.dirty = true
		}
		return nil, err
	}
}

var serveRestartResumeActions = map[string]map[string]fmbt.ActionFunc{"Room": {
	"Stop":     srrAction("Stop", (*serveRestartResumeAdapter).Stop),
	"Start":    srrAction("Start", (*serveRestartResumeAdapter).Start),
	"StartNew": srrAction("StartNew", (*serveRestartResumeAdapter).StartNew),
	"Drain":    srrAction("Drain", (*serveRestartResumeAdapter).Drain),
	"Ask":      srrAction("Ask", (*serveRestartResumeAdapter).Ask),
	"Answer":   srrAction("Answer", (*serveRestartResumeAdapter).Answer),
	"Finish":   srrAction("Finish", (*serveRestartResumeAdapter).Finish),
	"Send":     srrAction("Send", (*serveRestartResumeAdapter).Send),
	"Reload":   srrAction("Reload", (*serveRestartResumeAdapter).Reload),
}}

// A walk that gets past its first action restarts a serve and boots
// two or three children, a few seconds; most walks stop at a disabled
// first action and cost nothing (see dirty).
func serveRestartResumeOptions() map[string]any {
	return map[string]any{"max-seq-runs": 150, "max-actions": 8, "max-parallel-runs": 0}
}

// serveRestartResumeHistory reads p's transcript as the spec's steps.
// The first input is Init's; later inputs are Send. A "cancelled" that
// closes an open turn is the resume after a restart (the dangling turn
// closed by the history mount), so a Stop and Start came before the
// input that follows it. A restart while p was idle leaves nothing in
// its file, and none is needed: only status is checked.
func serveRestartResumeHistory(entries []history.Entry) []tracecheck.Step {
	status := func(s string) map[string]any { return map[string]any{"Room#0.status": s} }
	var steps []tracecheck.Step
	open, lastClose := false, ""
	for _, e := range entries {
		switch e.Kind {
		case "input":
			if len(steps) == 0 {
				steps = append(steps, tracecheck.Step{Action: "Init", State: status("running")})
			} else {
				steps = append(steps, tracecheck.Step{Action: "Room#0.Send", State: status("running")})
			}
			open, lastClose = true, ""
		case "ask":
			steps = append(steps, tracecheck.Step{Action: "Room#0.Ask", State: status("needs-you")})
		case "ask/answer":
			steps = append(steps, tracecheck.Step{Action: "Room#0.Answer", State: status("running")})
		case "cancelled":
			if open {
				steps = append(steps,
					tracecheck.Step{Action: "Room#0.Stop", State: status("interrupted")},
					tracecheck.Step{Action: "Room#0.Start", State: status("interrupted")})
			}
			open, lastClose = false, "cancelled"
		case "done":
			// The loop writes a done after every cancel: bookkeeping.
			if !open && lastClose == "cancelled" {
				continue
			}
			steps = append(steps, tracecheck.Step{Action: "Room#0.Finish", State: status("done")})
			open, lastClose = false, "done"
		}
	}
	return steps
}

func init() { historyProjections["serve_restart_resume"] = serveRestartResumeHistory }

func TestServeRestartResume(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newServeRestartResumeAdapter(t)
	if err := runMBT(t, "serve_restart_resume", a, serveRestartResumeActions, serveRestartResumeOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("actions taken: %v", a.taken)
	g, err := tracecheck.Load(fizzCheck(t, "serve_restart_resume"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), serveRestartResumeHistory)
	}
}

// The runner's random walks mostly stop at a disabled first action, so
// deep paths (a restart onto a new build, then Reload; a resume after a
// dead ask) are rare in them. This walks every generated path in
// testdata/serve_restart_resume/paths.json, the ones the browser spec
// walks, and compares the room with the spec's state at each step.
func TestServeRestartResumePaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	b, err := pathsJSON("serve_restart_resume")
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		t.Fatal(err)
	}
	a := newServeRestartResumeAdapter(t)
	for i, p := range f.Paths {
		a.dirty = true // every path starts from a fresh room
		for j, step := range p.Trace {
			if j == 0 {
				err = a.Init()
			} else {
				name := strings.TrimPrefix(step.Action, "Room#0.")
				_, err = serveRestartResumeActions["Room"][name](a, nil)
				if err == nil && a.gate.off {
					err = errors.New("the adapter found it disabled")
				}
			}
			if err != nil {
				t.Fatalf("path %d step %d (%s): %v", i, j, step.Action, err)
			}
			got, err := a.GetState()
			if err != nil {
				t.Fatalf("path %d step %d (%s): state: %v", i, j, step.Action, err)
			}
			if d := roomDiff(step.State, got); d != "" {
				t.Fatalf("path %d step %d (%s): %s", i, j, step.Action, d)
			}
		}
	}
	t.Logf("%d paths, actions taken: %v", len(f.Paths), a.taken)
	g, err := tracecheck.Load(fizzCheck(t, "serve_restart_resume"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), serveRestartResumeHistory)
	}
}

// The projection reads a resume after a restart as Stop and Start, and
// a transcript that lost the resume's "cancelled" (a Send straight out
// of needs-you) is not a path in the spec.
func TestServeRestartResumeHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(fizzCheck(t, "serve_restart_resume"))
	if err != nil {
		t.Fatal(err)
	}
	e := func(kind string) history.Entry { return history.Entry{Kind: kind} }
	resumed := []history.Entry{e("meta"), e("input"), e("ask"), e("cancelled"), e("done"), e("input"), e("done")}
	if v := g.Check(serveRestartResumeHistory(resumed)); v != nil {
		t.Fatalf("a resume after a dead ask: %v", v)
	}
	lost := []history.Entry{e("meta"), e("input"), e("ask"), e("input"), e("done")}
	if v := g.Check(serveRestartResumeHistory(lost)); v == nil {
		t.Fatal("a Send out of needs-you passed the trace check")
	}
}

// roomDiff compares the spec's Room#0 fields with the adapter's state,
// through JSON so ints match the graph's numbers.
func roomDiff(want, got map[string]any) string {
	gb, _ := json.Marshal(got)
	var have map[string]any
	json.Unmarshal(gb, &have)
	var d []string
	for k, v := range want {
		field, ok := strings.CutPrefix(k, "Room#0.")
		if !ok {
			continue
		}
		if fmt.Sprint(have[field]) != fmt.Sprint(v) {
			d = append(d, fmt.Sprintf("%s: spec %v, room %v", field, v, have[field]))
		}
	}
	sort.Strings(d)
	return strings.Join(d, "; ")
}

// A run where Ask ends the turn instead of asking must fail, or the run
// above is not checking the room's state.
func TestServeRestartResumeCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newServeRestartResumeAdapter(t)
	a.askAsFinish = true
	if err := runMBT(t, "serve_restart_resume", a, serveRestartResumeActions, serveRestartResumeOptions()); err == nil {
		t.Fatal("a run whose Ask ends the turn as done passed; the runner is not checking state")
	}
}
