//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/viewport_crossing.fizz at the server. Every field of the Vp role
// is the page's own (the width, the pane, the hash, the overlays, focus),
// which no API reports, so the adapter keeps it the way app.tsx does and
// the browser stage reads it off the DOM. What the server decides is read
// from it wherever the page would read it:
//
//   - a desktop reload at "#/" is arrival: the adapter runs app.tsx's
//     arrivalPick (navigation_routes_palette's) over GET /api/sessions, so selected and hash after that
//     Reload are the server's list, not the spec's promise. S must be a
//     session the pick takes: listed, not empty, recent.
//   - OpenSession clicks S's row, so S must be one the sidebar lists (an
//     empty session is left out unless it is open), and its transcript
//     loads (GET /api/sessions/{id}); so does every reload onto #/s/<id>.
//   - OpenPage and a reload onto #/me load GET /api/me.
//
// S is made once per serve with one finished turn: arrivalPick skips a
// session nobody has typed into.
type viewportCrossingAdapter struct {
	t    *testing.T
	s    *servetest.Server
	sid  string
	gate gate

	width, view, pane, hash, overlay, focus, last, prevHash, prevScreen string
	selected, closed, fitted, pushed                                    bool

	// paletteSurvives is the deliberate bug
	// TestViewportCrossingPathsCatchWrongAdapter injects: a palette that
	// does not listen to the (max-width:720px) query and stays open
	// across it.
	paletteSurvives bool
}

// newViewportCrossingAdapter starts a serve and makes S; turns false
// leaves S with no turn, the fixture TestViewportCrossingPathsNeedArrival
// uses.
func newViewportCrossingAdapter(t *testing.T, turns bool) *viewportCrossingAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &viewportCrossingAdapter{t: t, s: s}
	ctx, cancel := context.WithTimeout(context.Background(), 2*actionTimeout)
	defer cancel()
	row, err := s.CreateSession(ctx, s.Dir(t, "work"), "")
	if err != nil {
		t.Fatal(err)
	}
	a.sid = row.ID
	if !turns {
		return a
	}
	dir := control.Dir(s.Home)
	control.Queue(t, dir, "v0001", control.Turn{Mode: "ok", Text: "finished v0001"})
	if err := s.Prompt(ctx, a.sid, "turn v0001"); err != nil {
		t.Fatal(err)
	}
	if _, err := waitRow(s, a.sid, "S's first turn", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		t.Fatal(err)
	}
	return a
}

// Init is a desktop with S open at #/s/<id>. Nothing on the server
// changes during a walk, so there is nothing to undo there.
func (a *viewportCrossingAdapter) Init() error {
	a.width, a.view, a.pane, a.hash = "wide", "sessions", "thread", "session"
	a.selected, a.closed, a.overlay, a.fitted, a.focus = true, false, "none", true, "page"
	a.last, a.prevHash, a.pushed, a.prevScreen = "other", "", false, ""
	a.gate.reset()
	return a.load()
}

func (a *viewportCrossingAdapter) Cleanup() error { return nil }

func (a *viewportCrossingAdapter) GetState() (map[string]any, error) {
	return map[string]any{
		"width": a.width, "view": a.view, "selected": a.selected, "pane": a.pane,
		"hash": a.hash, "closed": a.closed, "overlay": a.overlay, "fitted": a.fitted,
		"focus": a.focus, "last": a.last, "prev_hash": a.prevHash, "pushed": a.pushed,
		"prev_screen": a.prevScreen,
	}, nil
}

// load is the transcript the page shows when S opens.
func (a *viewportCrossingAdapter) load() error {
	ctx, cancel := actionCtx()
	defer cancel()
	_, _, err := a.s.GetSession(ctx, a.sid)
	return err
}

// loadMe is the Me page's first read.
func (a *viewportCrossingAdapter) loadMe() error {
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, a.s.URL+"/api/me", nil)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		b, _ := io.ReadAll(resp.Body)
		return fmt.Errorf("GET /api/me: %d: %s", resp.StatusCode, strings.TrimSpace(string(b)))
	}
	return nil
}

func (a *viewportCrossingAdapter) rows() ([]serve.Row, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	return a.s.ListSessions(ctx, false)
}

