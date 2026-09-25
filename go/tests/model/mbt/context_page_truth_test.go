//go:build !windows

package mbt

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/context_page_truth.fizz against a real serve. The page is the
// client, so its state (open, whose URL, which GETs are out, how many
// switches) is the adapter's; what it shows once loaded is the snapshot
// GET /api/sessions/{id}/context answered. assigned is serve's meta
// (POST /api/sessions/S/project), child_project is the running child's
// Row.StartedIn, and a child restart is S's child killed (archive and
// unarchive) and started again by a turn, the only thing that starts a
// child for an existing session.

const ctpProject = "ctptruth"

type contextPageTruthAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	sCwd, tCwd string
	mem        string // the project's MEMORY.md
	s0, t0     string // this walk's S and T
	ids        []string
	turn       int

	// The page, as the person's browser holds it.
	page       string
	sel        string
	inflightS  bool
	inflightT  bool
	switches   int
	snap       *ctpSnapshot
	changed    bool // something the adapter did changed what snap describes
	editedOnce int

	// restartNoKill is the deliberate bug
	// TestContextPageTruthCatchesWrongAdapter injects: ChildRestart sends
	// its turn to the running child instead of starting a new one.
	restartNoKill bool
}

// ctpSnapshot is the part of GET /api/sessions/{id}/context the spec is
// about: whose it is, the files, which project's set they are, which
// set the next start reads, and when it was read.
type ctpSnapshot struct {
	Cwd          string `json:"cwd"`
	ContextFiles []struct {
		Path  string `json:"path"`
		Lines int    `json:"lines"`
		Found bool   `json:"found"`
	} `json:"contextFiles"`
	Project     string `json:"project"`
	NextProject string `json:"nextProject"`
	ReadAt      string `json:"readAt"`
}

// same is whether two reads describe the same thing; the read time is
// when, not what, so it is left out.
func (x *ctpSnapshot) same(y *ctpSnapshot) bool {
	a, b := *x, *y
	a.ReadAt, b.ReadAt = "", ""
	ja, _ := json.Marshal(a)
	jb, _ := json.Marshal(b)
	return bytes.Equal(ja, jb)
}

func newContextPageTruthAdapter(t *testing.T) *contextPageTruthAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &contextPageTruthAdapter{t: t, s: s, dir: control.Dir(s.Home)}
	a.sCwd, a.tCwd = s.Dir(t, "s-work"), s.Dir(t, "t-work")
	writeLines(t, filepath.Join(a.sCwd, "AGENTS.md"), "s agents", 10)
	writeLines(t, filepath.Join(a.tCwd, "AGENTS.md"), "t agents", 10)
	ctx, cancel := actionCtx()
	defer cancel()
	var p struct {
		Project struct {
			Slug string `json:"slug"`
		} `json:"project"`
	}
	if err := ctxDo(ctx, s, http.MethodPost, "/api/projects", map[string]string{"name": ctpProject}, &p); err != nil {
		t.Fatal(err)
	}
	if p.Project.Slug != ctpProject {
		t.Fatalf("project slug %q, want %q", p.Project.Slug, ctpProject)
	}
	a.mem = filepath.Join(s.Home, ".bough", "projects", ctpProject, "MEMORY.md")
	writeLines(t, a.mem, "memory", 5)
	return a
}

// Init is a fresh S and T in the same serve, the page closed. The last
// walk's pair is archived first: that kills their children, which
// would otherwise pile up two processes a walk.
func (a *contextPageTruthAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	for _, id := range []string{a.s0, a.t0} {
		if id != "" {
			if _, err := a.s.Archive(ctx, id); err != nil {
				return err
			}
		}
	}
	s, err := a.s.CreateSession(ctx, a.sCwd, "")
	if err != nil {
		return err
	}
	tr, err := a.s.CreateSession(ctx, a.tCwd, "")
	if err != nil {
		return err
	}
	a.s0, a.t0 = s.ID, tr.ID
	a.ids = append(a.ids, s.ID, tr.ID)
	a.page, a.sel, a.inflightS, a.inflightT, a.switches = "closed", "S", false, false, 0
	a.snap, a.changed = nil, false
	a.gate.reset()
	return nil
}

