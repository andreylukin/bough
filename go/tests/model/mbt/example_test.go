//go:build !windows

package mbt

import (
	"fmt"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// The worked example: specs/example.fizz, one web session's lifecycle
// (idle -> running -> done|error, unseen cleared on view), driven
// through a real serve. A flow copies this file; see ../README.md.

// exampleAdapter plays the web page against one serve: Prompt is the
// composer, View and Leave are opening and leaving the transcript, and
// while viewing it acks an unseen finish the way app.tsx does. It is
// both the fmbt.Model and the spec's one Session role: fizzbee-mbt
// checks only role state.
type exampleAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	id      string // this walk's session
	viewing bool
	turn    int    // turn names are unique across walks: the queue is shared
	held    string // the turn in flight, "" when none
	ids     []string

	// failAsOK is the deliberate bug TestExampleCatchesWrongAdapter
	// injects: Fail releases the turn as a success.
	failAsOK bool
}

func newExampleAdapter(t *testing.T) *exampleAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &exampleAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

// Init starts each walk on a fresh idle session in the same serve.
func (a *exampleAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.id, a.viewing, a.held = row.ID, false, ""
	a.gate.reset()
	a.ids = append(a.ids, row.ID)
	return nil
}

// Cleanup lets a turn the walk left running finish, so its child is not
// holding a request while the next walk queues its own turns.
func (a *exampleAdapter) Cleanup() error {
	if a.held == "" {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	_, err := waitRow(a.s, a.id, "the held turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning })
	return err
}

func (a *exampleAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

// GetState is the Session role's state: status and unseen from the
// server's row, viewing from the adapter, which is the one looking.
func (a *exampleAdapter) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return nil, err
	}
	return map[string]any{"status": string(row.Status), "unseen": row.Unseen, "viewing": a.viewing}, nil
}

// Each action first asks the gate with the spec's `require`: a walk's
// validation ends at its first disabled action, so the rest is skipped.

func (a *exampleAdapter) Prompt() error {
	if !a.gate.pass(a.held == "") {
		return nil
	}
	a.turn++
	name := fmt.Sprintf("t%04d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, "turn "+name); err != nil {
		return err
	}
	a.held = name
	control.WaitTaken(a.t, a.dir, name, actionTimeout)
	_, err := waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
	return err
}

func (a *exampleAdapter) Finish() error {
	if !a.gate.pass(a.held != "") {
		return nil
	}
	return a.end(control.Turn{}, serve.StatusDone)
}

func (a *exampleAdapter) Fail() error {
	if !a.gate.pass(a.held != "") {
		return nil
	}
	if a.failAsOK {
		return a.end(control.Turn{}, serve.StatusDone)
	}
	return a.end(control.Turn{Mode: "error", Error: "model says no"}, serve.StatusError)
}

// end releases the held turn as turn says and waits for the row to
// settle on want; a page that is viewing then acks the finish.
func (a *exampleAdapter) end(turn control.Turn, want serve.Status) error {
	if turn.Mode == "" {
		control.Release(a.t, a.dir, a.held)
	} else {
		control.ReleaseWith(a.t, a.dir, a.held, turn)
	}
	a.held = ""
	row, err := waitRow(a.s, a.id, string(want), func(r serve.Row) bool { return r.Status == want })
	if err != nil {
		return err
	}
	if a.viewing && row.Unseen {
		return a.ack()
	}
	return nil
}

func (a *exampleAdapter) View() error {
	if !a.gate.pass(!a.viewing) {
		return nil
	}
	a.viewing = true
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil || !row.Unseen {
		return err
	}
	return a.ack()
}

func (a *exampleAdapter) Leave() error {
	if a.gate.pass(a.viewing) {
		a.viewing = false
	}
	return nil
}

func (a *exampleAdapter) ack() error {
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Ack(ctx, a.id)
	return err
}

var exampleActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Prompt": action((*exampleAdapter).Prompt),
	"Finish": action((*exampleAdapter).Finish),
	"Fail":   action((*exampleAdapter).Fail),
	"View":   action((*exampleAdapter).View),
	"Leave":  action((*exampleAdapter).Leave),
}}

// Every step of a walk is a real turn through a real serve, so the
// default run is short; --max-seq-runs and --seq-seed raise or vary it
// when hunting. Parallel runs would drive one adapter from several
// sequences at once, so they are off.
func exampleOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 6, "max-parallel-runs": 0}
}

// exampleHistory reads the abstract trace off a transcript: an input is
// Prompt, and the turn's close is Fail when an error entry came inside
// it, else Finish. View and Leave leave nothing in history, and the
// check is on status alone, which they never change.
func exampleHistory(entries []history.Entry) []tracecheck.Step {
	status := func(s string) map[string]any { return map[string]any{"Session#0.status": s} }
	steps := []tracecheck.Step{{Action: "Init", State: status("idle")}}
	failed := false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			failed = false
			steps = append(steps, tracecheck.Step{Action: "Session#0.Prompt", State: status("running")})
		case "error":
			failed = true
		case "done":
			if failed {
				steps = append(steps, tracecheck.Step{Action: "Session#0.Fail", State: status("error")})
			} else {
				steps = append(steps, tracecheck.Step{Action: "Session#0.Finish", State: status("done")})
			}
		}
	}
	return steps
}

func init() { historyProjections["example"] = exampleHistory }

func TestExample(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newExampleAdapter(t)
	if err := runMBT(t, "example", a, exampleActions, exampleOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	// Every transcript the walks wrote is itself a path in the model.
	g, err := tracecheck.Load(fizzCheck(t, "example"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), exampleHistory)
	}
}

// The run above proves nothing unless a server that breaks the model
// fails it. This adapter releases Fail as a success, the kind of wrong
// wiring a flow's adapter can have, and the run must say so.
func TestExampleCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newExampleAdapter(t)
	a.failAsOK = true
	if err := runMBT(t, "example", a, exampleActions, exampleOptions()); err == nil {
		t.Fatal("a run whose Fail ends the turn as done passed; the runner is not checking state")
	}
}