// listed: the sidebar (and the phone's list) leaves out an empty session
// that is not live and not the open one (app.tsx's groups).
func (a *viewportCrossingAdapter) listed() bool {
	rows, err := a.rows()
	if err != nil {
		return false // a disabled action; the walk reports it
	}
	for _, r := range rows {
		if r.ID == a.sid {
			return !r.Archived && !(r.Empty && !r.Live && !a.selected)
		}
	}
	return false
}

func (a *viewportCrossingAdapter) screen() string {
	if a.width != "wide" {
		if a.pane == "list" {
			return "list"
		}
		return a.view
	}
	rail := "side"
	if a.closed {
		rail = "rail"
	}
	switch {
	case a.view == "page":
		return "desk-page-" + rail
	case a.selected:
		return "desk-session-" + rail
	}
	return "desk-overview-" + rail
}

func (a *viewportCrossingAdapter) onSession() bool {
	return a.view == "sessions" && a.selected && (a.width == "wide" || a.pane == "thread")
}

func (a *viewportCrossingAdapter) listVisible() bool {
	if a.width == "wide" {
		return !a.closed
	}
	return a.pane == "list"
}

func (a *viewportCrossingAdapter) other() {
	a.last, a.prevHash, a.pushed, a.prevScreen = "other", "", false, ""
}

// resize: the palette's closeOnNavigate listens to the 720 px query;
// Settings and the Work popover are the same element re-anchored, and
// their own listener refits them after (Refit); the rest is CSS.
func (a *viewportCrossingAdapter) resize(ok bool, w string) error {
	if !a.gate.pass(ok) {
		return nil
	}
	crossed := (a.width == "wide") != (w == "wide")
	a.width = w
	switch {
	case a.overlay == "palette" && crossed && !a.paletteSurvives:
		a.overlay, a.focus, a.fitted = "none", "page", true
	case a.overlay == "settings" || a.overlay == "work":
		a.fitted = false
	}
	a.last, a.prevHash, a.pushed, a.prevScreen = "resize", a.hash, false, ""
	return nil
}

func (a *viewportCrossingAdapter) ResizeNarrow() error { return a.resize(a.width == "wide", "phone") }
func (a *viewportCrossingAdapter) ResizeBelow480() error {
	return a.resize(a.width != "tiny", "tiny")
}
func (a *viewportCrossingAdapter) ResizeAbove480() error { return a.resize(a.width == "tiny", "phone") }
func (a *viewportCrossingAdapter) ResizeWide() error {
	return a.resize(a.width != "wide" && a.overlay != "details", "wide")
}

// goTo is a navigation that pushes the hash app.tsx writes for it.
func (a *viewportCrossingAdapter) goTo(view string, selected bool, pane string) {
	a.view, a.selected, a.pane = view, selected, pane
	switch {
	case view == "page":
		a.hash = "page"
	case selected:
		a.hash = "session"
	default:
		a.hash = "root"
	}
	a.last, a.prevHash, a.pushed, a.prevScreen = "other", "", true, ""
}

func (a *viewportCrossingAdapter) OpenSession() error {
	if !a.gate.pass(a.overlay == "none" && a.listVisible() && a.listed()) {
		return nil
	}
	a.goTo("sessions", true, "thread")
	return a.load()
}

func (a *viewportCrossingAdapter) Back() error {
	if a.gate.pass(a.overlay == "none" && a.width != "wide" && a.pane == "thread") {
		a.goTo("sessions", false, "list")
	}
	return nil
}

func (a *viewportCrossingAdapter) OpenPage() error {
	if !a.gate.pass(a.overlay == "none" && a.view == "sessions" && (a.width == "wide" || a.pane == "list")) {
		return nil
	}
	a.goTo("page", false, "thread")
	return a.loadMe()
}

func (a *viewportCrossingAdapter) SlashKey() error {
	if a.gate.pass(a.overlay == "none" && a.hash == "root") {
		a.closed, a.pane = false, "list"
		a.other()
	}
	return nil
}

func (a *viewportCrossingAdapter) CmdB() error {
	if a.gate.pass(a.overlay == "none") {
		a.closed = !a.closed
		a.other()
	}
	return nil
}

