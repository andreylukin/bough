//go:build !windows

package mbt

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/context_inspector.fizz against a real serve: the Context page
// (#/s/<id>/context) is GET /api/sessions/{id}/context, its skill
// toggle is POST /api/off, and filing the session is POST
// /api/sessions/{id}/project. The page's own state (open, loading, a
// file row expanded) is client state, so the adapter tracks it; what
// the page would show once loaded is the snapshot the GET answered.

// The fixture: an AGENTS.md in the long band and a CLAUDE.md past it,
// in the session's cwd, one skill in ~/.claude/skills, one project.
const (
	ctxAgentsLines = 250
	ctxClaudeLines = 450
	ctxSkill       = "ctxdemo"
	ctxProject     = "ctxproj"
)

// The page colours a file past these line counts (LONG / TOO_LONG in
// web/src/context.tsx). Serve reports only the count; the band is the
// page's, and the browser layer checks the page applies it. Here the
// band turns serve's count into the spec's flag, so a miscounted file
// is a flag mismatch.
const (
	ctxLong    = 200
	ctxTooLong = 400
)

type contextInspectorAdapter struct {
	t    *testing.T
	s    *servetest.Server
	gate gate

	cwd string
	id  string
	ids []string

	// The page, as the person's browser holds it.
	page     string
	inflight bool
	fileOpen bool
	// snap is what the last 200 answered, shown while loaded.
	snap *ctxSnapshot
	// last is the state GetState last read; see GetState.
	last map[string]any

	// assignNoop is the deliberate bug TestContextInspectorCatchesWrongAdapter
	// injects: Assign always files the session under no project.
	assignNoop bool
}

// ctxSnapshot is the part of GET /api/sessions/{id}/context the spec
// is about.
type ctxSnapshot struct {
	Cwd          string `json:"cwd"`
	ContextFiles []struct {
		Path  string `json:"path"`
		Lines int    `json:"lines"`
		Found bool   `json:"found"`
	} `json:"contextFiles"`
	Skills []struct {
		ID  string `json:"id"`
		Off bool   `json:"off"`
	} `json:"skills"`
}

func newContextInspectorAdapter(t *testing.T) *contextInspectorAdapter {
	s := servetest.Start(t, servetest.Options{
		Config: controlConfig,
		Files: map[string]string{
			".claude/skills/" + ctxSkill + "/SKILL.md": "---\nname: " + ctxSkill + "\ndescription: a skill the context page can switch off\n---\nDemo.\n",
		},
	})
	a := &contextInspectorAdapter{t: t, s: s}
	a.cwd = s.Dir(t, "work")
	writeLines(t, filepath.Join(a.cwd, "AGENTS.md"), "agents", ctxAgentsLines)
	writeLines(t, filepath.Join(a.cwd, "CLAUDE.md"), "claude", ctxClaudeLines)
	ctx, cancel := actionCtx()
	defer cancel()
	var p struct {
		Project struct {
			Slug string `json:"slug"`
		} `json:"project"`
	}
	if err := ctxDo(ctx, s, http.MethodPost, "/api/projects", map[string]string{"name": ctxProject}, &p); err != nil {
		t.Fatal(err)
	}
	if p.Project.Slug != ctxProject {
		t.Fatalf("project slug %q, want %q", p.Project.Slug, ctxProject)
	}
	return a
}

// writeLines writes n distinct lines, so context-md's section dedup has
// nothing to drop between the two files.
func writeLines(t *testing.T, path, word string, n int) {
	t.Helper()
	var b strings.Builder
	for i := range n {
		fmt.Fprintf(&b, "%s line %d\n", word, i)
	}
	if err := os.WriteFile(path, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
}

// Init is a fresh idle local session in the same serve, the page
// closed, and the skill back on: off.yml outlives a walk.
//
// The session's child is stopped (archive kills it, unarchive leaves it
// stopped): a running child keeps the context files it started with,
// so filing it would not change what it reads until it starts again
// (specs/context_page_truth.fizz). With none running, the page lists
// what the next start reads, which is what this spec's files are.
func (a *contextInspectorAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.cwd, "")
	if err != nil {
		return err
	}
	if _, err := a.s.Archive(ctx, row.ID); err != nil {
		return err
	}
	if _, err := a.s.Unarchive(ctx, row.ID); err != nil {
		return err
	}
	if err := a.setOff(ctx, false); err != nil {
		return err
	}
	a.id, a.page, a.inflight, a.fileOpen, a.snap, a.last = row.ID, "closed", false, false, nil, nil
	a.ids = append(a.ids, row.ID)
	a.gate.reset()
	return nil
}

