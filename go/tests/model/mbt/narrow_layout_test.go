//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"fmt"
	"reflect"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/narrow_layout.fizz at the server: the control room at phone
// width. Almost all of the Phone role is the page's own state (which
// pane, which view, the overlays, the keyboard), which no API reports,
// so the adapter keeps it the way app.tsx does, and the browser stage is
// where it is read off the DOM. What the server decides is read from it
// on every step:
//
//	empty   GET /api/sessions lists no session that is not archived
//	        (showWelcome's `!rows.some((r) => !r.archived)`)
//	unseen  the walk's session row's unseen, off the same list
//
// and the server is driven the way the page drives it: the welcome's
// Start and another tab's New session are POST /api/sessions, opening a
// thread is GET /api/sessions/{id}, Finish is a real turn through
// llm-control, Ack is POST .../ack. So a walk checks that listing and
// loading a transcript never ack it, that a finish nobody saw is unseen
// until the page acks, and that archiving empties the list again.
type narrowLayoutAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	cwd  string
	gate gate

	id   string // this walk's session, "" until one exists
	ids  []string
	turn int // turn names are unique across walks: the queue is shared
	held string

	// The page's state, as app.tsx keeps it.
	pane, view, overlay      string
	selected, welcome        bool
	keyboard, fitted, backed bool
	ackedOnList              bool

	// keepLast is the deliberate bug TestNarrowLayoutCatchesWrongAdapter
	// injects: Init does not archive the last walk's session.
	keepLast bool
	// ackOnFinish is the one TestNarrowLayoutPathsCatchWrongAdapter
	// injects: a page that acks every finish while a session is selected,
	// the desktop rule, forgetting that on a phone the list may be the
	// pane.
	ackOnFinish bool
}

func newNarrowLayoutAdapter(t *testing.T) *narrowLayoutAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &narrowLayoutAdapter{t: t, s: s, dir: control.Dir(s.Home), cwd: s.Dir(t, "work")}
}

// Init archives the last walk's session, so the server lists none again
// and the page opens on the welcome, as a fresh phone on an empty server
// does (the showWelcome effect sets pane=thread).
func (a *narrowLayoutAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	if a.id != "" && !a.keepLast {
		if _, err := a.s.Archive(ctx, a.id); err != nil {
			return err
		}
	}
	a.id, a.held = "", ""
	a.pane, a.view, a.overlay = "thread", "sessions", "none"
	a.selected, a.welcome = false, true
	a.keyboard, a.fitted, a.backed, a.ackedOnList = false, true, false, false
	a.gate.reset()
	return nil
}

func (a *narrowLayoutAdapter) Cleanup() error {
	if a.held == "" {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	_, err := waitRow(a.s, a.id, "the held turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning })
	return err
}

func (a *narrowLayoutAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Phone", Index: 0}: a}, nil
}

// server reads empty and unseen off the list, the page's poll.
func (a *narrowLayoutAdapter) server(ctx context.Context) (empty, unseen bool, err error) {
	rows, err := a.s.ListSessions(ctx, false)
	if err != nil {
		return false, false, err
	}
	empty = true
	for _, r := range rows {
		if r.Archived {
			continue
		}
		empty = false
		if r.ID == a.id {
			unseen = r.Unseen
		}
	}
	return empty, unseen, nil
}

func (a *narrowLayoutAdapter) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	empty, unseen, err := a.server(ctx)
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"pane": a.pane, "view": a.view, "selected": a.selected,
		"empty": empty, "welcome": a.welcome, "unseen": unseen,
		"overlay": a.overlay, "keyboard": a.keyboard, "fitted": a.fitted,
		"backed": a.backed, "acked_on_list": a.ackedOnList,
	}, nil
}

// The page's derived values (app.tsx): the welcome, the phone ViewNav,
// the Back the thread-pane routes pass, what can take typing, viewing.

func (a *narrowLayoutAdapter) showWelcome(empty bool) bool { return a.welcome && empty }

func (a *narrowLayoutAdapter) navVisible() bool { return a.pane == "list" || a.view != "sessions" }