// Reload: the hash decides the view and the pane, and at "#/" on a
// desktop the server's list decides whether a session opens.
func (a *viewportCrossingAdapter) Reload() error {
	if !a.gate.pass(true) {
		return nil
	}
	a.prevScreen = a.screen()
	if a.hash == "root" && a.width == "wide" {
		rows, err := a.rows()
		if err != nil {
			return err
		}
		pick, err := arrivalPick(rows, time.Now())
		if err != nil {
			return err
		}
		if pick == a.sid {
			a.hash = "session"
		}
	}
	a.view = "sessions"
	if a.hash == "page" {
		a.view = "page"
	}
	a.selected = a.hash == "session"
	a.pane = "thread"
	if a.hash == "root" {
		a.pane = "list"
	}
	a.overlay, a.fitted, a.focus = "none", true, "page"
	a.last, a.prevHash, a.pushed = "reload", "", false
	switch {
	case a.selected:
		return a.load()
	case a.view == "page":
		return a.loadMe()
	}
	return nil
}

func (a *viewportCrossingAdapter) open(ok bool, o string) error {
	if a.gate.pass(ok) {
		a.overlay, a.fitted, a.focus = o, true, "overlay"
		a.other()
	}
	return nil
}

func (a *viewportCrossingAdapter) OpenSettings() error {
	return a.open(a.overlay == "none" && a.onSession(), "settings")
}

func (a *viewportCrossingAdapter) OpenWork() error {
	return a.open(a.overlay == "none" && a.onSession(), "work")
}

func (a *viewportCrossingAdapter) OpenDetails() error {
	return a.open(a.overlay == "none" && a.onSession() && a.width != "wide", "details")
}

func (a *viewportCrossingAdapter) OpenPalette() error {
	return a.open(a.overlay == "none", "palette")
}

func (a *viewportCrossingAdapter) CloseOverlay() error {
	if a.gate.pass(a.overlay != "none") {
		a.overlay, a.fitted, a.focus = "none", true, "page"
		a.other()
	}
	return nil
}

func (a *viewportCrossingAdapter) Refit() error {
	if a.gate.pass(a.overlay != "none" && !a.fitted) {
		a.fitted = true
	}
	return nil
}

var viewportCrossingActions = map[string]func(*viewportCrossingAdapter) error{
	"ResizeNarrow":   (*viewportCrossingAdapter).ResizeNarrow,
	"ResizeBelow480": (*viewportCrossingAdapter).ResizeBelow480,
	"ResizeAbove480": (*viewportCrossingAdapter).ResizeAbove480,
	"ResizeWide":     (*viewportCrossingAdapter).ResizeWide,
	"OpenSession":    (*viewportCrossingAdapter).OpenSession,
	"Back":           (*viewportCrossingAdapter).Back,
	"OpenPage":       (*viewportCrossingAdapter).OpenPage,
	"SlashKey":       (*viewportCrossingAdapter).SlashKey,
	"CmdB":           (*viewportCrossingAdapter).CmdB,
	"Reload":         (*viewportCrossingAdapter).Reload,
	"OpenSettings":   (*viewportCrossingAdapter).OpenSettings,
	"OpenWork":       (*viewportCrossingAdapter).OpenWork,
	"OpenDetails":    (*viewportCrossingAdapter).OpenDetails,
	"OpenPalette":    (*viewportCrossingAdapter).OpenPalette,
	"CloseOverlay":   (*viewportCrossingAdapter).CloseOverlay,
	"Refit":          (*viewportCrossingAdapter).Refit,
}

// viewportCrossingHistory reads the trace off S's transcript. Nothing in
// this flow writes one (a resize, a reload, an overlay never reach the
// model), so what the transcript can say is the thing the spec takes on
// trust: that arrival opens S. That needs S to have been typed into
// (arrivalPick skips an empty session). The projection takes the one
// short path to a desktop reload at "#/" (to a phone, Back to the list,
// widen, reload) and says S opened iff the transcript holds a finished
// turn. A second turn is one the flow sent, which no action does: it is
// projected as an action the spec does not have.
func viewportCrossingHistory(entries []history.Entry) []tracecheck.Step {
	inputs, done := 0, false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			inputs++
			done = false
		case "done":
			done = true
		}
	}
	opened := inputs > 0 && done
	hash := "root"
	if opened {
		hash = "session"
	}
	st := func(width string, selected bool, hash string) map[string]any {
		return map[string]any{"Vp#0.width": width, "Vp#0.selected": selected, "Vp#0.hash": hash}
	}
	steps := []tracecheck.Step{
		{Action: "Init", State: st("wide", true, "session")},
		{Action: "Vp#0.ResizeNarrow", State: st("phone", true, "session")},
		{Action: "Vp#0.Back", State: st("phone", false, "root")},
		{Action: "Vp#0.ResizeWide", State: st("wide", false, "root")},
		{Action: "Vp#0.Reload", State: st("wide", opened, hash)},
	}
	for i := 1; i < inputs; i++ {
		steps = append(steps, tracecheck.Step{Action: "Vp#0.Prompt"})
	}
	return steps
}

