//go:build !windows

package mbt

import (
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/search_scope_vs_project_deletion.fizz against a real serve: one
// session, filed into one project, that a full-text hit keeps pointing
// at while the project is deleted (and its slug possibly reused)
// underneath it.
//
// A query is one opaque round trip to /api/search for a word this
// walk's fixture session said; the label a hit carries is read off the
// session's row (its Project field) at the moment the query answers,
// the same thing app.tsx's search surfaces read to name a hit's
// project. DeleteProject only unassigns the session (AGENTS.md), so
// OpenHit — GetSession on the id the hit named — must keep succeeding
// whatever disk says.
type searchScopeVsProjectDeletionAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	n    int
	id   string // this walk's session
	slug string // this walk's project
	word string // the unique word the session's reply says

	query, label, disk, opened string

	// labelIgnoresDelete is the deliberate wiring bug
	// TestSearchScopeVsProjectDeletionCatchesWrongAdapter injects: the
	// label is read as "alpha" whenever the hit is found, never "gone".
	labelIgnoresDelete bool
}

func newSearchScopeVsProjectDeletionAdapter(t *testing.T) *searchScopeVsProjectDeletionAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &searchScopeVsProjectDeletionAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

// Init makes a fresh session, filed into a fresh project, with a reply
// that says a word unique to this walk so /api/search finds only it.
func (a *searchScopeVsProjectDeletionAdapter) Init() error {
	a.n++
	a.word = fmt.Sprintf("zerqol%d", a.n)
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.id = row.ID
	name := fmt.Sprintf("t%04d", a.n)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "ok", Text: "the " + a.word + " sighting"})
	if err := a.s.Prompt(ctx, a.id, "log a sighting"); err != nil {
		return err
	}
	if _, err := a.s.WaitSession(ctx, a.id, func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		return err
	}
	var r struct {
		Project serve.Project `json:"project"`
	}
	if err := a.s.API(ctx, "POST", "/api/projects", map[string]string{"name": fmt.Sprintf("Alpha %d", a.n)}, &r); err != nil {
		return err
	}
	a.slug = r.Project.Slug
	if err := a.s.API(ctx, "POST", "/api/sessions/"+a.id+"/project", map[string]string{"project": a.slug}, nil); err != nil {
		return err
	}
	a.query, a.label, a.disk, a.opened = "idle", "", "ok", ""
	a.gate.reset()
	return nil
}

func (a *searchScopeVsProjectDeletionAdapter) Cleanup() error { return nil }

func (a *searchScopeVsProjectDeletionAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Search", Index: 0}: a}, nil
}

func (a *searchScopeVsProjectDeletionAdapter) GetState() (map[string]any, error) {
	return map[string]any{"query": a.query, "label": a.label, "disk": a.disk, "opened": a.opened}, nil
}

func (a *searchScopeVsProjectDeletionAdapter) Ask() error {
	if a.gate.pass(a.query == "idle") {
		a.query = "loading"
	}
	return nil
}

func (a *searchScopeVsProjectDeletionAdapter) Reask() error {
	if a.gate.pass(a.query != "idle") {
		a.query, a.label = "loading", ""
	}
	return nil
}

// Answer settles the round trip: the hit's label is read off the
// session's row exactly as it stands right now, not as it stood when
// Ask fired.
func (a *searchScopeVsProjectDeletionAdapter) Answer() error {
	if !a.gate.pass(a.query == "loading") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	var d struct {
		Hits []serve.SearchHit `json:"hits"`
	}
	if err := a.s.API(ctx, "GET", "/api/search?q="+a.word, nil, &d); err != nil {
		return err
	}
	found := false
	for _, h := range d.Hits {
		if h.ID == a.id {
			found = true
		}
	}
	if !found {
		return fmt.Errorf("search for %q did not find the fixture session", a.word)
	}
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return err
	}
	if a.labelIgnoresDelete || row.Project == a.slug {
		a.label = "alpha"
	} else {
		a.label = "gone"
	}
	a.query = "shown"
	return nil
}

func (a *searchScopeVsProjectDeletionAdapter) DeleteProject() error {
	if !a.gate.pass(a.disk == "ok") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.API(ctx, "DELETE", "/api/projects/"+a.slug, nil, nil); err != nil {
		return err
	}
	a.disk = "none"
	return nil
}

func (a *searchScopeVsProjectDeletionAdapter) RecreateProject() error {
	if !a.gate.pass(a.disk == "none") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	var r struct {
		Project serve.Project `json:"project"`
	}
	// A new project of the same name gets the same slug back: the old
	// directory is gone, so nothing holds it.
	if err := a.s.API(ctx, "POST", "/api/projects", map[string]string{"name": fmt.Sprintf("Alpha %d", a.n)}, &r); err != nil {
		return err
	}
	if r.Project.Slug != a.slug {
		return fmt.Errorf("recreated project got slug %q, want the reused %q", r.Project.Slug, a.slug)
	}
	a.disk = "recreated"
	return nil
}

