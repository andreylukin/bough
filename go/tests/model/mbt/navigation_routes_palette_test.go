//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
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

// specs/navigation_routes_palette.fizz at the server level: one tab's
// routes, Back, arrival and palette against a real serve.
//
// Most of the spec's state is the page's own (the hash, what one Back
// lands on, the palette and the sheet): nothing on the server can see
// it, so the adapter keeps it as the page would. What the server
// decides, it reads off the server, every time a page would:
//
//   - unseen is S's row. The adapter acks exactly when the page's ack
//     effect would (S's transcript on screen), so what is checked is
//     that nothing else — a lookup, a Context read, a list read, a
//     reload — clears it, and that the ack does.
//   - page, wherever a page renders a server read: S's transcript and
//     its Context page, the project list, the lookup of a session the
//     list does not hold (200 / 404 / other), and a gone project slug
//     (404, "not found", rather than an error with a Retry).
//   - the arrival pick is taken from GET /api/sessions with the page's
//     own rule (arrivalPick in app.tsx), so S must be listed, and the
//     archived X must not be.
//
// The four places where the spec is ahead of the product (push vs
// replace, the palette's default highlight, the gone project's Retry)
// live in the page, not the server; this level cannot see them and the
// browser flow is where they go red.
type navAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	// The fixture, made once per serve: S is the one listed session,
	// with an unseen finish; the rest are sessions the list does not
	// hold, one per lookup outcome.
	sid     string // S
	xFound  string // archived: its transcript still loads
	xBroken string // listed once, then its transcript made unreadable: a 500
	xGone   string // an id this server never had: a 404
	broken  string // xBroken's history file

	hash, page, back, overlay, sel string
	arrived                        bool

	turn int // turn names are unique across walks: the queue is shared

	// ackUnderContext is the deliberate bug
	// TestNavigationRoutesPaletteCatchesWrongAdapter injects: the page
	// acks S while only its Context page is on screen.
	ackUnderContext bool
}

// The slug no project has. It is a valid slug, so the server has to
// say "no such project" rather than "bad link".
const navGoneSlug = "no-such-project"

func newNavAdapter(t *testing.T) *navAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &navAdapter{t: t, s: s, dir: control.Dir(s.Home), xGone: "01999999-0000-7000-8000-000000000000"}
	ctx, cancel := context.WithTimeout(context.Background(), 2*actionTimeout)
	defer cancel()
	work := s.Dir(t, "work")
	for _, id := range []*string{&a.xFound, &a.xBroken, &a.sid} {
		row, err := s.CreateSession(ctx, work, "")
		if err != nil {
			t.Fatal(err)
		}
		*id = row.ID
	}
	for _, id := range []string{a.xFound, a.xBroken} {
		if _, err := s.Archive(ctx, id); err != nil {
			t.Fatal(err)
		}
	}
	// The list caches a transcript by size and mtime, and chmod moves
	// neither: xBroken stays known to the server while reading it
	// fails, which is a lookup that is neither found nor not found.
	a.broken = filepath.Join(s.Home, ".bough", "history", a.xBroken+".jsonl")
	if _, _, err := s.GetSession(ctx, a.xBroken); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(a.broken, 0); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.Chmod(a.broken, 0o644) })
	if _, _, err := s.GetSession(ctx, a.xBroken); status(err) != http.StatusInternalServerError {
		t.Fatalf("fixture: an unreadable transcript should read as a 500, got %v", err)
	}
	return a
}

// Init is a fresh tab on #/ with S's finish unseen: a walk that saw it
// gets a new one, a turn that answers at once.
func (a *navAdapter) Init() error {
	a.hash, a.page, a.back, a.overlay, a.sel, a.arrived = "home", "overview", "none", "none", "", false
	a.gate.reset()
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.sid)
	if err != nil || row.Unseen {
		return err
	}
	a.turn++
	name := fmt.Sprintf("n%04d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "ok", Text: "finished " + name})
	if err := a.s.Prompt(ctx, a.sid, "turn "+name); err != nil {
		return err
	}
	_, err = waitRow(a.s, a.sid, "an unseen finish", func(r serve.Row) bool {
		return r.Status == serve.StatusDone && r.Unseen
	})
	return err
}