func init() { historyProjections["viewport_crossing"] = viewportCrossingHistory }

// TestViewportCrossingPaths walks the generated paths (every settled
// state; MODEL_COVER=transitions every link) through the adapter against
// one serve, then replays S's transcript on the graph.
func TestViewportCrossingPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newViewportCrossingAdapter(t, true)
	if err := walkViewportCrossingPaths(a, envCover()); err != nil {
		t.Fatal(err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "viewport_crossing"))
	if err != nil {
		t.Fatal(err)
	}
	checkHistory(t, g, sessionHistory(t, a.s.Home, a.sid), viewportCrossingHistory)
}

// walkViewportCrossingPaths returns the first step whose action the
// adapter did not enable or whose state is not the path's.
func walkViewportCrossingPaths(a *viewportCrossingAdapter, cover tracecheck.Cover) error {
	b, err := pathsJSONCover("viewport_crossing", cover)
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
	if len(file.Paths) == 0 {
		return fmt.Errorf("no paths")
	}
	for pi, p := range file.Paths {
		for si, step := range p.Trace {
			name := strings.TrimPrefix(step.Action, "Vp#0.")
			if si == 0 {
				err = a.Init()
			} else if f, ok := viewportCrossingActions[name]; !ok {
				return fmt.Errorf("path %d step %d: no adapter action for %q", pi, si, step.Action)
			} else {
				err = f(a)
			}
			if err != nil {
				return fmt.Errorf("path %d step %d (%s): %w", pi, si, name, err)
			}
			if a.gate.off {
				return fmt.Errorf("path %d step %d: %s is enabled in the model but not in the adapter's view", pi, si, name)
			}
			got, _ := a.GetState()
			want := map[string]any{}
			for k, v := range step.State {
				if f, ok := strings.CutPrefix(k, "Vp#0."); ok {
					want[f] = v
				}
			}
			if !reflect.DeepEqual(got, want) {
				return fmt.Errorf("path %d step %d (%s): state\n got %v\nwant %v", pi, si, name, got, want)
			}
		}
	}
	return nil
}

// The walk proves nothing unless an adapter that breaks the model fails
// it. A palette that stays open across 720 px shows only on a resize
// taken with the palette open, one transition a states walk need not
// take, so this walks every link.
func TestViewportCrossingPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newViewportCrossingAdapter(t, true)
	a.paletteSurvives = true
	err := walkViewportCrossingPaths(a, tracecheck.CoverTransitions)
	if err == nil {
		t.Fatal("a palette that survives the 720 px crossing walked every link; the walk is not checking state")
	}
	t.Logf("caught: %v", err)
}

// The server half: with S never typed into, the sidebar does not list it
// once it is closed and arrival does not pick it, so the walk must fail.
// Without this, a walk whose server reads were stubbed would pass too.
func TestViewportCrossingPathsNeedArrival(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newViewportCrossingAdapter(t, false)
	err := walkViewportCrossingPaths(a, tracecheck.CoverTransitions)
	if err == nil {
		t.Fatal("an empty S walked every link; the adapter is not reading the server")
	}
	t.Logf("caught: %v", err)

	g, lerr := tracecheck.Load(fizzCheck(t, "viewport_crossing"))
	if lerr != nil {
		t.Fatal(lerr)
	}
	if v := g.Check(viewportCrossingHistory(sessionHistory(t, a.s.Home, a.sid))); v == nil {
		t.Fatal("an empty transcript replayed as a path; the projection checks nothing")
	} else {
		t.Logf("history caught: %v", v)
	}
}
