//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/overlay_stack_navigation.fizz at the server: a modal up while
// the route moves under it. The layer, what it is about, its opener and
// focus are the page's own (DialogHost, the palette, the hash router):
// no API reports them, so the adapter keeps them the way the spec's
// contract says the page must, and the browser stage is where they are
// read off the DOM. What the server decides is read from it on every
// step:
//
//	page      the page's own read: GET /api/sessions/{id} for A, B or the
//	          session a create landed on (A only when the id is A's),
//	          GET /api/projects for the Projects page
//	archived  A's row
//	deleted   P not in GET /api/projects
//	creating  a POST /api/sessions the page sent whose answer it has not
//	          landed on yet
//
// and the server is driven the way the page drives it: Confirm posts
// the archive or the delete for the subject the dialog was opened about
// (archiveRow's row, GroupMenu's slug), not for whatever is on screen
// when it resolves; a Start is POST /api/sessions, answered in the
// background and landed on by CreateLands. So a walk checks that only a
// Confirm archives A or deletes P, that it archives nothing else (B and
// every created session stay listed), and that a create's session is
// readable where the page lands.
type overlayNavAdapter struct {
	t    *testing.T
	s    *servetest.Server
	cwd  string
	gate gate

	aID, bID, slug string   // the fixture: sessions A and B, project P
	created        []string // every session a create landed on

	// The page's state, as the spec's contract has it.
	pageID                        string // A's or a B's id, or "projects"
	backID, fwdID                 string // "" for no entry
	layer, aboutID, opener, focus string
	pending                       chan overlayCreate // the Start in flight

	// resolveOnRoute is the deliberate bug
	// TestOverlayStackNavigationPathsCatchWrongAdapter injects: a route
	// change resolves a DialogHost ask up as confirmed rather than
	// dismissed, archiving A (or deleting P) off screen.
	resolveOnRoute bool
}

type overlayCreate struct {
	row serve.Row
	err error
}

const overlayProjects = "projects"

func newOverlayNavAdapter(t *testing.T) *overlayNavAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig, Env: []string{"BOUGH_CONTAINER=none"}})
	a := &overlayNavAdapter{t: t, s: s, cwd: s.Dir(t, "work")}
	ctx, cancel := context.WithTimeout(context.Background(), 2*actionTimeout)
	defer cancel()
	for _, id := range []*string{&a.aID, &a.bID} {
		row, err := s.CreateSession(ctx, a.cwd, "")
		if err != nil {
			t.Fatal(err)
		}
		*id = row.ID
	}
	if err := a.makeProject(ctx); err != nil {
		t.Fatal(err)
	}
	return a
}

func (a *overlayNavAdapter) makeProject(ctx context.Context) error {
	var r struct {
		Project serve.Project `json:"project"`
	}
	if err := apiCall(ctx, a.s, http.MethodPost, "/api/projects", map[string]string{"name": "P"}, &r); err != nil {
		return fmt.Errorf("create project P: %w", err)
	}
	a.slug = r.Project.Slug
	return nil
}

// Init is a fresh tab on A's thread with the Projects page one Back
// away: A listed, P on disk, nothing up, no create in flight.
func (a *overlayNavAdapter) Init() error {
	if err := a.Cleanup(); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	archived, deleted, err := a.server(ctx)
	if err != nil {
		return err
	}
	if archived {
		if _, err := a.s.Unarchive(ctx, a.aID); err != nil {
			return err
		}
	}
	if deleted {
		if err := a.makeProject(ctx); err != nil {
			return err
		}
	}
	a.pageID, a.backID, a.fwdID = a.aID, overlayProjects, ""
	a.layer, a.aboutID, a.opener, a.focus = "", "", "", "page"
	a.gate.reset()
	return nil
}

// Cleanup lands a create a walk left in flight, so its answer does not
// leak into the next walk.
func (a *overlayNavAdapter) Cleanup() error {
	if a.pending == nil {
		return nil
	}
	r := <-a.pending
	a.pending = nil
	if r.err == nil {
		a.created = append(a.created, r.row.ID)
	}
	return r.err
}