func (a *narrowLayoutAdapter) hasBack(empty bool) bool {
	if a.view != "sessions" {
		return true
	}
	return a.selected || a.showWelcome(empty)
}

func (a *narrowLayoutAdapter) canType() bool {
	if a.overlay == "settings" || a.overlay == "panel" {
		return true
	}
	if a.overlay != "none" || a.pane != "thread" {
		return false
	}
	return (a.view == "sessions" && a.selected) || a.view == "project"
}

func (a *narrowLayoutAdapter) viewing() bool {
	return a.selected && (a.view == "sessions" || a.view == "project") && a.pane == "thread"
}

// state reads the server's half for an action's require.
func (a *narrowLayoutAdapter) state() (empty, unseen bool) {
	ctx, cancel := actionCtx()
	defer cancel()
	empty, unseen, err := a.server(ctx)
	if err != nil {
		// A require that cannot be read is a disabled action: the step
		// that follows reports the server's failure through GetState.
		return false, true
	}
	return empty, unseen
}

// load is what showing a transcript costs the server: the page GETs the
// session. It must not ack.
func (a *narrowLayoutAdapter) load() error {
	ctx, cancel := actionCtx()
	defer cancel()
	_, _, err := a.s.GetSession(ctx, a.id)
	return err
}

func (a *narrowLayoutAdapter) create() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.cwd, "")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	return nil
}

func (a *narrowLayoutAdapter) OpenRow() error {
	empty, _ := a.state()
	if !a.gate.pass(a.pane == "list" && !empty) {
		return nil
	}
	a.view, a.selected, a.pane, a.backed = "sessions", true, "thread", false
	return a.load()
}

func (a *narrowLayoutAdapter) SearchKey() error {
	if a.gate.pass(a.overlay == "none" && a.view == "sessions" && a.pane == "thread" && a.selected) {
		a.pane, a.keyboard, a.backed = "list", false, false
	}
	return nil
}

func (a *narrowLayoutAdapter) Back() error {
	empty, _ := a.state()
	if !a.gate.pass(a.pane == "thread" && a.overlay == "none" && a.hasBack(empty)) {
		return nil
	}
	if a.showWelcome(empty) {
		a.welcome = false // the welcome's Back is setWelcome("off"); goList()
	}
	a.view, a.selected, a.pane, a.keyboard, a.backed = "sessions", false, "list", false, true
	return nil
}

func (a *narrowLayoutAdapter) NavPage() error {
	if a.gate.pass(a.navVisible() && a.overlay == "none") {
		a.view, a.selected, a.pane, a.keyboard, a.backed = "page", false, "thread", false, false
	}
	return nil
}

func (a *narrowLayoutAdapter) NavSessions() error {
	if a.gate.pass(a.navVisible() && a.overlay == "none" && a.pane == "thread") {
		a.view, a.selected, a.pane, a.keyboard, a.backed = "sessions", false, "list", false, false
	}
	return nil
}

func (a *narrowLayoutAdapter) OpenProject() error {
	if a.gate.pass(a.view == "page" && a.overlay == "none") {
		a.view, a.selected, a.pane, a.backed = "project", false, "thread", false
	}
	return nil
}

func (a *narrowLayoutAdapter) OpenThread() error {
	empty, _ := a.state()
	if !a.gate.pass(a.view == "project" && a.overlay == "none" && !a.selected && !empty) {
		return nil
	}
	a.selected, a.keyboard, a.backed = true, false, false
	return a.load()
}

// Reload: the hash decides the pane, then the welcome effect runs once
// the first list lands. A reload onto a transcript loads it again.
func (a *narrowLayoutAdapter) Reload() error {
	empty, _ := a.state()
	if !a.gate.pass(true) {
		return nil
	}
	onList := a.view == "sessions" && !a.selected
	a.pane = "thread"
	if onList && !a.showWelcome(empty) {
		a.pane = "list"
	}
	a.overlay, a.keyboard, a.fitted = "none", false, true
	if a.selected {
		return a.load()
	}
	return nil
}

