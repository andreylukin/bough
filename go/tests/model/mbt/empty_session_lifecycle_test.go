//go:build !windows

package mbt

import (
	"encoding/json"
	"fmt"
	"path/filepath"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/empty_session_lifecycle.fizz against a real serve: one session
// started with no prompt, and which of the page's surfaces list it.
//
// The adapter plays the page. rows is the list it last read (GET
// /api/sessions, archived left out, as the page reads it with the
// Archived section shut): at load, after each of its own acts (they all
// end in refresh()), and at Poll. seen, sidebar, recent and welcome are
// read off rows with the page's own rules, transcribed below; open and
// filter are the page's own state. made, live, input, titled and
// archived are read off the server on every GetState.
//
// One serve for the whole run: Init archives the session a past walk
// made (which also kills its child), so the page starts on a server
// with nothing unarchived, the welcome's case. ServeRestart restarts
// serve on the same HOME and port and leaves the page as it was.

type emptySessionLifecycleAdapter struct {
	t    *testing.T
	s    *servetest.Server
	cwd  string
	gate gate

	id     string // this walk's session, "" before StartEmpty
	name   string // what Rename sets
	open   bool
	filter bool
	rows   []serve.Row // the page's last read of the list
	walk   int
	ids    []string

	// restartRereads is the deliberate bug the wrong-adapter tests
	// inject: the page learns of a serve restart at once instead of at
	// its next poll.
	restartRereads bool
}

func newEmptySessionLifecycleAdapter(t *testing.T) *emptySessionLifecycleAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &emptySessionLifecycleAdapter{t: t, s: s, cwd: s.Dir(t, "work")}
}

func (a *emptySessionLifecycleAdapter) Init() error {
	a.gate.reset()
	if a.id != "" {
		ctx, cancel := actionCtx()
		defer cancel()
		row, _, err := a.s.GetSession(ctx, a.id)
		if err != nil {
			return err
		}
		if !row.Archived {
			if _, err := a.s.Archive(ctx, a.id); err != nil {
				return err
			}
		}
	}
	a.walk++
	a.id, a.name, a.open, a.filter = "", "", false, false
	return a.refresh()
}

func (a *emptySessionLifecycleAdapter) Cleanup() error { return nil }

func (a *emptySessionLifecycleAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Empty", Index: 0}: a}, nil
}

// refresh is the page's refresh(): it re-reads the list.
func (a *emptySessionLifecycleAdapter) refresh() error {
	ctx, cancel := actionCtx()
	defer cancel()
	rows, err := a.s.ListSessions(ctx, false)
	if err != nil {
		return err
	}
	a.rows = rows
	return nil
}

// row is this walk's session as the page last read it.
func (a *emptySessionLifecycleAdapter) row() (serve.Row, bool) {
	for _, r := range a.rows {
		if r.ID == a.id {
			return r, true
		}
	}
	return serve.Row{}, false
}

func (a *emptySessionLifecycleAdapter) selected() string {
	if a.open {
		return a.id
	}
	return ""
}

// hiddenEmpty is the page's rule for an empty session it does not list
// (hiddenEmpty in web/src/palette.tsx): nobody sent it a message, its
// child is gone, and nobody named it. A project session whose orb failed
// stays, or the failure would be swallowed.
func hiddenEmpty(r serve.Row) bool {
	failed := r.Orb != nil && r.Orb.Status == "failed"
	return r.Empty && !r.Live && r.Title == "" && !failed
}

// sidebarOf is the Sidebar's rows loop (app.tsx): a hidden empty session
// shows anyway while it is open or a filter is set, and so does a
// project's thread; an archived one is not in the list the page read.
func (a *emptySessionLifecycleAdapter) sidebarOf() string {
	if a.id == "" {
		return "none"
	}
	r, ok := a.row()
	if !ok {
		return "hidden"
	}
	member := r.Project != "" && !r.Archived
	if !member && hiddenEmpty(r) && !r.Archived && r.ID != a.selected() && !a.filter {
		return "hidden"
	}
	return "listed"
}