func (a *overlayNavAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Page", Index: 0}: a}, nil
}

// server reads A's archived and P's deleted, and fails when anything
// but A was archived: a Confirm acts on its subject alone.
func (a *overlayNavAdapter) server(ctx context.Context) (archived, deleted bool, err error) {
	rows, err := a.s.ListSessions(ctx, true)
	if err != nil {
		return false, false, err
	}
	seen := false
	for _, r := range rows {
		switch {
		case r.ID == a.aID:
			seen, archived = true, r.Archived
		case r.Archived:
			return false, false, fmt.Errorf("session %s was archived; only A (%s) is ever confirmed", r.ID, a.aID)
		}
	}
	if !seen {
		return false, false, fmt.Errorf("A (%s) is not listed", a.aID)
	}
	var list struct {
		Projects []serve.Project `json:"projects"`
	}
	if err := apiCall(ctx, a.s, http.MethodGet, "/api/projects", nil, &list); err != nil {
		return false, false, err
	}
	deleted = true
	for _, p := range list.Projects {
		if p.Slug != a.slug {
			return false, false, fmt.Errorf("unexpected project %q listed", p.Slug)
		}
		deleted = false
	}
	return archived, deleted, nil
}

// page is what the page's own read of pageID puts up.
func (a *overlayNavAdapter) page(ctx context.Context) (string, error) {
	if a.pageID == overlayProjects {
		if err := apiCall(ctx, a.s, http.MethodGet, "/api/projects", nil, nil); err != nil {
			return "", fmt.Errorf("the Projects page's read: %w", err)
		}
		return "projects", nil
	}
	row, _, err := a.s.GetSession(ctx, a.pageID)
	if err != nil {
		return "", fmt.Errorf("the thread's read of %s: %w", a.pageID, err)
	}
	if row.ID == a.aID {
		return "A", nil
	}
	return "B", nil
}

// name is the spec's name for an id the page holds.
func (a *overlayNavAdapter) name(id string) string {
	switch id {
	case "", overlayProjects:
		return id
	case a.aID:
		return "A"
	}
	return "B"
}

func (a *overlayNavAdapter) about() string {
	switch a.aboutID {
	case "":
		return ""
	case a.aID:
		return "A"
	case a.slug:
		return "P"
	}
	return "?" + a.aboutID
}

func (a *overlayNavAdapter) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	archived, deleted, err := a.server(ctx)
	if err != nil {
		return nil, err
	}
	page, err := a.page(ctx)
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"page": page, "back": a.name(a.backID), "fwd": a.name(a.fwdID),
		"layer": a.layer, "about": a.about(), "opener": a.opener, "focus": a.focus,
		"creating": a.pending != nil, "archived": archived, "deleted": deleted,
		// lost: the page's ⌘K guard keeps a second ask from being pushed,
		// so no ask the adapter makes is ever dropped.
		"lost": false,
	}, nil
}

// view is GetState's server half, for requires.
func (a *overlayNavAdapter) view() (page string, archived, deleted bool) {
	ctx, cancel := actionCtx()
	defer cancel()
	archived, deleted, err := a.server(ctx)
	if err != nil {
		a.gate.pass(false)
		return "", false, false
	}
	page, err = a.page(ctx)
	if err != nil {
		a.gate.pass(false)
	}
	return page, archived, deleted
}

func isDialog(layer string) bool { return layer == "keys" || layer == "archive" || layer == "delete" }

// closeLayer is a layer going: focus to its opener, .app if it is gone.
func (a *overlayNavAdapter) closeLayer() {
	a.focus = a.opener
	if a.opener == "gone" {
		a.focus = "app"
	}
	a.layer, a.aboutID, a.opener = "", "", ""
}

