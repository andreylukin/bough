//go:build !windows

package mbt

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
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

// specs/unseen_trouble_ack.fizz against a real serve: one web session's
// unseen finish and trouble marks, and every way the control room acks
// them (the page's auto-ack of a shown finish, Mark seen, a drag to
// Done), all of which are POST /api/sessions/{id}/ack.
//
// Two of the spec's steps are never taken here (they answer
// errDisabled whatever the state):
//   - Expire needs seven days to pass, and serve reads the wall clock.
//   - Note is "a title or a notice written after the ack" with no turn.
//     A live serve child turns a notice into a wake-up turn (a new
//     input and done, so a new unseen finish), and the title is only
//     written as a turn ends; neither is the spec's Note.
//
// The runner (0.2.0) picks among all thirteen operations whatever the
// state and checks a walk only up to its first disabled pick, so a
// random walk rarely gets past a prompt: of 150, 132 never prompted
// and one was acked. TestUnseenTroubleAckPaths therefore also walks
// every path the generator wrote (the ones stage 3 walks in the
// browser), which covers each transition up to a Note or an Expire.

// errDisabled is the runner's own "not this one" (STATUS_NOT_IMPLEMENTED).
// A no-op would not do for Note and Expire: the graph enables them, so
// the server would check the unchanged state against the spec's.
var errDisabled = fmbt.ErrNotImplemented

// troubleTestCmd is a failing test run as the row reads one: a bash
// call whose command names a test runner and whose exit is not zero.
// It runs nothing: the "go test" is a comment.
const troubleTestCmd = "exit 3 # go test"

// unseenTroubleAckAdapter plays the control room against one serve. It
// is the fmbt.Model and the spec's one Session role.
type unseenTroubleAckAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	id     string
	screen string // "closed", "hidden" or "shown": the page, which is the adapter
	turn   int    // turn names are unique across walks: the queue is shared
	held   string // the model request in flight, "" when none
	ids    []string

	// failAsOK is the deliberate bug the wrong-adapter test injects: Fail
	// releases the turn as a success.
	failAsOK bool
}

func newUnseenTroubleAckAdapter(t *testing.T) *unseenTroubleAckAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &unseenTroubleAckAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

func (a *unseenTroubleAckAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.id, a.screen, a.held = row.ID, "closed", ""
	a.gate.reset()
	a.ids = append(a.ids, row.ID)
	return nil
}