func (a *navAdapter) Cleanup() error { return nil }

func (a *navAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Nav", Index: 0}: a}, nil
}

// GetState is the Nav role: unseen from S's row, the rest as the page
// holds it.
func (a *navAdapter) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.sid)
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"hash": a.hash, "page": a.page, "back": a.back, "overlay": a.overlay,
		"sel": a.sel, "unseen": row.Unseen, "arrived": a.arrived,
	}, nil
}

// status is an API error's HTTP status, 0 for none, -1 for a failure
// that never got an answer.
func status(err error) int {
	var ae *servetest.APIError
	switch {
	case err == nil:
		return 0
	case errors.As(err, &ae):
		return ae.Status
	}
	return -1
}

// get is a GET the page makes that servetest has no helper for.
func (a *navAdapter) get(path string) (int, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, a.s.URL+path, nil)
	if err != nil {
		return 0, err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, err
	}
	resp.Body.Close()
	return resp.StatusCode, nil
}

// lookup is the page reading one session: its transcript, "Session not
// found" on a 404 (only an authoritative 404 says a session is gone),
// else "Couldn't load" with a Retry.
func (a *navAdapter) lookup(id, found string) (string, serve.Row, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, id)
	switch st := status(err); {
	case st == 0:
		return found, row, nil
	case st == http.StatusNotFound:
		return "missing", row, nil
	case st > 0:
		return "loadfail", row, nil
	}
	return "", row, err
}