// WelcomeStart is the welcome's Start: a web session, opened at once.
func (a *narrowLayoutAdapter) WelcomeStart() error {
	empty, _ := a.state()
	if !a.gate.pass(a.showWelcome(empty) && a.pane == "thread" && a.view == "sessions" && !a.selected) {
		return nil
	}
	if err := a.create(); err != nil {
		return err
	}
	a.welcome, a.selected, a.backed = false, true, false
	return a.load()
}

// Arrive is another tab's New session: the same API, a web session.
func (a *narrowLayoutAdapter) Arrive() error {
	empty, _ := a.state()
	if !a.gate.pass(empty && !a.welcome) {
		return nil
	}
	return a.create()
}

func (a *narrowLayoutAdapter) openOverlay(ok bool, name string, fitted bool) error {
	if a.gate.pass(ok) {
		a.overlay, a.keyboard, a.fitted, a.backed = name, false, fitted, false
	}
	return nil
}

func (a *narrowLayoutAdapter) OpenSettings() error {
	return a.openOverlay(a.overlay == "none" && a.pane == "thread" && a.selected && a.view != "page", "settings", true)
}

// OpenWork: the sheet is fixed at 80dvh, which the keyboard does not
// shrink, so it is not fitted to the visual viewport.
func (a *narrowLayoutAdapter) OpenWork() error {
	return a.openOverlay(a.overlay == "none" && a.pane == "thread" && a.selected && a.view == "sessions", "work", false)
}

func (a *narrowLayoutAdapter) OpenThreads() error {
	return a.openOverlay(a.overlay == "none" && a.view == "project" && a.selected, "threads", true)
}

func (a *narrowLayoutAdapter) OpenPanel() error {
	return a.openOverlay(a.overlay == "none" && a.view == "project", "panel", true)
}

func (a *narrowLayoutAdapter) CloseOverlay() error {
	if a.gate.pass(a.overlay != "none") {
		a.overlay, a.keyboard, a.fitted = "none", false, true
	}
	return nil
}

func (a *narrowLayoutAdapter) KeyboardUp() error {
	if !a.gate.pass(!a.keyboard && a.canType()) {
		return nil
	}
	a.keyboard = true
	if a.overlay != "none" {
		a.fitted = a.overlay == "settings" || a.overlay == "panel"
	}
	return nil
}

func (a *narrowLayoutAdapter) KeyboardDown() error {
	if a.gate.pass(a.keyboard) {
		a.keyboard = false
	}
	return nil
}

// Finish is a real turn in the walk's session while the list is the
// pane: queued, prompted, seen running, released, seen done. The server
// then says unseen until the page acks it.
func (a *narrowLayoutAdapter) Finish() error {
	empty, unseen := a.state()
	if !a.gate.pass(!empty && !unseen && a.pane == "list") {
		return nil
	}
	a.turn++
	name := fmt.Sprintf("n%04d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, "turn "+name); err != nil {
		return err
	}
	a.held = name
	control.WaitTaken(a.t, a.dir, name, actionTimeout)
	if _, err := waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning }); err != nil {
		return err
	}
	control.Release(a.t, a.dir, name)
	a.held = ""
	if _, err := waitRow(a.s, a.id, "done", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		return err
	}
	if a.ackOnFinish && a.selected {
		return a.ack()
	}
	return nil
}

// Ack is the page's ack of an unseen finish that is on screen.
func (a *narrowLayoutAdapter) Ack() error {
	_, unseen := a.state()
	if !a.gate.pass(unseen && a.viewing()) {
		return nil
	}
	return a.ack()
}