func (a *unseenTroubleAckAdapter) Cleanup() error {
	if a.held == "" {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	_, err := waitRow(a.s, a.id, "the held turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning })
	return err
}

func (a *unseenTroubleAckAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

// GetState reads the marks off the row and what they are derived from
// off disk: the ack serve saved in meta.json and the transcript. Only
// screen is the adapter's own, since it is the page.
func (a *unseenTroubleAckAdapter) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return nil, err
	}
	ack, err := a.savedAck()
	if err != nil {
		return nil, err
	}
	entries, err := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
	if err != nil {
		return nil, err
	}
	// The spec's "last entry" is the last one that is news: a turn's
	// summary and title, written after its done, are not (serve's digest).
	var lastSeq, doneSeq int64
	stale := false
	for i := len(entries) - 1; i >= 0; i-- {
		if k := entries[i].Kind; k != "turn-summary" && k != "title" {
			lastSeq = entries[i].Seq
			stale = time.Since(entries[i].At) >= 7*24*time.Hour
			break
		}
	}
	for i := len(entries) - 1; i >= 0; i-- {
		if entries[i].Kind == "done" {
			doneSeq = entries[i].Seq
			break
		}
	}
	return map[string]any{
		"status":       specStatus(row.Status),
		"testsFailed":  row.TestsFailed,
		"newSinceAck":  lastSeq > ack,
		"doneSinceAck": doneSeq > ack,
		"stale":        stale,
		"screen":       a.screen,
		"trouble":      specTrouble(row.Trouble),
		"unseen":       row.Unseen,
	}, nil
}

// specStatus folds serve's statuses into the spec's: error and
// interrupted are both "failed", and mark trouble the same way.
func specStatus(s serve.Status) string {
	switch s {
	case serve.StatusError, serve.StatusInterrupted:
		return "failed"
	}
	return string(s)
}

func specTrouble(t string) string {
	if t == "interrupted" {
		return "failed"
	}
	return t
}

// savedAck is meta.Ack as serve wrote it to meta.json (0 when unset).
func (a *unseenTroubleAckAdapter) savedAck() (int64, error) {
	b, err := os.ReadFile(filepath.Join(a.s.Home, ".bough", "serve", "meta.json"))
	if os.IsNotExist(err) {
		return 0, nil
	}
	if err != nil {
		return 0, err
	}
	var m struct {
		Sessions map[string]struct {
			Ack int64 `json:"ack"`
		} `json:"sessions"`
	}
	if err := json.Unmarshal(b, &m); err != nil {
		return 0, fmt.Errorf("meta.json: %w", err)
	}
	return m.Sessions[a.id].Ack, nil
}

// view is the state each action reads the spec's require off.
func (a *unseenTroubleAckAdapter) view() map[string]any {
	st, err := a.GetState()
	if err != nil {
		return map[string]any{}
	}
	return st
}

func (a *unseenTroubleAckAdapter) Prompt() error {
	if !a.gate.pass(a.view()["status"] == "idle") {
		return nil
	}
	a.turn++
	name := fmt.Sprintf("u%04d", a.turn)
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

// TestFail answers the held request with a failing test command; the
// engine records it and asks the model again, and that request is held
// in turn, so the session is still running.
func (a *unseenTroubleAckAdapter) TestFail() error {
	v := a.view()
	if !a.gate.pass(v["status"] == "running" && v["testsFailed"] == false) {
		return nil
	}
	next := a.held + "b"
	control.Queue(a.t, a.dir, next, control.Turn{Mode: "block", Text: "finished " + next})
	control.ReleaseWith(a.t, a.dir, a.held, control.Turn{Bash: troubleTestCmd})
	a.held = next
	control.WaitTaken(a.t, a.dir, next, actionTimeout)
	_, err := waitRow(a.s, a.id, "tests failed", func(r serve.Row) bool { return r.TestsFailed && r.Status == serve.StatusRunning })
	return err
}

func (a *unseenTroubleAckAdapter) Finish() error {
	if !a.gate.pass(a.view()["status"] == "running") {
		return nil
	}
	return a.end(control.Turn{}, serve.StatusDone)
}

func (a *unseenTroubleAckAdapter) Fail() error {
	if !a.gate.pass(a.view()["status"] == "running") {
		return nil
	}
	if a.failAsOK {
		return a.end(control.Turn{}, serve.StatusDone)
	}
	return a.end(control.Turn{Mode: "error", Error: "model says no"}, serve.StatusError)
}

// end releases the held request as turn says and waits for the row to
// say want. It does not wait for the turn's summary and title, which
// the child writes a moment after the done: an ack that lands before
// them must hold (serve's digest), and a walk that acks at once checks
// that it does.
func (a *unseenTroubleAckAdapter) end(turn control.Turn, want serve.Status) error {
	if turn.Mode == "" {
		control.Release(a.t, a.dir, a.held)
	} else {
		control.ReleaseWith(a.t, a.dir, a.held, turn)
	}
	a.held = ""
	_, err := waitRow(a.s, a.id, string(want), func(r serve.Row) bool { return r.Status == want })
	return err
}

// Note and Expire cannot be driven here (see the top of the file); the
// walk's checking ends at them, so the gate closes too.
func (a *unseenTroubleAckAdapter) Note() error {
	a.gate.pass(false)
	return errDisabled
}

func (a *unseenTroubleAckAdapter) Expire() error {
	a.gate.pass(false)
	return errDisabled
}

func (a *unseenTroubleAckAdapter) Open() error {
	v := a.view()
	if !a.gate.pass(a.screen == "closed" && (v["status"] == "running" || v["unseen"] == true)) {
		return nil
	}
	a.screen = "hidden"
	return nil
}

func (a *unseenTroubleAckAdapter) Show() error {
	if !a.gate.pass(a.screen == "hidden") {
		return nil
	}
	a.screen = "shown"
	return nil
}

func (a *unseenTroubleAckAdapter) Close() error {
	if !a.gate.pass(a.screen != "closed") {
		return nil
	}
	a.screen = "closed"
	return nil
}

// AutoAck is app.tsx acking the unseen finish it has on screen.
func (a *unseenTroubleAckAdapter) AutoAck() error {
	if !a.gate.pass(a.screen == "shown" && a.view()["unseen"] == true) {
		return nil
	}
	return a.ack()
}

// MarkSeen is the thread header's Mark seen, offered on trouble only.
func (a *unseenTroubleAckAdapter) MarkSeen() error {
	if !a.gate.pass(a.view()["trouble"] != "") {
		return nil
	}
	return a.ack()
}

// DragDone is a project thread dropped on Done while another is shown.
func (a *unseenTroubleAckAdapter) DragDone() error {
	if !a.gate.pass(a.screen == "closed" && a.view()["unseen"] == true) {
		return nil
	}
	return a.ack()
}

func (a *unseenTroubleAckAdapter) ack() error {
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Ack(ctx, a.id)
	return err
}

var unseenTroubleAckActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Prompt":   action((*unseenTroubleAckAdapter).Prompt),
	"TestFail": action((*unseenTroubleAckAdapter).TestFail),
	"Finish":   action((*unseenTroubleAckAdapter).Finish),
	"Fail":     action((*unseenTroubleAckAdapter).Fail),
	"Note":     action((*unseenTroubleAckAdapter).Note),
	"Expire":   action((*unseenTroubleAckAdapter).Expire),
	"Open":     action((*unseenTroubleAckAdapter).Open),
	"Show":     action((*unseenTroubleAckAdapter).Show),
	"Close":    action((*unseenTroubleAckAdapter).Close),
	"AutoAck":  action((*unseenTroubleAckAdapter).AutoAck),
	"MarkSeen": action((*unseenTroubleAckAdapter).MarkSeen),
	"DragDone": action((*unseenTroubleAckAdapter).DragDone),
}, "": {
	// The spec turns deadlock detection off, and the runner then offers a
	// role-less "end" operation in every state. The library dereferences
	// a missing action (a nil-pointer panic that takes the test binary
	// and leaves the graph server orphaned on :50051), so it must be
	// here. A taken "end" matches no link in the graph, and the server
	// silently stops checking the walk there, so it declines and closes
	// the gate like any disabled pick.
	"end": func(m any, _ []fmbt.Arg) (any, error) {
		m.(*unseenTroubleAckAdapter).gate.pass(false)
		return nil, errDisabled
	},
}}