// showSession puts S's transcript up, and acks its finish the way the
// page's ack effect does once it is on screen.
func (a *navAdapter) showSession() (string, error) {
	page, row, err := a.lookup(a.sid, "session")
	if err != nil || page != "session" || !row.Unseen {
		return page, err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, err = a.s.Ack(ctx, a.sid)
	return page, err
}

// showSub is S's Context page: its own read, and no ack.
func (a *navAdapter) showSub() (string, error) {
	st, err := a.get("/api/sessions/" + a.sid + "/context")
	if err != nil {
		return "", err
	}
	if st != http.StatusOK {
		return fmt.Sprintf("sub_failed_%d", st), nil
	}
	if a.ackUnderContext {
		ctx, cancel := actionCtx()
		defer cancel()
		if _, err := a.s.Ack(ctx, a.sid); err != nil {
			return "", err
		}
	}
	return "sub", nil
}

func (a *navAdapter) showProjects() (string, error) {
	st, err := a.get("/api/projects")
	if err != nil {
		return "", err
	}
	if st != http.StatusOK {
		return fmt.Sprintf("projects_failed_%d", st), nil
	}
	return "projects", nil
}

// showGoneProject is the project page reading a slug nothing has: a
// 404 is "not found"; anything else is the error with a Retry.
func (a *navAdapter) showGoneProject() (string, error) {
	st, err := a.get("/api/projects/" + navGoneSlug)
	if err != nil {
		return "", err
	}
	if st == http.StatusNotFound {
		return "project_missing", nil
	}
	return fmt.Sprintf("project_error_%d", st), nil
}

// land is what a fresh read of the hash puts up (the spec's landing):
// the router reads only the hash.
func (a *navAdapter) land(h string) (string, error) {
	switch h {
	case "home":
		return "overview", nil
	case "s":
		return a.showSession()
	case "sub":
		return a.showSub()
	case "other":
		return "loading", nil
	case "projects":
		return a.showProjects()
	case "project_gone":
		return a.showGoneProject()
	}
	return "lost", nil
}

// push is a move that makes a history entry.
func (a *navAdapter) push(h string) error {
	page, err := a.land(h)
	if err != nil {
		return err
	}
	a.back, a.hash, a.page, a.arrived = a.hash, h, page, true
	return nil
}

// arrivalPick mirrors app.tsx's: the listed, non-empty, foreground
// sessions of the last day. The fixture lists S alone, so the page's
// ordering among several (signal, then recency) never decides here;
// more than one candidate means the list holds a session it should not.
func arrivalPick(rows []serve.Row, now time.Time) (string, error) {
	var ids []string
	for _, r := range rows {
		if !r.Archived && !r.Empty && !r.Background && now.Sub(r.LastAt) < 24*time.Hour {
			ids = append(ids, r.ID)
		}
	}
	switch len(ids) {
	case 0:
		return "", nil
	case 1:
		return ids[0], nil
	}
	return "", fmt.Errorf("arrival: the list holds %v; only S should be a candidate", ids)
}

// --- system ---

func (a *navAdapter) ArriveFast() error {
	if !a.gate.pass(!a.arrived && a.hash == "home" && a.page == "overview") {
		return nil
	}
	a.arrived = true
	ctx, cancel := actionCtx()
	defer cancel()
	rows, err := a.s.ListSessions(ctx, false)
	if err != nil {
		return err
	}
	pick, err := arrivalPick(rows, time.Now())
	if err != nil || pick == "" {
		return err
	}
	if pick != a.sid {
		return fmt.Errorf("arrival picked %s, not S (%s)", pick, a.sid)
	}
	// replaceState: the arrival pick is not a history entry.
	page, err := a.showSession()
	a.hash, a.page = "s", page
	return err
}

func (a *navAdapter) ArriveLate() error {
	if a.gate.pass(!a.arrived) {
		a.arrived = true
	}
	return nil
}

func (a *navAdapter) settle(id, found string) error {
	if !a.gate.pass(a.page == "loading") {
		return nil
	}
	page, _, err := a.lookup(id, found)
	a.page = page
	return err
}

func (a *navAdapter) Found() error      { return a.settle(a.xFound, "other_session") }
func (a *navAdapter) NotFound() error   { return a.settle(a.xGone, "other_session") }
func (a *navAdapter) LoadFailed() error { return a.settle(a.xBroken, "other_session") }

// --- the person: pushes ---

func (a *navAdapter) backIn(hs ...string) bool {
	for _, h := range hs {
		if a.back == h {
			return true
		}
	}
	return false
}

func (a *navAdapter) ClickRow() error {
	if !a.gate.pass(a.overlay == "none" && (a.page == "overview" || a.page == "projects") && a.backIn("none", "home", "s")) {
		return nil
	}
	return a.push("s")
}

func (a *navAdapter) GoHome() error {
	if !a.gate.pass(a.overlay == "none" && a.page != "overview" && a.backIn("none", "home", "s")) {
		return nil
	}
	return a.push("home")
}

func (a *navAdapter) NavProjects() error {
	if !a.gate.pass(a.overlay == "none" && (a.page == "overview" || a.page == "session") && a.backIn("none", "home", "s")) {
		return nil
	}
	return a.push("projects")
}

// A typed or followed link, from the overview with S one Back away.
func (a *navAdapter) openLink(h string) error {
	if !a.gate.pass(a.overlay == "none" && a.page == "overview" && a.back == "s") {
		return nil
	}
	return a.push(h)
}

func (a *navAdapter) OpenOther() error       { return a.openLink("other") }
func (a *navAdapter) OpenUnknown() error     { return a.openLink("unknown") }
func (a *navAdapter) OpenGoneProject() error { return a.openLink("project_gone") }

// OpenOldProject: the link is #/projects/<uuid>, which the page
// redirects to #/projects in place, so as a history entry it is the
// project list. Whether the redirect really replaces is the page's
// business (the browser flow); here it is the project list's read.
func (a *navAdapter) OpenOldProject() error { return a.openLink("projects") }

func (a *navAdapter) OpenSubLink() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.sid)
	if err != nil {
		return err
	}
	if !a.gate.pass(a.overlay == "none" && a.page == "overview" && row.Unseen) {
		return nil
	}
	return a.push("sub")
}

// --- the person: replaces ---

func (a *navAdapter) OpenContext() error {
	if !a.gate.pass(a.overlay == "none" && a.page == "session" && a.back == "home") {
		return nil
	}
	page, err := a.showSub()
	a.hash, a.page = "sub", page
	return err
}

func (a *navAdapter) CloseSub() error {
	if !a.gate.pass(a.overlay == "none" && a.page == "sub") {
		return nil
	}
	page, err := a.showSession()
	a.hash, a.page = "s", page
	return err
}

func (a *navAdapter) Retry() error {
	if a.gate.pass(a.overlay == "none" && a.page == "loadfail") {
		a.page = "loading"
	}
	return nil
}