// route is a route change with anything up: the layer is dismissed (a
// DialogHost ask resolving as dismissed) and focus goes to .app.
func (a *overlayNavAdapter) route() error {
	if a.resolveOnRoute && (a.layer == "archive" || a.layer == "delete") {
		if err := a.confirm(); err != nil {
			return err
		}
	}
	a.layer, a.aboutID, a.opener, a.focus = "", "", "", "app"
	return nil
}

// confirm resolves the ask up as OK: it acts on the subject it was
// opened about.
func (a *overlayNavAdapter) confirm() error {
	ctx, cancel := actionCtx()
	defer cancel()
	if a.layer == "archive" {
		_, err := a.s.Archive(ctx, a.aboutID)
		return err
	}
	return apiCall(ctx, a.s, http.MethodDelete, "/api/projects/"+a.aboutID, nil, nil)
}

func (a *overlayNavAdapter) push(id string) {
	a.backID, a.fwdID, a.pageID = a.pageID, "", id
}

// --- opening a layer ---

func (a *overlayNavAdapter) ArchiveMenu() error {
	page, archived, _ := a.view()
	if a.gate.pass(page == "A" && a.layer == "" && !archived) {
		a.layer, a.aboutID, a.opener, a.focus = "archive", a.pageID, "page", "layer"
	}
	return nil
}

func (a *overlayNavAdapter) DeleteMenu() error {
	page, _, deleted := a.view()
	if a.gate.pass(page == "projects" && a.layer == "" && !deleted) {
		a.layer, a.aboutID, a.opener, a.focus = "delete", a.slug, "gone", "layer"
	}
	return nil
}

func (a *overlayNavAdapter) open(layer string) error {
	if a.gate.pass(a.layer == "") {
		a.layer, a.opener, a.focus = layer, a.focus, "layer"
	}
	return nil
}

func (a *overlayNavAdapter) CmdK() error     { return a.open("palette") }
func (a *overlayNavAdapter) Question() error { return a.open("keys") }

// ⌘K, "/" and ? under a DialogHost dialog do nothing.
func (a *overlayNavAdapter) underDialog() error {
	a.gate.pass(isDialog(a.layer))
	return nil
}

func (a *overlayNavAdapter) CmdKUnderDialog() error     { return a.underDialog() }
func (a *overlayNavAdapter) SlashUnderDialog() error    { return a.underDialog() }
func (a *overlayNavAdapter) QuestionUnderDialog() error { return a.underDialog() }

// --- palette picks: after the palette closed, its opener nulled ---

func (a *overlayNavAdapter) PickArchive() error {
	page, archived, _ := a.view()
	if a.gate.pass(a.layer == "palette" && page == "A" && !archived) {
		a.layer, a.aboutID, a.opener, a.focus = "archive", a.pageID, "gone", "layer"
	}
	return nil
}

func (a *overlayNavAdapter) PickKeys() error {
	if a.gate.pass(a.layer == "palette") {
		a.layer, a.aboutID, a.opener, a.focus = "keys", "", "gone", "layer"
	}
	return nil
}

// PickStart sends the create; its answer is landed on by CreateLands.
func (a *overlayNavAdapter) PickStart() error {
	if !a.gate.pass(a.layer == "palette" && a.pending == nil) {
		return nil
	}
	a.layer, a.opener, a.focus = "", "", "body"
	ch := make(chan overlayCreate, 1)
	a.pending = ch
	go func() {
		ctx, cancel := actionCtx()
		defer cancel()
		row, err := a.s.CreateSession(ctx, a.cwd, "")
		ch <- overlayCreate{row, err}
	}()
	return nil
}

func (a *overlayNavAdapter) PickProjects() error {
	page, _, _ := a.view()
	if a.gate.pass(a.layer == "palette" && page != "projects") {
		a.push(overlayProjects)
		a.layer, a.opener, a.focus = "", "", "app"
	}
	return nil
}

func (a *overlayNavAdapter) Resize() error {
	if a.gate.pass(a.layer == "palette") {
		a.closeLayer()
	}
	return nil
}