func (a *contextPageTruthAdapter) Cleanup() error { return nil }

func (a *contextPageTruthAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Inspector", Index: 0}: a}, nil
}

// who names the session a snapshot is of by its cwd, "" for neither.
func (a *contextPageTruthAdapter) who(snap *ctpSnapshot) string {
	real := func(p string) string {
		if r, err := filepath.EvalSymlinks(p); err == nil {
			return r
		}
		return p
	}
	switch real(snap.Cwd) {
	case real(a.sCwd):
		return "S"
	case real(a.tCwd):
		return "T"
	}
	return snap.Cwd
}

func (a *contextPageTruthAdapter) idOf(who string) string {
	if who == "T" {
		return a.t0
	}
	return a.s0
}

// serverS is S's row: assigned and child_project. S's child must be
// running at every step: the spec's child_project is about it.
func (a *contextPageTruthAdapter) serverS(ctx context.Context) (assigned, childProject bool, err error) {
	row, _, err := a.s.GetSession(ctx, a.s0)
	if err != nil {
		return false, false, err
	}
	if !row.Live {
		return false, false, fmt.Errorf("S's child is not running: %+v", row)
	}
	if row.StartedIn != "" && row.StartedIn != ctpProject {
		return false, false, fmt.Errorf("S's child started in %q, not %q", row.StartedIn, ctpProject)
	}
	return row.Project != "", row.StartedIn != "", nil
}

// stale is the spec's changed: the adapter changed what the snapshot
// describes, or a read now says something else than the snapshot does
// (something the adapter did not mean to change did).
func (a *contextPageTruthAdapter) stale(ctx context.Context) (bool, error) {
	if a.page != "loaded" || a.snap == nil {
		return false, nil
	}
	if a.changed {
		return true, nil
	}
	live, err := a.fetch(ctx, a.idOf(a.who(a.snap)))
	if err != nil {
		return false, err
	}
	return !live.same(a.snap), nil
}

func (a *contextPageTruthAdapter) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	assigned, childProject, err := a.serverS(ctx)
	if err != nil {
		return nil, err
	}
	changed, err := a.stale(ctx)
	if err != nil {
		return nil, err
	}
	shown, shownProject, shownNote, stamped := "", false, false, false
	if a.page == "loaded" {
		shown = a.who(a.snap)
		shownProject = len(a.snap.ContextFiles) > 0 && a.snap.ContextFiles[0].Path == a.mem
		shownNote = a.snap.Project != a.snap.NextProject
		stamped = a.snap.ReadAt != ""
	}
	return map[string]any{
		"page":          a.page,
		"sel":           a.sel,
		"shown":         shown,
		"inflight_S":    a.inflightS,
		"inflight_T":    a.inflightT,
		"switches":      a.switches,
		"assigned":      assigned,
		"child_project": childProject,
		"shown_project": shownProject,
		"shown_note":    shownNote,
		"stamped":       stamped,
		"changed":       changed,
	}, nil
}

func (a *contextPageTruthAdapter) Open() error {
	if a.gate.pass(a.page == "closed") {
		a.page, a.sel, a.inflightS = "loading", "S", true
	}
	return nil
}

// Switch is the hash moving to the other session with the page open:
// what the page shows goes, and a GET for the new session goes out.
func (a *contextPageTruthAdapter) Switch() error {
	if !a.gate.pass(a.page != "closed" && a.switches < 2) {
		return nil
	}
	a.switches++
	if a.sel == "S" {
		a.sel, a.inflightT = "T", true
	} else {
		a.sel, a.inflightS = "S", true
	}
	a.page, a.snap, a.changed = "loading", nil, false
	return nil
}

func (a *contextPageTruthAdapter) AnswerS() error { return a.answer("S") }
func (a *contextPageTruthAdapter) AnswerT() error { return a.answer("T") }