// --- the browser ---

func (a *navAdapter) Back() error {
	if !a.gate.pass(a.overlay == "none" && a.back != "none") {
		return nil
	}
	page, err := a.land(a.back)
	a.hash, a.back, a.page, a.arrived = a.back, "none", page, true
	return err
}

func (a *navAdapter) Reload() error {
	if !a.gate.pass(a.overlay == "none" && a.back == "home") {
		return nil
	}
	page, err := a.land(a.hash)
	a.page = page
	return err
}

// --- the palette and the sheet: the page's own state ---

func (a *navAdapter) OpenPalette() error {
	if a.gate.pass(a.overlay == "none" && (a.page == "session" || a.page == "missing") && a.back == "home") {
		a.overlay, a.sel = "pal", "safe"
	}
	return nil
}

// TypeArchive: only a destructive row answers, so nothing is
// highlighted. The product highlights it anyway (palette.tsx); the
// highlight is in the page, which the browser flow reads.
func (a *navAdapter) TypeArchive() error {
	if a.gate.pass(a.overlay == "pal" && a.sel == "safe" && a.page == "session" && a.back == "home") {
		a.sel = "none"
	}
	return nil
}

func (a *navAdapter) ArrowToArchive() error {
	if a.gate.pass(a.overlay == "pal" && a.sel == "none") {
		a.sel = "chosen"
	}
	return nil
}

func (a *navAdapter) PalClose() error {
	if a.gate.pass(a.overlay == "pal") {
		a.overlay, a.sel = "none", ""
	}
	return nil
}

func (a *navAdapter) PalSession() error {
	if !a.gate.pass(a.overlay == "pal" && a.sel == "safe" && a.hash != "s") {
		return nil
	}
	a.overlay, a.sel = "none", ""
	return a.push("s")
}

func (a *navAdapter) PalProjects() error {
	if !a.gate.pass(a.overlay == "pal" && a.sel == "safe" && a.hash != "projects") {
		return nil
	}
	a.overlay, a.sel = "none", ""
	return a.push("projects")
}

func (a *navAdapter) PalShortcuts() error {
	if a.gate.pass(a.overlay == "pal" && a.sel == "safe" && a.page == "session" && a.back == "home") {
		a.overlay, a.sel = "sheet", ""
	}
	return nil
}

func (a *navAdapter) ShowShortcuts() error {
	if a.gate.pass(a.overlay == "none" && a.page == "session" && a.back == "home") {
		a.overlay = "sheet"
	}
	return nil
}

func (a *navAdapter) SheetClose() error {
	if a.gate.pass(a.overlay == "sheet") {
		a.overlay = "none"
	}
	return nil
}

var navigationRoutesPaletteActions = map[string]map[string]fmbt.ActionFunc{"Nav": {
	"ArriveFast":      action((*navAdapter).ArriveFast),
	"ArriveLate":      action((*navAdapter).ArriveLate),
	"Found":           action((*navAdapter).Found),
	"NotFound":        action((*navAdapter).NotFound),
	"LoadFailed":      action((*navAdapter).LoadFailed),
	"ClickRow":        action((*navAdapter).ClickRow),
	"GoHome":          action((*navAdapter).GoHome),
	"NavProjects":     action((*navAdapter).NavProjects),
	"OpenOther":       action((*navAdapter).OpenOther),
	"OpenUnknown":     action((*navAdapter).OpenUnknown),
	"OpenGoneProject": action((*navAdapter).OpenGoneProject),
	"OpenSubLink":     action((*navAdapter).OpenSubLink),
	"OpenOldProject":  action((*navAdapter).OpenOldProject),
	"OpenContext":     action((*navAdapter).OpenContext),
	"CloseSub":        action((*navAdapter).CloseSub),
	"Retry":           action((*navAdapter).Retry),
	"Back":            action((*navAdapter).Back),
	"Reload":          action((*navAdapter).Reload),
	"OpenPalette":     action((*navAdapter).OpenPalette),
	"TypeArchive":     action((*navAdapter).TypeArchive),
	"ArrowToArchive":  action((*navAdapter).ArrowToArchive),
	"PalClose":        action((*navAdapter).PalClose),
	"PalSession":      action((*navAdapter).PalSession),
	"PalProjects":     action((*navAdapter).PalProjects),
	"PalShortcuts":    action((*navAdapter).PalShortcuts),
	"ShowShortcuts":   action((*navAdapter).ShowShortcuts),
	"SheetClose":      action((*navAdapter).SheetClose),
}}