func (a *contextInspectorAdapter) Cleanup() error { return nil }

func (a *contextInspectorAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Context", Index: 0}: a}, nil
}

// GetState reads project and skill_off off serve on every step (they
// are server state and survive the page closing); files and the flags
// come from the snapshot the page is showing.
//
// Once the gate is off nothing the runner reads is validated, so the
// last state is handed back instead of reading serve again: with nine
// actions most walks go off within a step or two, and the reads were
// most of a run's time.
func (a *contextInspectorAdapter) GetState() (map[string]any, error) {
	if a.gate.off && a.last != nil {
		return a.last, nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return nil, err
	}
	live, err := a.fetch(ctx, a.id)
	if err != nil {
		return nil, err
	}
	skillOff, ok := live.skillOff()
	if !ok {
		return nil, fmt.Errorf("skill %q missing from the context's skills: %+v", ctxSkill, live.Skills)
	}
	files, agents, claude := []any{}, "", ""
	if a.page == "loaded" {
		files, agents, claude = a.snap.view(a.s.Home)
	}
	a.last = map[string]any{
		"page":        a.page,
		"inflight":    a.inflight,
		"project":     row.Project != "",
		"skill_off":   skillOff,
		"file_open":   a.fileOpen,
		"files":       files,
		"agents_flag": agents,
		"claude_flag": claude,
	}
	return a.last, nil
}

func (s *ctxSnapshot) skillOff() (bool, bool) {
	for _, k := range s.Skills {
		if k.ID == ctxSkill {
			return k.Off, true
		}
	}
	return false, false
}

// view is the spec's files list and flags: the cwd's files by
// basename, the project's MEMORY.md by name, home files as ~/…. A path
// that is none of those is reported whole, so it shows up in the
// mismatch rather than being folded into a name it is not.
func (s *ctxSnapshot) view(home string) (files []any, agents, claude string) {
	files = []any{}
	projects := filepath.Join(home, ".bough", "projects") + string(filepath.Separator)
	for _, f := range s.ContextFiles {
		name := f.Path
		switch {
		case filepath.Dir(f.Path) == s.Cwd:
			name = filepath.Base(f.Path)
		case strings.HasPrefix(f.Path, projects) && filepath.Base(f.Path) == "MEMORY.md":
			name = "MEMORY.md"
		case strings.HasPrefix(f.Path, home+string(filepath.Separator)):
			name = "~/" + strings.TrimPrefix(f.Path, home+string(filepath.Separator))
		}
		files = append(files, name)
		switch name {
		case "AGENTS.md":
			agents = ctxFlag(f.Lines)
		case "CLAUDE.md":
			claude = ctxFlag(f.Lines)
		}
	}
	return files, agents, claude
}

func ctxFlag(lines int) string {
	switch {
	case lines > ctxTooLong:
		return "max"
	case lines > ctxLong:
		return "long"
	}
	return ""
}

// Open mounts the page: its GET goes out and is answered by Resolve or
// Reject.
func (a *contextInspectorAdapter) Open() error {
	if a.gate.pass(a.page == "closed") {
		a.page, a.inflight = "loading", true
	}
	return nil
}

// Resolve is the page's GET answering 200.
func (a *contextInspectorAdapter) Resolve() error {
	if !a.gate.pass(a.inflight) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	snap, err := a.fetch(ctx, a.id)
	if err != nil {
		return err
	}
	a.snap, a.page, a.inflight = snap, "loaded", false
	return nil
}

// Reject is the page's GET failing. Serve fails it for a session it
// does not know (deleted under an open page), so that is the request
// sent; it must come back a non-2xx that names the session.
func (a *contextInspectorAdapter) Reject() error {
	if !a.gate.pass(a.inflight) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	gone := a.id + "-gone"
	_, err := a.fetch(ctx, gone)
	var apiErr *servetest.APIError
	if !errors.As(err, &apiErr) || apiErr.Status/100 == 2 || !strings.Contains(apiErr.Msg, gone) {
		return fmt.Errorf("GET context of an unknown session: want a non-2xx naming it, got %v", err)
	}
	a.page, a.inflight = "failed", false
	return nil
}