func (a *narrowLayoutAdapter) ack() error {
	if a.pane == "list" {
		a.ackedOnList = true
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Ack(ctx, a.id)
	return err
}

var narrowLayoutActions = map[string]map[string]fmbt.ActionFunc{"Phone": {
	"OpenRow":      action((*narrowLayoutAdapter).OpenRow),
	"SearchKey":    action((*narrowLayoutAdapter).SearchKey),
	"Back":         action((*narrowLayoutAdapter).Back),
	"NavPage":      action((*narrowLayoutAdapter).NavPage),
	"NavSessions":  action((*narrowLayoutAdapter).NavSessions),
	"OpenProject":  action((*narrowLayoutAdapter).OpenProject),
	"OpenThread":   action((*narrowLayoutAdapter).OpenThread),
	"Reload":       action((*narrowLayoutAdapter).Reload),
	"WelcomeStart": action((*narrowLayoutAdapter).WelcomeStart),
	"Arrive":       action((*narrowLayoutAdapter).Arrive),
	"OpenSettings": action((*narrowLayoutAdapter).OpenSettings),
	"OpenWork":     action((*narrowLayoutAdapter).OpenWork),
	"OpenThreads":  action((*narrowLayoutAdapter).OpenThreads),
	"OpenPanel":    action((*narrowLayoutAdapter).OpenPanel),
	"CloseOverlay": action((*narrowLayoutAdapter).CloseOverlay),
	"KeyboardUp":   action((*narrowLayoutAdapter).KeyboardUp),
	"KeyboardDown": action((*narrowLayoutAdapter).KeyboardDown),
	"Finish":       action((*narrowLayoutAdapter).Finish),
	"Ack":          action((*narrowLayoutAdapter).Ack),
}}

// Most steps are page state and cost nothing, so there are more walks
// than the example's; TestNarrowLayoutPaths is what reaches the deep
// server steps (a finish on the list, back to the thread, the ack).
func narrowLayoutOptions() map[string]any {
	return map[string]any{"max-seq-runs": 600, "max-actions": 8, "max-parallel-runs": 0}
}

// narrowLayoutHistory reads the trace off a transcript. The transcript
// holds only what the server saw: the turns. Everything the page did
// between them (which pane, the ack) is not in it, so between turns the
// projection takes the one canonical page path the spec allows, and the
// check is on the server's half of the state: a turn is a Finish while
// the list is the pane, and it must leave the session unseen, which a
// turn that failed, or one that never closed, does not.
//
// The canonical path: the welcome's Back (its Back dismisses it), the
// session arriving, then per turn Finish, and before the next one the
// row opened, acked and left.
func narrowLayoutHistory(entries []history.Entry) []tracecheck.Step {
	st := func(empty, unseen bool) map[string]any {
		return map[string]any{"Phone#0.empty": empty, "Phone#0.unseen": unseen}
	}
	steps := []tracecheck.Step{
		{Action: "Init", State: st(true, false)},
		{Action: "Phone#0.Back", State: st(true, false)},
		{Action: "Phone#0.Arrive", State: st(false, false)},
	}
	for i, e := range entries {
		if e.Kind != "input" {
			continue
		}
		// The turn's close: a done with no error inside the turn.
		clean := false
	close:
		for _, f := range entries[i+1:] {
			switch f.Kind {
			case "input":
				break close
			case "error":
				clean = false
				break close
			case "done":
				clean = true
				break close
			}
		}
		if len(steps) > 3 {
			steps = append(steps,
				tracecheck.Step{Action: "Phone#0.OpenRow", State: st(false, true)},
				tracecheck.Step{Action: "Phone#0.Ack", State: st(false, false)},
				tracecheck.Step{Action: "Phone#0.Back", State: st(false, false)})
		}
		steps = append(steps, tracecheck.Step{Action: "Phone#0.Finish", State: st(false, clean)})
	}
	return steps
}

func init() { historyProjections["narrow_layout"] = narrowLayoutHistory }

func TestNarrowLayout(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newNarrowLayoutAdapter(t)
	if err := runMBT(t, "narrow_layout", a, narrowLayoutActions, narrowLayoutOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkNarrowLayoutHistories(t, a)
}

// The runner picks actions uniformly, disabled ones included, and a walk
// ends at its first disabled one; with twenty actions and one or two
// enabled at a time, a random walk almost never gets a session onto the
// list and finishes a turn there (3000 walks ran none). So the same
// adapter also walks every path in testdata/narrow_layout/paths.json,
// the paths the browser stage walks, which together take every
// transition: each action must be enabled in the adapter's view where
// the graph enables it, and the state after it must be the graph's.
func TestNarrowLayoutPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newNarrowLayoutAdapter(t)
	if err := walkNarrowLayoutPaths(t, a); err != nil {
		t.Fatal(err)
	}
	checkNarrowLayoutHistories(t, a)
}

// checkNarrowLayoutHistories replays every transcript the walks wrote on
// the spec's graph.
func checkNarrowLayoutHistories(t *testing.T, a *narrowLayoutAdapter) {
	t.Helper()
	g, err := tracecheck.Load(fizzCheck(t, "narrow_layout"))
	if err != nil {
		t.Fatal(err)
	}
	turns := 0
	for _, id := range a.ids {
		entries := sessionHistory(t, a.s.Home, id)
		for _, e := range entries {
			if e.Kind == "input" {
				turns++
			}
		}
		checkHistory(t, g, entries, narrowLayoutHistory)
	}
	t.Logf("%d sessions, %d turns replayed", len(a.ids), turns)
}

// walkNarrowLayoutPaths drives a through every generated path and returns
// the first step whose action the adapter did not enable or whose state
// is not the path's.
func walkNarrowLayoutPaths(t *testing.T, a *narrowLayoutAdapter) error {
	t.Helper()
	b, err := pathsJSON("narrow_layout")
	if err != nil {
		return err
	}
	var file struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &file); err != nil {
		return err
	}
	turns := 0
	for pi, p := range file.Paths {
		for si, step := range p.Trace {
			if si == 0 {
				if err := a.Init(); err != nil {
					return fmt.Errorf("path %d init: %w", pi, err)
				}
			} else {
				name := strings.TrimPrefix(step.Action, "Phone#0.")
				f, ok := narrowLayoutActions["Phone"][name]
				if !ok {
					return fmt.Errorf("path %d step %d: no adapter action for %q", pi, si, step.Action)
				}
				if name == "Finish" {
					turns++
				}
				if _, err := f(a, nil); err != nil {
					return fmt.Errorf("path %d step %d (%s): %w", pi, si, name, err)
				}
				if a.gate.off {
					return fmt.Errorf("path %d step %d: %s is enabled in the model but not in the adapter's view", pi, si, name)
				}
			}
			got, err := a.GetState()
			if err != nil {
				return fmt.Errorf("path %d step %d: %w", pi, si, err)
			}
			for k, v := range step.State {
				field, ok := strings.CutPrefix(k, "Phone#0.")
				if !ok {
					continue
				}
				if !reflect.DeepEqual(got[field], v) {
					return fmt.Errorf("path %d step %d (%s): %s is %v, the model says %v", pi, si, step.Action, field, got[field], v)
				}
			}
		}
		if err := a.Cleanup(); err != nil {
			return err
		}
	}
	if turns == 0 {
		return fmt.Errorf("%d paths ran no turn", len(file.Paths))
	}
	return nil
}

// The runs above prove nothing unless an adapter that breaks the model
// fails them. The runner's random walks reach a new session every few
// dozen walks, so the bug they must catch is one a second walk trips
// over at once: Init leaving the last walk's session listed.
func TestNarrowLayoutCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newNarrowLayoutAdapter(t)
	a.keepLast = true
	if err := runMBT(t, "narrow_layout", a, narrowLayoutActions, narrowLayoutOptions()); err == nil {
		t.Fatal("a run whose Init leaves a session listed passed; the runner is not checking state")
	}
}

// A page that acks a finish while only the list shows (the desktop's
// "selected is viewing") must fail the path walk: the paths take Finish
// with a session still selected under the list.
func TestNarrowLayoutPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newNarrowLayoutAdapter(t)
	a.ackOnFinish = true
	err := walkNarrowLayoutPaths(t, a)
	if err == nil {
		t.Fatal("a page that acks a finish under the list walked every path; the walk is not checking state")
	}
	t.Logf("caught: %v", err)
}