// answer is the GET for who answering 200 now. An answer for a session
// the page has left lands on nothing.
func (a *contextPageTruthAdapter) answer(who string) error {
	inflight := &a.inflightS
	if who == "T" {
		inflight = &a.inflightT
	}
	if !a.gate.pass(*inflight) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	snap, err := a.fetch(ctx, a.idOf(who))
	if err != nil {
		return err
	}
	*inflight = false
	if a.sel == who && a.page != "closed" {
		a.page, a.snap, a.changed = "loaded", snap, false
	}
	return nil
}

func (a *contextPageTruthAdapter) FailS() error { return a.fail("S") }
func (a *contextPageTruthAdapter) FailT() error { return a.fail("T") }

// fail is the GET for who failing. Serve fails it for a session it does
// not know, so that is the request sent; it must come back a non-2xx
// that names the session. Only a page with nothing on screen shows it.
func (a *contextPageTruthAdapter) fail(who string) error {
	inflight := &a.inflightS
	if who == "T" {
		inflight = &a.inflightT
	}
	if !a.gate.pass(*inflight) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	gone := a.idOf(who) + "-gone"
	_, err := a.fetch(ctx, gone)
	var apiErr *servetest.APIError
	if !errors.As(err, &apiErr) || apiErr.Status/100 == 2 || !strings.Contains(apiErr.Msg, gone) {
		return fmt.Errorf("GET context of an unknown session: want a non-2xx naming it, got %v", err)
	}
	*inflight = false
	if a.sel == who && a.page == "loading" {
		a.page = "failed"
	}
	return nil
}

func (a *contextPageTruthAdapter) Retry() error {
	out := a.inflightS
	if a.sel == "T" {
		out = a.inflightT
	}
	if !a.gate.pass(a.page == "failed" && !out) {
		return nil
	}
	if a.sel == "S" {
		a.inflightS = true
	} else {
		a.inflightT = true
	}
	return nil
}

func (a *contextPageTruthAdapter) Assign() error   { return a.file(true) }
func (a *contextPageTruthAdapter) Unassign() error { return a.file(false) }

// file files S under the project, or takes it out, with its child left
// running.
func (a *contextPageTruthAdapter) file(in bool) error {
	ctx, cancel := actionCtx()
	defer cancel()
	assigned, _, err := a.serverS(ctx)
	if err != nil {
		return err
	}
	if !a.gate.pass(assigned != in) {
		return nil
	}
	slug := ""
	if in {
		slug = ctpProject
	}
	if err := ctxDo(ctx, a.s, http.MethodPost, "/api/sessions/"+url.PathEscape(a.s0)+"/project", map[string]string{"project": slug}, nil); err != nil {
		return err
	}
	a.markS()
	return nil
}

// markS: whatever changed was S's, so a snapshot of S is now stale.
func (a *contextPageTruthAdapter) markS() {
	if a.page == "loaded" && a.who(a.snap) == "S" {
		a.changed = true
	}
}