// recentOf is the palette's recentSessions: never the open session.
func (a *emptySessionLifecycleAdapter) recentOf() bool {
	r, ok := a.row()
	return ok && !r.Archived && !hiddenEmpty(r) && r.ID != a.selected()
}

// welcomeOf is showWelcome with welcome "auto" (noSessionsListed in
// app.tsx), which the thread pane shows only while nothing is open: on
// while the page read nothing unarchived the sidebar would list.
func (a *emptySessionLifecycleAdapter) welcomeOf() bool {
	if a.open {
		return false
	}
	for _, r := range a.rows {
		if !r.Archived && (r.Project != "" || !hiddenEmpty(r)) {
			return false
		}
	}
	return true
}

// GetState reads the session off the server and the surfaces off rows.
func (a *emptySessionLifecycleAdapter) GetState() (map[string]any, error) {
	st := map[string]any{
		"made": a.id != "", "live": false, "input": false, "titled": false, "archived": false,
		"open": a.open, "filter": a.filter, "seen": false,
		"sidebar": a.sidebarOf(), "recent": a.recentOf(), "welcome": a.welcomeOf(),
	}
	if r, ok := a.row(); ok {
		st["seen"] = r.Live
	}
	if a.id == "" {
		return st, nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return nil, err
	}
	st["live"] = row.Live
	st["input"] = !row.Empty
	st["titled"] = a.name != "" && row.Title == a.name
	st["archived"] = row.Archived
	return st, nil
}

// StartEmpty is start(dir, ""): POST /api/sessions with no prompt, then
// refresh and open #/s/<id>. The child is up before the 201.
func (a *emptySessionLifecycleAdapter) StartEmpty() error {
	if !a.gate.pass(a.id == "") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.cwd, "")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	a.open = true
	return a.refresh()
}

// NavigateAway goes to the overview: the stream closes and the list
// poll's effect reads the list at once.
func (a *emptySessionLifecycleAdapter) NavigateAway() error {
	if !a.gate.pass(a.open) {
		return nil
	}
	a.open = false
	return a.refresh()
}

// OpenByLink is a pasted #/s/<id>: the page reads the transcript, which
// must not spawn a child.
func (a *emptySessionLifecycleAdapter) OpenByLink() error {
	if !a.gate.pass(a.id != "" && !a.open) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if _, _, err := a.s.GetSession(ctx, a.id); err != nil {
		return err
	}
	a.open = true
	return nil
}

// ServeRestart stops serve with SIGTERM and starts it again on the same
// HOME and port; the page keeps the rows it had.
func (a *emptySessionLifecycleAdapter) ServeRestart() error {
	st, err := a.GetState()
	if err != nil {
		return err
	}
	if !a.gate.pass(st["live"] == true) {
		return nil
	}
	a.s.Shutdown()
	if err := a.s.Resume(""); err != nil {
		return err
	}
	if a.restartRereads {
		return a.refresh()
	}
	return nil
}

func (a *emptySessionLifecycleAdapter) Poll() error {
	st, err := a.GetState()
	if err != nil {
		return err
	}
	if !a.gate.pass(st["seen"] != st["live"]) {
		return nil
	}
	return a.refresh()
}

func (a *emptySessionLifecycleAdapter) TypeFilter() error {
	if a.gate.pass(a.id != "" && !a.filter) {
		a.filter = true
	}
	return nil
}

func (a *emptySessionLifecycleAdapter) ClearFilter() error {
	if a.gate.pass(a.filter) {
		a.filter = false
	}
	return nil
}

// Rename is onRename: POST rename, then refresh.
func (a *emptySessionLifecycleAdapter) Rename() error {
	st, err := a.GetState()
	if err != nil {
		return err
	}
	if !a.gate.pass(st["titled"] == false && st["archived"] == false && (a.open || st["sidebar"] == "listed")) {
		return nil
	}
	a.name = fmt.Sprintf("named %d", a.walk)
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Rename(ctx, a.id, a.name); err != nil {
		return err
	}
	return a.refresh()
}