func (a *contextInspectorAdapter) Retry() error {
	if a.gate.pass(a.page == "failed" && !a.inflight) {
		a.inflight = true
	}
	return nil
}

// ToggleSkill is the skill row's button: POST /api/off with the
// opposite of what the page shows, which is serve's off flag.
func (a *contextInspectorAdapter) ToggleSkill() error {
	if !a.gate.pass(a.page == "loaded") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	live, err := a.fetch(ctx, a.id)
	if err != nil {
		return err
	}
	off, _ := live.skillOff()
	return a.setOff(ctx, !off)
}

func (a *contextInspectorAdapter) OpenFile() error {
	if a.gate.pass(a.page == "loaded" && !a.fileOpen) {
		a.fileOpen = true
	}
	return nil
}

func (a *contextInspectorAdapter) CloseFile() error {
	if a.gate.pass(a.fileOpen) {
		a.fileOpen = false
	}
	return nil
}

// Close unmounts the page; an answer still in flight lands on nothing.
func (a *contextInspectorAdapter) Close() error {
	if a.gate.pass(a.page != "closed") {
		a.page, a.inflight, a.fileOpen, a.snap = "closed", false, false, nil
	}
	return nil
}

// Assign files the session under the project, or takes it out.
func (a *contextInspectorAdapter) Assign() error {
	if !a.gate.pass(a.page == "closed") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return err
	}
	slug := ctxProject
	if row.Project != "" || a.assignNoop {
		slug = ""
	}
	return ctxDo(ctx, a.s, http.MethodPost, "/api/sessions/"+url.PathEscape(a.id)+"/project", map[string]string{"project": slug}, nil)
}

func (a *contextInspectorAdapter) fetch(ctx context.Context, id string) (*ctxSnapshot, error) {
	var snap ctxSnapshot
	if err := ctxDo(ctx, a.s, http.MethodGet, "/api/sessions/"+url.PathEscape(id)+"/context", nil, &snap); err != nil {
		return nil, err
	}
	return &snap, nil
}

func (a *contextInspectorAdapter) setOff(ctx context.Context, off bool) error {
	return ctxDo(ctx, a.s, http.MethodPost, "/api/off", map[string]any{"id": "skill:" + ctxSkill, "off": off}, nil)
}

// ctxDo is one authenticated API call for the endpoints servetest has
// no method for; a non-2xx is a *servetest.APIError like servetest's.
func ctxDo(ctx context.Context, s *servetest.Server, method, path string, body, out any) error {
	var rd io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return err
		}
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, s.URL+path, rd)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+s.Token)
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return err
	}
	if resp.StatusCode/100 != 2 {
		var e struct {
			Error string `json:"error"`
		}
		json.Unmarshal(raw, &e)
		return &servetest.APIError{Status: resp.StatusCode, Msg: e.Error + string(raw)}
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(raw, out)
}

var contextInspectorActions = map[string]map[string]fmbt.ActionFunc{"Context": {
	"Open":        action((*contextInspectorAdapter).Open),
	"Resolve":     action((*contextInspectorAdapter).Resolve),
	"Reject":      action((*contextInspectorAdapter).Reject),
	"Retry":       action((*contextInspectorAdapter).Retry),
	"ToggleSkill": action((*contextInspectorAdapter).ToggleSkill),
	"OpenFile":    action((*contextInspectorAdapter).OpenFile),
	"CloseFile":   action((*contextInspectorAdapter).CloseFile),
	"Close":       action((*contextInspectorAdapter).Close),
	"Assign":      action((*contextInspectorAdapter).Assign),
}}

// No step is a model turn, so walks are cheap. The runner picks among
// all nine actions, disabled ones included, and stops checking a walk
// at its first disabled one, so only about one walk in seventy gets as
// far as Open then Resolve. 300 walks usually reach it a few times
// (a spec with AGENTS_LINES mutated failed at walk 101);
// TestContextInspectorPaths is what covers every transition each run.
func contextInspectorOptions() map[string]any {
	return map[string]any{"max-seq-runs": 300, "max-actions": 8, "max-parallel-runs": 0}
}