// OpenHit is the click-through: the session always opens, whatever the
// hit's stale label said.
func (a *searchScopeVsProjectDeletionAdapter) OpenHit() error {
	if !a.gate.pass(a.query == "shown") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if _, _, err := a.s.GetSession(ctx, a.id); err != nil {
		return fmt.Errorf("opening the hit's session: %w", err)
	}
	a.opened = "session"
	return nil
}

var searchScopeVsProjectDeletionActions = map[string]map[string]fmbt.ActionFunc{"Search": {
	"Ask":             action((*searchScopeVsProjectDeletionAdapter).Ask),
	"Answer":          action((*searchScopeVsProjectDeletionAdapter).Answer),
	"Reask":           action((*searchScopeVsProjectDeletionAdapter).Reask),
	"DeleteProject":   action((*searchScopeVsProjectDeletionAdapter).DeleteProject),
	"RecreateProject": action((*searchScopeVsProjectDeletionAdapter).RecreateProject),
	"OpenHit":         action((*searchScopeVsProjectDeletionAdapter).OpenHit),
}}

func searchScopeVsProjectDeletionOptions() map[string]any {
	return map[string]any{"max-seq-runs": 200, "max-actions": 8, "max-parallel-runs": 0}
}

// walkSearchScopeVsProjectDeletionPaths walks every derived path against
// one serve, a fresh fixture per walk (Init re-creates it), and compares
// the role's state with the spec's after every step.
func walkSearchScopeVsProjectDeletionPaths(a *searchScopeVsProjectDeletionAdapter, cover tracecheck.Cover) error {
	b, err := pathsJSONCover("search_scope_vs_project_deletion", cover)
	if err != nil {
		return err
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		return err
	}
	if len(doc.Paths) == 0 {
		return errors.New("no paths")
	}
	var errs []error
	for i, p := range doc.Paths {
		if err := a.walk(p.Trace); err != nil {
			var names []string
			for _, s := range p.Trace {
				names = append(names, strings.TrimPrefix(s.Action, "Search#0."))
			}
			errs = append(errs, fmt.Errorf("path %d %v: %w", i, names, err))
		}
	}
	return errors.Join(errs...)
}

func (a *searchScopeVsProjectDeletionAdapter) walk(trace []tracecheck.Step) error {
	for j, s := range trace {
		name := strings.TrimPrefix(s.Action, "Search#0.")
		var err error
		if s.Action == "Init" {
			err = a.Init()
		} else if f, ok := searchScopeVsProjectDeletionActions["Search"][name]; !ok {
			return fmt.Errorf("step %d: no action %q", j, s.Action)
		} else {
			_, err = f(a, nil)
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", j, name, err)
		}
		if a.gate.off {
			return fmt.Errorf("step %d (%s): the adapter's require says disabled", j, name)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w", j, name, err)
		}
		b, _ := json.Marshal(got)
		var norm map[string]any
		json.Unmarshal(b, &norm)
		for k, v := range s.State {
			f, ok := strings.CutPrefix(k, "Search#0.")
			if !ok {
				continue
			}
			want, _ := json.Marshal(v)
			have, _ := json.Marshal(norm[f])
			if string(want) != string(have) {
				return fmt.Errorf("step %d (%s): %s is %s, the spec says %s (state %s)", j, name, f, have, want, b)
			}
		}
	}
	return nil
}

// TestSearchScopeVsProjectDeletion lets fizzbee-mbt walk the spec at
// random against a real serve (MODEL_COVER=transitions only).
func TestSearchScopeVsProjectDeletion(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSearchScopeVsProjectDeletionAdapter(t)
	if err := runMBT(t, "search_scope_vs_project_deletion", a, searchScopeVsProjectDeletionActions, searchScopeVsProjectDeletionOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// TestSearchScopeVsProjectDeletionPaths walks every derived path against
// one serve: every settled state, or every link under
// MODEL_COVER=transitions.
func TestSearchScopeVsProjectDeletionPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSearchScopeVsProjectDeletionAdapter(t)
	if err := walkSearchScopeVsProjectDeletionPaths(a, envCover()); err != nil {
		t.Fatalf("spec path: %v", err)
	}
}

// The run above proves nothing unless a server that breaks the model
// fails it. This adapter reads every found hit's label as "alpha",
// never "gone" — the kind of wrong wiring a flow's adapter (or the
// product) could have — and the run must say so.
func TestSearchScopeVsProjectDeletionCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSearchScopeVsProjectDeletionAdapter(t)
	a.labelIgnoresDelete = true
	err := walkSearchScopeVsProjectDeletionPaths(a, tracecheck.CoverTransitions)
	if err == nil {
		t.Fatal("a run whose label ignores the project's deletion passed; the paths are not checking state")
	}
	t.Logf("caught as expected: %v", err)
}