// Walks are cheap past their first disabled pick (the gate), so many of
// them buy the rare deep one; the paths test is the systematic cover.
func unseenTroubleAckOptions() map[string]any {
	return map[string]any{"max-seq-runs": 300, "max-actions": 8, "max-parallel-runs": 0}
}

// unseenTroubleAckHistory reads the trace off a transcript. Acks live
// in meta.json and the page is not in history, so the steps are the
// turn's (Prompt, TestFail, Finish or Fail) and the check is on status
// and testsFailed; the spec enables each of those whatever the screen.
func unseenTroubleAckHistory(entries []history.Entry) []tracecheck.Step {
	state := func(status string, tf bool) map[string]any {
		return map[string]any{"Session#0.status": status, "Session#0.testsFailed": tf}
	}
	steps := []tracecheck.Step{{Action: "Init", State: state("idle", false)}}
	failed, tf := false, false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			failed = false
			steps = append(steps, tracecheck.Step{Action: "Session#0.Prompt", State: state("running", tf)})
		case "call":
			// The row's test detection (serve's callTest): a bash call
			// naming a test runner with a non-zero exit.
			cmd, _ := e.Data["cmd"].(string)
			exit, _ := e.Data["exit"].(float64)
			if e.Data["tool"] == "bash" && strings.Contains(cmd, "go test") && exit != 0 && !tf {
				tf = true
				steps = append(steps, tracecheck.Step{Action: "Session#0.TestFail", State: state("running", tf)})
			}
		case "error":
			failed = true
		case "done":
			if failed {
				steps = append(steps, tracecheck.Step{Action: "Session#0.Fail", State: state("failed", tf)})
			} else {
				steps = append(steps, tracecheck.Step{Action: "Session#0.Finish", State: state("done", tf)})
			}
		}
	}
	return steps
}

func init() { historyProjections["unseen_trouble_ack"] = unseenTroubleAckHistory }

func TestUnseenTroubleAck(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newUnseenTroubleAckAdapter(t)
	if err := runMBT(t, "unseen_trouble_ack", a, unseenTroubleAckActions, unseenTroubleAckOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "unseen_trouble_ack"))
	if err != nil {
		t.Fatal(err)
	}
	// What the walks reached, so a green run that never got past a
	// prompt shows as one: sessions by end status, how many were acked.
	ends, acked := map[string]int{}, 0
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), unseenTroubleAckHistory)
		a.id = id
		if st, err := a.GetState(); err == nil {
			ends[st["status"].(string)]++
		}
		if ack, _ := a.savedAck(); ack > 0 {
			acked++
		}
	}
	t.Logf("walks: %d sessions, end status %v, %d acked", len(a.ids), ends, acked)
}