// Archive is archiveRow with no agents: POST archive, then refresh. The
// page stays on the session if it was open.
func (a *emptySessionLifecycleAdapter) Archive() error {
	st, err := a.GetState()
	if err != nil {
		return err
	}
	if !a.gate.pass(st["made"] == true && st["archived"] == false && (a.open || st["sidebar"] == "listed")) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if _, err := a.s.Archive(ctx, a.id); err != nil {
		return err
	}
	return a.refresh()
}

func (a *emptySessionLifecycleAdapter) Unarchive() error {
	st, err := a.GetState()
	if err != nil {
		return err
	}
	if !a.gate.pass(st["archived"] == true) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if _, err := a.s.Unarchive(ctx, a.id); err != nil {
		return err
	}
	return a.refresh()
}

// SendFirstMessage is the composer's Send: nothing is queued for the
// model, so llm-control answers at once. deliverTo refreshes after.
func (a *emptySessionLifecycleAdapter) SendFirstMessage() error {
	st, err := a.GetState()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.open && st["input"] == false && st["archived"] == false) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, "first message"); err != nil {
		return err
	}
	if _, err := waitRow(a.s, a.id, "the first message recorded, child up", func(r serve.Row) bool { return !r.Empty && r.Live }); err != nil {
		return err
	}
	return a.refresh()
}

var emptySessionLifecycleActions = map[string]map[string]fmbt.ActionFunc{"Empty": {
	"StartEmpty":       action((*emptySessionLifecycleAdapter).StartEmpty),
	"NavigateAway":     action((*emptySessionLifecycleAdapter).NavigateAway),
	"OpenByLink":       action((*emptySessionLifecycleAdapter).OpenByLink),
	"ServeRestart":     action((*emptySessionLifecycleAdapter).ServeRestart),
	"Poll":             action((*emptySessionLifecycleAdapter).Poll),
	"TypeFilter":       action((*emptySessionLifecycleAdapter).TypeFilter),
	"ClearFilter":      action((*emptySessionLifecycleAdapter).ClearFilter),
	"Rename":           action((*emptySessionLifecycleAdapter).Rename),
	"Archive":          action((*emptySessionLifecycleAdapter).Archive),
	"Unarchive":        action((*emptySessionLifecycleAdapter).Unarchive),
	"SendFirstMessage": action((*emptySessionLifecycleAdapter).SendFirstMessage),
}}

// Only StartEmpty is enabled at Init, so eleven in twelve walks end at
// their first pick; 300 walks reach a ServeRestart often enough for the
// wrong-adapter run to meet one.
func emptySessionLifecycleOptions() map[string]any {
	return map[string]any{"max-seq-runs": 300, "max-actions": 8, "max-parallel-runs": 0}
}

// emptySessionLifecycleHistory reads the trace off a transcript. History
// holds no rename, archive or navigation, and an empty session's file
// has no input: the steps are the creation (StartEmpty) and the first
// input (SendFirstMessage), which the spec allows straight after it.
func emptySessionLifecycleHistory(entries []history.Entry) []tracecheck.Step {
	f := func(kv ...any) map[string]any {
		m := map[string]any{}
		for i := 0; i < len(kv); i += 2 {
			m["Empty#0."+kv[i].(string)] = kv[i+1]
		}
		return m
	}
	steps := []tracecheck.Step{
		{Action: "Init", State: f("made", false)},
		{Action: "Empty#0.StartEmpty", State: f("made", true, "input", false, "live", true)},
	}
	for _, e := range entries {
		if e.Kind == "input" {
			steps = append(steps, tracecheck.Step{Action: "Empty#0.SendFirstMessage", State: f("input", true, "live", true)})
		}
	}
	return steps
}

func init() { historyProjections["empty_session_lifecycle"] = emptySessionLifecycleHistory }