// EditContextFile appends a line to S's AGENTS.md.
func (a *contextPageTruthAdapter) EditContextFile() error {
	ctx, cancel := actionCtx()
	defer cancel()
	changed, err := a.stale(ctx)
	if err != nil {
		return err
	}
	if !a.gate.pass(a.page == "loaded" && a.who(a.snap) == "S" && !changed) {
		return nil
	}
	a.editedOnce++
	f, err := os.OpenFile(filepath.Join(a.sCwd, "AGENTS.md"), os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	_, err = fmt.Fprintf(f, "edit %d\n", a.editedOnce)
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	if err != nil {
		return err
	}
	a.markS()
	return nil
}

// ChildRestart stops S's child (archive, which kills it, then unarchive)
// and starts it again the one way serve starts a child for an existing
// session: a turn, answered at once.
func (a *contextPageTruthAdapter) ChildRestart() error {
	ctx, cancel := actionCtx()
	defer cancel()
	assigned, childProject, err := a.serverS(ctx)
	if err != nil {
		return err
	}
	if !a.gate.pass(childProject != assigned) {
		return nil
	}
	before, _, err := a.s.GetSession(ctx, a.s0)
	if err != nil {
		return err
	}
	if !a.restartNoKill {
		if _, err := a.s.Archive(ctx, a.s0); err != nil {
			return err
		}
		if _, err := a.s.Unarchive(ctx, a.s0); err != nil {
			return err
		}
	}
	a.turn++
	name := fmt.Sprintf("ctp%04d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "ok", Text: "restarted " + name})
	if err := a.s.Prompt(ctx, a.s0, "restart "+name); err != nil {
		return err
	}
	control.WaitTaken(a.t, a.dir, name, actionTimeout)
	if _, err := waitRow(a.s, a.s0, "the restart turn to finish", func(r serve.Row) bool {
		return r.Live && r.Status == serve.StatusDone && r.Entries > before.Entries+1
	}); err != nil {
		return err
	}
	a.markS()
	return nil
}

// Close unmounts the page; what is in flight lands on nothing.
func (a *contextPageTruthAdapter) Close() error {
	if a.gate.pass(a.page != "closed") {
		a.page, a.sel, a.inflightS, a.inflightT = "closed", "S", false, false
		a.snap, a.changed = nil, false
	}
	return nil
}

func (a *contextPageTruthAdapter) fetch(ctx context.Context, id string) (*ctpSnapshot, error) {
	var snap ctpSnapshot
	if err := ctxDo(ctx, a.s, http.MethodGet, "/api/sessions/"+url.PathEscape(id)+"/context", nil, &snap); err != nil {
		return nil, err
	}
	return &snap, nil
}

var contextPageTruthActions = map[string]map[string]fmbt.ActionFunc{"Inspector": {
	"Open":            action((*contextPageTruthAdapter).Open),
	"Switch":          action((*contextPageTruthAdapter).Switch),
	"AnswerS":         action((*contextPageTruthAdapter).AnswerS),
	"AnswerT":         action((*contextPageTruthAdapter).AnswerT),
	"FailS":           action((*contextPageTruthAdapter).FailS),
	"FailT":           action((*contextPageTruthAdapter).FailT),
	"Retry":           action((*contextPageTruthAdapter).Retry),
	"Assign":          action((*contextPageTruthAdapter).Assign),
	"Unassign":        action((*contextPageTruthAdapter).Unassign),
	"EditContextFile": action((*contextPageTruthAdapter).EditContextFile),
	"ChildRestart":    action((*contextPageTruthAdapter).ChildRestart),
	"Close":           action((*contextPageTruthAdapter).Close),
}}

// Twelve actions and most walks leave the graph within a step or two;
// the *Paths test is what covers every state on each run.
func contextPageTruthOptions() map[string]any {
	return map[string]any{"max-seq-runs": 200, "max-actions": 8, "max-parallel-runs": 0}
}

// contextPageTruthHistory: the page never writes a transcript; only a
// child restart does, one turn each. Between two restarts S was filed
// in or out an odd number of times (a restart needs the child and the
// meta to disagree, and leaves them agreeing), so the n-th turn is a
// restart into the project for odd n and out of it for even n. What
// the page did in between is not in the file and is not claimed.
func contextPageTruthHistory(entries []history.Entry) []tracecheck.Step {
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{
		"Inspector#0.page": "closed", "Inspector#0.assigned": false, "Inspector#0.child_project": false,
	}}}
	in := false
	for _, e := range entries {
		if e.Kind != "input" {
			continue
		}
		file := "Inspector#0.Assign"
		if in {
			file = "Inspector#0.Unassign"
		}
		in = !in
		steps = append(steps,
			tracecheck.Step{Action: file, State: map[string]any{"Inspector#0.assigned": in}},
			tracecheck.Step{Action: "Inspector#0.ChildRestart", State: map[string]any{"Inspector#0.child_project": in}},
		)
	}
	return steps
}

func init() { historyProjections["context_page_truth"] = contextPageTruthHistory }