// Most steps are a read or two, and most walks end early on a disabled
// action (the runner picks from all 27), so many short walks cover
// more than a few long ones. Seedless, as every flow's options are.
func navigationRoutesPaletteOptions() map[string]any {
	return map[string]any{"max-seq-runs": 300, "max-actions": 8, "max-parallel-runs": 0}
}

// navigationRoutesPaletteHistory: navigation leaves nothing in a
// transcript, so what S's history can say is the one thing the spec
// starts from — S has a finish (Init's unseen). Every walk that saw it
// gave it a new one, so the transcript must end on a done.
func navigationRoutesPaletteHistory(entries []history.Entry) []tracecheck.Step {
	finished := false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			finished = false
		case "done":
			finished = true
		}
	}
	return []tracecheck.Step{{Action: "Init", State: map[string]any{"Nav#0.unseen": finished}}}
}

func init() { historyProjections["navigation_routes_palette"] = navigationRoutesPaletteHistory }

func TestNavigationRoutesPalette(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newNavAdapter(t)
	if err := runMBT(t, "navigation_routes_palette", a, navigationRoutesPaletteActions, navigationRoutesPaletteOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "navigation_routes_palette"))
	if err != nil {
		t.Fatal(err)
	}
	checkHistory(t, g, sessionHistory(t, a.s.Home, a.sid), navigationRoutesPaletteHistory)
}

// TestNavigationRoutesPalettePaths walks every generated path (the
// ones the browser flow walks) through the adapter, step by step. The
// runner's random walks mostly die on a disabled action among 27, so
// this is what makes sure every transition is taken against the server.
func TestNavigationRoutesPalettePaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	raw, err := os.ReadFile(filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "navigation_routes_palette", "paths.json"))
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		t.Fatal(err)
	}
	a := newNavAdapter(t)
	for i, p := range doc.Paths {
		if err := walkNavPath(a, p.Trace); err != nil {
			t.Errorf("path %d: %v", i, err)
		}
	}
	g, err := tracecheck.Load(fizzCheck(t, "navigation_routes_palette"))
	if err != nil {
		t.Fatal(err)
	}
	checkHistory(t, g, sessionHistory(t, a.s.Home, a.sid), navigationRoutesPaletteHistory)
}

// walkNavPath runs one path's actions and compares the role's state
// with the path's after every step.
func walkNavPath(a *navAdapter, trace []tracecheck.Step) error {
	for i, step := range trace {
		name := strings.TrimPrefix(step.Action, "Nav#0.")
		var err error
		if i == 0 {
			err = a.Init()
		} else if f, ok := navigationRoutesPaletteActions["Nav"][name]; !ok {
			return fmt.Errorf("step %d: no action %q", i, step.Action)
		} else {
			_, err = f(a, nil)
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, name, err)
		}
		if a.gate.off {
			return fmt.Errorf("step %d (%s): the adapter's require says disabled", i, name)
		}
		want := map[string]any{}
		for k, v := range step.State {
			if f, ok := strings.CutPrefix(k, "Nav#0."); ok {
				want[f] = v
			}
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, name, err)
		}
		if !reflect.DeepEqual(got, want) {
			return fmt.Errorf("step %d (%s): state\n got %v\nwant %v", i, name, got, want)
		}
	}
	return nil
}

// The run above proves nothing unless a server that breaks the model
// fails it. This adapter acks S while only its Context page is up —
// the ack effect reading "S is selected" as "S is on screen" — and
// the run must say so.
func TestNavigationRoutesPaletteCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newNavAdapter(t)
	a.ackUnderContext = true
	if err := runMBT(t, "navigation_routes_palette", a, navigationRoutesPaletteActions, navigationRoutesPaletteOptions()); err == nil {
		t.Fatal("a run that acks S under its Context page passed; the runner is not checking state")
	}
}