// A green TestUnseenTroubleAck proves nothing unless a server that
// breaks the model fails it: this adapter ends Fail as a clean finish.
func TestUnseenTroubleAckCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newUnseenTroubleAckAdapter(t)
	a.failAsOK = true
	if err := runMBT(t, "unseen_trouble_ack", a, unseenTroubleAckActions, unseenTroubleAckOptions()); err == nil {
		t.Fatal("a run whose Fail ends the turn as done passed; the runner is not checking state")
	}
}

// unseenTroubleAckPath is one path of testdata/unseen_trouble_ack/
// paths.json, the generator's cover of the graph: the links from Init
// and the state the spec has after each step.
type unseenTroubleAckPath struct {
	Links []int `json:"links"`
	Trace []struct {
		Action string         `json:"action"`
		State  map[string]any `json:"state"`
	} `json:"trace"`
}

func loadUnseenTroubleAckPaths(t *testing.T) []unseenTroubleAckPath {
	t.Helper()
	b, err := pathsJSON("unseen_trouble_ack")
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Paths []unseenTroubleAckPath `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		t.Fatal(err)
	}
	return f.Paths
}

// walkPath takes one path through the adapter, comparing the state after
// every step with the spec's. It stops, without error, at the first Note
// or Expire, and returns the links it checked.
func (a *unseenTroubleAckAdapter) walkPath(p unseenTroubleAckPath) (checked []int, err error) {
	if err := a.Init(); err != nil {
		return nil, err
	}
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	for i, step := range p.Trace {
		if i > 0 {
			name := strings.TrimPrefix(step.Action, "Session#0.")
			if _, err := unseenTroubleAckActions["Session"][name](a, nil); errors.Is(err, errDisabled) {
				return checked, nil
			} else if err != nil {
				return checked, fmt.Errorf("step %d (%s): %w", i, name, err)
			}
			if a.gate.off {
				return checked, fmt.Errorf("step %d (%s): the adapter says the spec's require does not hold", i, name)
			}
		}
		got, err := a.GetState()
		if err != nil {
			return checked, err
		}
		want := map[string]any{}
		for k, v := range step.State {
			if f, ok := strings.CutPrefix(k, "Session#0."); ok {
				want[f] = v
			}
		}
		if !reflect.DeepEqual(got, want) {
			return checked, fmt.Errorf("step %d (%s): state\n got  %v\n want %v", i, step.Action, got, want)
		}
		// links[i-1] is the link step i took (Init takes none).
		if i > 0 && i <= len(p.Links) {
			checked = append(checked, p.Links[i-1])
		}
	}
	return checked, nil
}

// Every generated path through a real serve: the runner's random walks
// seldom get past a prompt, and these are the paths stage 3 walks in
// the browser, so a divergence shows here first and without a page.
func TestUnseenTroubleAckPaths(t *testing.T) {
	t.Parallel()
	a := newUnseenTroubleAckAdapter(t)
	covered := map[int]bool{}
	paths := loadUnseenTroubleAckPaths(t)
	for i, p := range paths {
		checked, err := a.walkPath(p)
		if err != nil {
			t.Errorf("path %d: %v", i, err)
		}
		for _, l := range checked {
			covered[l] = true
		}
	}
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "unseen_trouble_ack"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), unseenTroubleAckHistory)
	}
	// Every link reachable without a Note or an Expire must have been
	// checked; the rest cannot be driven (see the top of the file).
	reach, seen := 0, map[int]bool{0: true}
	for queue := []int{0}; len(queue) > 0; queue = queue[1:] {
		for li, l := range g.Links {
			if l.Src != queue[0] || strings.HasSuffix(l.Name, ".Note") || strings.HasSuffix(l.Name, ".Expire") {
				continue
			}
			reach++
			if !covered[li] {
				t.Errorf("link %d (%s from node %d) is reachable without a Note or an Expire but no path checked it", li, l.Name, l.Src)
			}
			if !seen[l.Dest] {
				seen[l.Dest] = true
				queue = append(queue, l.Dest)
			}
		}
	}
	t.Logf("paths: %d walked, %d links checked, %d reachable without a Note or an Expire, %d in the graph", len(paths), len(covered), reach, len(g.Links))
}

// The paths test is only worth its time if a wrong server fails it.
func TestUnseenTroubleAckPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newUnseenTroubleAckAdapter(t)
	a.failAsOK = true
	for _, p := range loadUnseenTroubleAckPaths(t) {
		if _, err := a.walkPath(p); err != nil {
			return
		}
	}
	t.Fatal("every path passed with Fail ending the turn as done; the walk is not checking state")
}