func TestEmptySessionLifecycle(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newEmptySessionLifecycleAdapter(t)
	if err := runMBT(t, "empty_session_lifecycle", a, emptySessionLifecycleActions, emptySessionLifecycleOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkEmptySessionHistories(t, a)
}

func TestEmptySessionLifecycleCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newEmptySessionLifecycleAdapter(t)
	a.restartRereads = true
	if err := runMBT(t, "empty_session_lifecycle", a, emptySessionLifecycleActions, emptySessionLifecycleOptions()); err == nil {
		t.Fatal("a run whose page saw a serve restart at once passed; the runner is not checking state")
	}
}

// Walks every path over the checked-in graph (every state; every link
// under MODEL_COVER=transitions) and compares the adapter's state with
// the spec's after each step.
func TestEmptySessionLifecyclePaths(t *testing.T) {
	t.Parallel()
	b, err := pathsJSON("empty_session_lifecycle")
	if err != nil {
		t.Fatal(err)
	}
	a := newEmptySessionLifecycleAdapter(t)
	if err := walkEmptySessionPaths(a, b); err != nil {
		t.Fatal(err)
	}
	checkEmptySessionHistories(t, a)
}

// The wrong adapter's bug shows on the ServeRestart link, so it walks
// every link: its green must not depend on which paths reach one.
func TestEmptySessionLifecyclePathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	b, err := pathsJSONCover("empty_session_lifecycle", tracecheck.CoverTransitions)
	if err != nil {
		t.Fatal(err)
	}
	a := newEmptySessionLifecycleAdapter(t)
	a.restartRereads = true
	err = walkEmptySessionPaths(a, b)
	if err == nil {
		t.Fatal("a paths walk whose page saw a serve restart at once passed")
	}
	t.Logf("caught: %v", err)
}

func checkEmptySessionHistories(t *testing.T, a *emptySessionLifecycleAdapter) {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join("..", "testdata", "empty_session_lifecycle"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), emptySessionLifecycleHistory)
	}
}

func walkEmptySessionPaths(a *emptySessionLifecycleAdapter, b []byte) error {
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		return err
	}
	acts := emptySessionLifecycleActions["Empty"]
	for pi, p := range doc.Paths {
		if err := a.Init(); err != nil {
			return fmt.Errorf("path %d: Init: %w", pi, err)
		}
		var names []string
		for si, step := range p.Trace {
			name := strings.TrimPrefix(step.Action, "Empty#0.")
			names = append(names, name)
			if si > 0 {
				if _, err := acts[name](a, nil); err != nil {
					return fmt.Errorf("path %d %v: %w", pi, names, err)
				}
				if a.gate.off {
					return fmt.Errorf("path %d %v: the adapter found %s not enabled", pi, names, name)
				}
			}
			got, err := a.GetState()
			if err != nil {
				return fmt.Errorf("path %d %v: %w", pi, names, err)
			}
			var d []string
			for k, v := range step.State {
				field, ok := strings.CutPrefix(k, "Empty#0.")
				if ok && got[field] != v {
					d = append(d, fmt.Sprintf("%s is %v, the spec says %v", field, got[field], v))
				}
			}
			if d != nil {
				return fmt.Errorf("path %d %v: %s", pi, names, strings.Join(d, "; "))
			}
		}
	}
	return nil
}

// The projection reads creation then the first message as a path, and a
// second message (which the spec's one-message flow has no step for) or a
// message the session never got created for is not one.
func TestEmptySessionLifecycleHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(filepath.Join("..", "testdata", "empty_session_lifecycle"))
	if err != nil {
		t.Fatal(err)
	}
	e := func(kind string) history.Entry { return history.Entry{Kind: kind} }
	for _, ok := range [][]history.Entry{{e("meta")}, {e("meta"), e("input"), e("done")}} {
		if v := g.Check(emptySessionLifecycleHistory(ok)); v != nil {
			t.Fatalf("%v: %v", ok, v)
		}
	}
	two := []history.Entry{e("meta"), e("input"), e("done"), e("input"), e("done")}
	if v := g.Check(emptySessionLifecycleHistory(two)); v == nil {
		t.Fatal("a second message passed the trace check")
	}
}