func (a *overlayNavAdapter) Escape() error {
	if a.gate.pass(a.layer != "") {
		a.closeLayer()
	}
	return nil
}

func (a *overlayNavAdapter) Confirm() error {
	if !a.gate.pass(a.layer == "archive" || a.layer == "delete") {
		return nil
	}
	if err := a.confirm(); err != nil {
		return err
	}
	a.closeLayer()
	return nil
}

// --- routes ---

func (a *overlayNavAdapter) goTo(page, id string) error {
	now, _, _ := a.view()
	if a.gate.pass(a.layer == "" && now != page) {
		a.push(id)
		a.focus = "page"
	}
	return nil
}

func (a *overlayNavAdapter) GoProjects() error { return a.goTo("projects", overlayProjects) }
func (a *overlayNavAdapter) GoA() error        { return a.goTo("A", a.aID) }
func (a *overlayNavAdapter) GoB() error        { return a.goTo("B", a.bID) }

func (a *overlayNavAdapter) Back() error {
	if !a.gate.pass(a.backID != "") {
		return nil
	}
	a.fwdID, a.pageID, a.backID = a.pageID, a.backID, ""
	return a.route()
}

func (a *overlayNavAdapter) Forward() error {
	if !a.gate.pass(a.fwdID != "") {
		return nil
	}
	a.backID, a.pageID, a.fwdID = a.pageID, a.fwdID, ""
	return a.route()
}

// CreateLands is the create's answer: the new session opens by
// pushState, and whatever is up is dismissed.
func (a *overlayNavAdapter) CreateLands() error {
	if !a.gate.pass(a.pending != nil) {
		return nil
	}
	r := <-a.pending
	a.pending = nil
	if r.err != nil {
		return fmt.Errorf("the create: %w", r.err)
	}
	a.created = append(a.created, r.row.ID)
	if a.name(a.pageID) != "B" {
		a.push(r.row.ID)
	} else {
		a.pageID = r.row.ID
	}
	if a.layer == "" {
		return nil
	}
	return a.route()
}

var overlayStackNavigationActions = map[string]map[string]fmbt.ActionFunc{"Page": {
	"ArchiveMenu":         action((*overlayNavAdapter).ArchiveMenu),
	"DeleteMenu":          action((*overlayNavAdapter).DeleteMenu),
	"CmdK":                action((*overlayNavAdapter).CmdK),
	"Question":            action((*overlayNavAdapter).Question),
	"CmdKUnderDialog":     action((*overlayNavAdapter).CmdKUnderDialog),
	"SlashUnderDialog":    action((*overlayNavAdapter).SlashUnderDialog),
	"QuestionUnderDialog": action((*overlayNavAdapter).QuestionUnderDialog),
	"PickArchive":         action((*overlayNavAdapter).PickArchive),
	"PickKeys":            action((*overlayNavAdapter).PickKeys),
	"PickStart":           action((*overlayNavAdapter).PickStart),
	"PickProjects":        action((*overlayNavAdapter).PickProjects),
	"Resize":              action((*overlayNavAdapter).Resize),
	"Escape":              action((*overlayNavAdapter).Escape),
	"Confirm":             action((*overlayNavAdapter).Confirm),
	"GoProjects":          action((*overlayNavAdapter).GoProjects),
	"GoA":                 action((*overlayNavAdapter).GoA),
	"GoB":                 action((*overlayNavAdapter).GoB),
	"Back":                action((*overlayNavAdapter).Back),
	"Forward":             action((*overlayNavAdapter).Forward),
	"CreateLands":         action((*overlayNavAdapter).CreateLands),
}}

// overlayStackNavigationHistory: nothing in this flow is a turn (a
// Start sends no prompt; archive and delete live in serve's metadata,
// not the transcript), so what a transcript can say is that it has
// none. It is Init, then one step per input with an action the spec does
// not have, which no graph path takes.
func overlayStackNavigationHistory(entries []history.Entry) []tracecheck.Step {
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Page#0.creating": false, "Page#0.lost": false}}}
	for _, e := range entries {
		if e.Kind == "input" {
			steps = append(steps, tracecheck.Step{Action: "Page#0.Prompt"})
		}
	}
	return steps
}