// contextInspectorHistory: nothing the page does is a turn, and filing
// a session is serve's meta, not its history, so a transcript this
// flow wrote holds no action at all. What it can vouch for is that: an
// input in it is projected as Context#0.Prompt, which the spec does
// not have, so a walk that spent a model turn is a violation.
func contextInspectorHistory(entries []history.Entry) []tracecheck.Step {
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Context#0.page": "closed", "Context#0.inflight": false}}}
	for _, e := range entries {
		if e.Kind == "input" {
			steps = append(steps, tracecheck.Step{Action: "Context#0.Prompt"})
		}
	}
	return steps
}

func init() { historyProjections["context_inspector"] = contextInspectorHistory }

// A transcript with a turn in it is not one this flow wrote: looking at
// the Context page never prompts. The projection must say so rather
// than pass whatever it is handed.
func TestContextInspectorHistoryRejectsTurns(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	g, err := tracecheck.Load(fizzCheck(t, "context_inspector"))
	if err != nil {
		t.Fatal(err)
	}
	quiet := []history.Entry{{Seq: 1, Kind: "meta"}}
	if v := g.Check(contextInspectorHistory(quiet)); v != nil {
		t.Fatalf("a transcript with no turn: %v", v)
	}
	turn := append(quiet, history.Entry{Seq: 2, Kind: "input"}, history.Entry{Seq: 3, Kind: "done"})
	if v := g.Check(contextInspectorHistory(turn)); v == nil {
		t.Fatal("a transcript with a turn in it passed as a context_inspector path")
	}
}

func TestContextInspector(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newContextInspectorAdapter(t)
	if err := runMBT(t, "context_inspector", a, contextInspectorActions, contextInspectorOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "context_inspector"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), contextInspectorHistory)
	}
}

// TestContextInspectorPaths walks every generated path (the ones the
// browser spec walks) through the same adapter and compares each state
// with the path's. The random runner reaches a loaded page in about
// one walk in seventy and a loaded page in a project far more rarely;
// this covers each transition of the graph on every run, and needs no
// fizz tools: TestSpecFixtures keeps paths.json the spec's.
func TestContextInspectorPaths(t *testing.T) {
	t.Parallel()
	raw, err := pathsJSON("context_inspector")
	if err != nil {
		t.Fatal(err)
	}
	var file struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(raw, &file); err != nil {
		t.Fatal(err)
	}
	if len(file.Paths) == 0 {
		t.Fatal("no paths in testdata/context_inspector/paths.json")
	}
	a := newContextInspectorAdapter(t)
	for i, p := range file.Paths {
		if err := a.Init(); err != nil {
			t.Fatalf("path %d: Init: %v", i, err)
		}
		for j, step := range p.Trace {
			if step.Action != "Init" {
				name := strings.TrimPrefix(step.Action, "Context#0.")
				act, ok := contextInspectorActions["Context"][name]
				if !ok {
					t.Fatalf("path %d step %d: no adapter action %q", i, j, step.Action)
				}
				if _, err := act(a, nil); err != nil {
					t.Fatalf("path %d step %d (%s): %v", i, j, step.Action, err)
				}
			}
			got, err := a.GetState()
			if err != nil {
				t.Fatalf("path %d step %d (%s): state: %v", i, j, step.Action, err)
			}
			want := map[string]any{}
			for k, v := range step.State {
				if f, ok := strings.CutPrefix(k, "Context#0."); ok {
					want[f] = v
				}
			}
			gb, _ := json.Marshal(got)
			wb, _ := json.Marshal(want)
			if !bytes.Equal(gb, wb) {
				t.Fatalf("path %d step %d (%s):\n got  %s\n want %s", i, j, step.Action, gb, wb)
			}
		}
	}
}

// Assign that never files the session anywhere is the wrong wiring a
// run must catch: project stays false where the spec has it true.
func TestContextInspectorCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newContextInspectorAdapter(t)
	a.assignNoop = true
	if err := runMBT(t, "context_inspector", a, contextInspectorActions, contextInspectorOptions()); err == nil {
		t.Fatal("a run whose Assign files the session nowhere passed; the runner is not checking state")
	}
}