// walkContextPageTruth runs every walk in raw (pathsJSON's shape)
// through a and returns the first state that differs from the walk's.
func walkContextPageTruth(a *contextPageTruthAdapter, raw []byte) error {
	var file struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(raw, &file); err != nil {
		return err
	}
	if len(file.Paths) == 0 {
		return errors.New("no walks over testdata/context_page_truth")
	}
	for i, p := range file.Paths {
		if err := a.Init(); err != nil {
			return fmt.Errorf("walk %d: Init: %w", i, err)
		}
		for j, step := range p.Trace {
			if step.Action != "Init" {
				name := strings.TrimPrefix(step.Action, "Inspector#0.")
				act, ok := contextPageTruthActions["Inspector"][name]
				if !ok {
					return fmt.Errorf("walk %d step %d: no adapter action %q", i, j, step.Action)
				}
				if _, err := act(a, nil); err != nil {
					return fmt.Errorf("walk %d step %d (%s): %w", i, j, step.Action, err)
				}
				if a.gate.off {
					return fmt.Errorf("walk %d step %d (%s): the adapter found it disabled", i, j, step.Action)
				}
			}
			got, err := a.GetState()
			if err != nil {
				return fmt.Errorf("walk %d step %d (%s): state: %w", i, j, step.Action, err)
			}
			want := map[string]any{}
			for k, v := range step.State {
				if f, ok := strings.CutPrefix(k, "Inspector#0."); ok {
					want[f] = v
				}
			}
			gb, _ := json.Marshal(got)
			wb, _ := json.Marshal(want)
			if !bytes.Equal(gb, wb) {
				return fmt.Errorf("walk %d step %d (%s):\n got  %s\n want %s", i, j, step.Action, gb, wb)
			}
		}
	}
	return nil
}

func TestContextPageTruth(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newContextPageTruthAdapter(t)
	if err := runMBT(t, "context_page_truth", a, contextPageTruthActions, contextPageTruthOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "context_page_truth"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), contextPageTruthHistory)
	}
}

// TestContextPageTruthPaths walks the checked-in graph (every state, or
// every transition under MODEL_COVER=transitions) against one serve and
// then replays each transcript the walks wrote on the graph.
func TestContextPageTruthPaths(t *testing.T) {
	t.Parallel()
	raw, err := pathsJSON("context_page_truth")
	if err != nil {
		t.Fatal(err)
	}
	a := newContextPageTruthAdapter(t)
	if err := walkContextPageTruth(a, raw); err != nil {
		t.Fatal(err)
	}
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("context_page_truth")), "..", "testdata", "context_page_truth"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), contextPageTruthHistory)
	}
}

// A restart that never restarts the child is the wrong wiring a walk
// must catch: child_project stays false where the spec has it true.
func TestContextPageTruthCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	raw, err := pathsJSONCover("context_page_truth", tracecheck.CoverStates)
	if err != nil {
		t.Fatal(err)
	}
	a := newContextPageTruthAdapter(t)
	a.restartNoKill = true
	err = walkContextPageTruth(a, raw)
	if err == nil || !strings.Contains(err.Error(), "ChildRestart") {
		t.Fatalf("walks whose ChildRestart leaves the child running: %v; want a mismatch at a ChildRestart", err)
	}
}

// Two turns project to a restart into the project and one out of it,
// which is a path; the same trace with the second restart leaving the
// child in the project is not, so the projection's states are checked.
func TestContextPageTruthHistoryShape(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("context_page_truth")), "..", "testdata", "context_page_truth"))
	if err != nil {
		t.Fatal(err)
	}
	two := []history.Entry{{Seq: 1, Kind: "meta"}, {Seq: 2, Kind: "input"}, {Seq: 3, Kind: "done"}, {Seq: 4, Kind: "input"}, {Seq: 5, Kind: "done"}}
	if v := g.Check(contextPageTruthHistory(two)); v != nil {
		t.Fatalf("two restarts: %v", v)
	}
	steps := contextPageTruthHistory(two)
	steps[len(steps)-1].State["Inspector#0.child_project"] = true
	if v := g.Check(steps); v == nil {
		t.Fatal("a restart out of the project that left the child in it passed")
	}
}