func init() { historyProjections["overlay_stack_navigation"] = overlayStackNavigationHistory }

func overlayStackNavigationGraph(t *testing.T) *tracecheck.Graph {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("overlay_stack_navigation")), "..", "testdata", "overlay_stack_navigation"))
	if err != nil {
		t.Fatal(err)
	}
	return g
}

// TestOverlayStackNavigationPaths walks the checked-in graph's paths
// (every settled state; MODEL_COVER=transitions takes every link)
// through the adapter against one real serve, then replays every
// transcript the walks left.
func TestOverlayStackNavigationPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newOverlayNavAdapter(t)
	if err := walkOverlayStackNavigation(a, envCover()); err != nil {
		t.Fatal(err)
	}
	if len(a.created) == 0 {
		t.Fatal("no walk landed a create")
	}
	g := overlayStackNavigationGraph(t)
	for _, id := range append([]string{a.aID, a.bID}, a.created...) {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), overlayStackNavigationHistory)
	}
	t.Logf("%d creates landed", len(a.created))
}

// A route change that resolves the ask as confirmed archives A off its
// page: the walk must say so. It shows on Back or Forward out of the
// archive or delete confirm, which a walk reaching every state need not
// take, so this one takes every link.
func TestOverlayStackNavigationPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newOverlayNavAdapter(t)
	a.resolveOnRoute = true
	err := walkOverlayStackNavigation(a, tracecheck.CoverTransitions)
	if err == nil {
		t.Fatal("a route change that confirms the ask up walked every link; the walk is not checking state")
	}
	t.Logf("caught: %v", err)
}

// A transcript with a turn in it is not a path in this spec.
func TestOverlayStackNavigationHistoryRejectsTurn(t *testing.T) {
	t.Parallel()
	g := overlayStackNavigationGraph(t)
	if v := g.Check(overlayStackNavigationHistory(nil)); v != nil {
		t.Fatalf("an empty transcript: %v", v)
	}
	if v := g.Check(overlayStackNavigationHistory([]history.Entry{{Kind: "input"}, {Kind: "done"}})); v == nil {
		t.Fatal("a transcript with a turn replayed as a path")
	}
}

// walkOverlayStackNavigation drives a through every path of cover and
// returns the first step the adapter did not enable or whose state is
// not the path's.
func walkOverlayStackNavigation(a *overlayNavAdapter, cover tracecheck.Cover) error {
	raw, err := pathsJSONCover("overlay_stack_navigation", cover)
	if err != nil {
		return err
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		return err
	}
	actions := overlayStackNavigationActions["Page"]
	for pi, p := range doc.Paths {
		for si, step := range p.Trace {
			name := strings.TrimPrefix(step.Action, "Page#0.")
			if si == 0 {
				if err := a.Init(); err != nil {
					return fmt.Errorf("path %d init: %w", pi, err)
				}
			} else if f, ok := actions[name]; !ok {
				return fmt.Errorf("path %d step %d: no adapter action for %q", pi, si, step.Action)
			} else if _, err := f(a, nil); err != nil {
				return fmt.Errorf("path %d step %d (%s): %w", pi, si, name, err)
			} else if a.gate.off {
				return fmt.Errorf("path %d step %d: %s is enabled in the model but not in the adapter's view", pi, si, name)
			}
			got, err := a.GetState()
			if err != nil {
				return fmt.Errorf("path %d step %d (%s): %w", pi, si, name, err)
			}
			want := map[string]any{}
			for k, v := range step.State {
				if f, ok := strings.CutPrefix(k, "Page#0."); ok {
					want[f] = v
				}
			}
			if !reflect.DeepEqual(got, want) {
				return fmt.Errorf("path %d step %d (%s): state\n got %v\nwant %v", pi, si, name, got, want)
			}
		}
	}
	return a.Cleanup()
}
