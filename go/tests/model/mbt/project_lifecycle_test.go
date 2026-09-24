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

// specs/project_lifecycle.fizz against a real serve: one project slot
// ("alpha"), created, renamed, broken and fixed, given a MEMORY.md and
// deleted through the API the control room calls, and written by an
// "agent" straight into ~/.bough/projects/alpha — agents have no API,
// they use the file tools.
//
// The serve runs with BOUGH_CONTAINER=none: listing a project asks the
// runtime whether its image exists, and this flow has no containers in
// it, so the host's engine must never be the one answering.

// plAdapter plays the web page and the agent. shown is the page's last
// project list read, which only a person's action (or the poll) makes;
// everything else is read off the server on every GetState.
type plAdapter struct {
	t    *testing.T
	s    *servetest.Server
	gate gate

	convo string // the one local conversation, created once per serve
	shown string

	// journal is every walk's executed steps with the state observed
	// after each, replayed on the graph after the run: the runner stops
	// checking a walk silently at an action the graph does not enable,
	// so a gate that let one through would otherwise go unseen.
	journal [][]tracecheck.Step

	// createLower is the deliberate bug TestProjectLifecycleCatches
	// WrongAdapter injects: Create names the project "alpha", the slug,
	// instead of "Alpha". It is on Create because a random walk reaches
	// Create in one step and Rename seldom.
	createLower bool
}

const plSlug = "alpha"

func newProjectLifecycleAdapter(t *testing.T) *plAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig, Env: []string{"BOUGH_CONTAINER=none"}})
	a := &plAdapter{t: t, s: s}
	// The conversation is created once and reused: every walk ends with
	// it loose again (Init deletes the project, which unfiles it). Its
	// one turn names a repo so 'Create project from <repo>' has a group.
	dir := control.Dir(s.Home)
	control.Queue(t, dir, "p0000", control.Turn{Mode: "ok", Text: "looked"})
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := s.CreateSession(ctx, s.Dir(t, "work"), "look at ~/repos/widget")
	if err != nil {
		t.Fatal(err)
	}
	a.convo = row.ID
	if _, err := waitRow(s, row.ID, "the repo turn to finish", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		t.Fatal(err)
	}
	return a
}

// plView is the spec's state as the server has it, less shown.
type plView struct{ disk, name, slug, memory, convo string }

func (a *plAdapter) view() (plView, error) {
	v := plView{disk: "none"}
	var list struct {
		Projects []serve.Project `json:"projects"`
	}
	if err := a.call(http.MethodGet, "/api/projects", nil, &list); err != nil {
		return v, err
	}
	for _, p := range list.Projects {
		if p.Slug != plSlug {
			return v, fmt.Errorf("unexpected project %q listed (%+v)", p.Slug, p)
		}
		v.disk, v.name, v.slug = "ok", p.Name, p.Slug
		if p.Error != "" {
			v.disk = "broken"
		}
	}
	if v.disk != "none" {
		var d serve.OrbDetail
		if err := a.call(http.MethodGet, "/api/projects/"+plSlug+"/orb", nil, &d); err != nil {
			return v, err
		}
		if d.Files["MEMORY.md"] != "" {
			v.memory = "saved"
		}
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.convo)
	if err != nil {
		return v, err
	}
	switch row.Project {
	case "":
		v.convo = "loose"
	case plSlug:
		v.convo = "filed"
	default:
		v.convo = "in " + row.Project // a mismatch that names itself
	}
	return v, nil
}

func (a *plAdapter) state(v plView) map[string]any {
	return map[string]any{"disk": v.disk, "name": v.name, "slug": v.slug, "shown": a.shown, "memory": v.memory, "convo": v.convo}
}

// Init empties the slot in the same serve: the project (if a walk left
// one) is deleted the way the page does, which also unfiles the
// conversation and takes MEMORY.md with the directory.
func (a *plAdapter) Init() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if v.disk != "none" {
		if err := a.call(http.MethodDelete, "/api/projects/"+plSlug, nil, nil); err != nil {
			return err
		}
	}
	if v, err = a.view(); err != nil {
		return err
	}
	if v.disk != "none" || v.convo != "loose" {
		return fmt.Errorf("init: slot not empty after delete: %+v", v)
	}
	a.shown = "none"
	a.gate.reset()
	a.journal = append(a.journal, []tracecheck.Step{{Action: "Init", State: qualify(a.state(v))}})
	return nil
}

func (a *plAdapter) Cleanup() error { return nil }

func (a *plAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Project", Index: 0}: a}, nil
}

func (a *plAdapter) GetState() (map[string]any, error) {
	v, err := a.view()
	if err != nil {
		return nil, err
	}
	return a.state(v), nil
}

func qualify(st map[string]any) map[string]any {
	out := map[string]any{}
	for k, v := range st {
		out["Project#0."+k] = v
	}
	return out
}

// step runs one action: the spec's require (read off the server and the
// page's list) gates it, do performs it, and the state after is
// journaled for the replay.
func (a *plAdapter) step(name string, require func(v plView) bool, do func(v plView) error) error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(require(v)) {
		return nil
	}
	if err := do(v); err != nil {
		return fmt.Errorf("%s: %w", name, err)
	}
	after, err := a.view()
	if err != nil {
		return err
	}
	w := len(a.journal) - 1
	a.journal[w] = append(a.journal[w], tracecheck.Step{Action: "Project#0." + name, State: qualify(a.state(after))})
	return nil
}

// refresh is the page re-reading its list, as every person's action
// ends by doing.
func (a *plAdapter) refresh() error {
	v, err := a.view()
	a.shown = v.disk
	return err
}

func (a *plAdapter) Create() error {
	return a.step("Create", func(v plView) bool { return v.disk == "none" }, func(plView) error {
		name := "Alpha"
		if a.createLower {
			name = plSlug
		}
		if err := a.call(http.MethodPost, "/api/projects", map[string]string{"name": name}, nil); err != nil {
			return err
		}
		return a.refresh()
	})
}

func (a *plAdapter) CreateTaken() error {
	return a.step("CreateTaken", func(v plView) bool { return v.disk != "none" && v.memory == "" && v.convo == "loose" }, func(plView) error {
		if err := refused(http.StatusConflict, a.call(http.MethodPost, "/api/projects", map[string]string{"name": "Alpha"}, nil)); err != nil {
			return err
		}
		return a.refresh()
	})
}

func (a *plAdapter) CreateBadName() error {
	return a.step("CreateBadName", func(v plView) bool { return v.disk == "none" && a.shown == v.disk }, func(plView) error {
		return refused(http.StatusBadRequest, a.call(http.MethodPost, "/api/projects", map[string]string{"name": "!!!"}, nil))
	})
}

func (a *plAdapter) CreateFromRepo() error {
	return a.step("CreateFromRepo", func(v plView) bool { return v.disk == "none" && a.shown == v.disk && v.convo == "loose" }, func(plView) error {
		body := map[string]any{"repos": []string{"widget"}, "name": "Alpha"}
		if err := a.call(http.MethodPost, "/api/projects/from-repo", body, nil); err != nil {
			return err
		}
		return a.refresh()
	})
}

func (a *plAdapter) File() error {
	return a.step("File", func(v plView) bool { return a.shown != "none" && v.disk != "none" && v.convo == "loose" }, func(plView) error {
		if err := a.call(http.MethodPost, "/api/sessions/"+a.convo+"/project", map[string]string{"project": plSlug}, nil); err != nil {
			return err
		}
		return a.refresh()
	})
}

func (a *plAdapter) Rename() error {
	return a.step("Rename", func(v plView) bool { return a.shown != "none" && v.disk == "ok" }, func(v plView) error {
		to := "Beta"
		if v.name != "Alpha" {
			to = "Alpha"
		}
		if err := a.call(http.MethodPost, "/api/projects/"+plSlug+"/rename", map[string]string{"name": to}, nil); err != nil {
			return err
		}
		return a.refresh()
	})
}

// RenameBroken: the name: line cannot be spliced into a file that does
// not parse. The person's request was fine; the definition is not, so
// it is a 400 that says so, not a server fault.
func (a *plAdapter) RenameBroken() error {
	return a.step("RenameBroken", func(v plView) bool {
		return a.shown != "none" && v.disk == "broken" && v.memory == "" && v.convo == "loose"
	}, func(plView) error {
		if err := refused(http.StatusBadRequest, a.call(http.MethodPost, "/api/projects/"+plSlug+"/rename", map[string]string{"name": "Beta"}, nil)); err != nil {
			return err
		}
		return a.refresh()
	})
}

func (a *plAdapter) Delete() error {
	return a.step("Delete", func(v plView) bool { return a.shown == v.disk && v.disk != "none" }, func(plView) error {
		if err := a.call(http.MethodDelete, "/api/projects/"+plSlug, nil, nil); err != nil {
			return err
		}
		return a.refresh()
	})
}

// DeleteMistyped sends nothing: the dialog refuses a confirm that is
// not the slug before any request.
func (a *plAdapter) DeleteMistyped() error {
	return a.step("DeleteMistyped", func(v plView) bool {
		return a.shown == v.disk && v.disk != "none" && v.memory == "" && v.convo == "loose"
	}, func(plView) error { return nil })
}

func (a *plAdapter) SaveMemory() error {
	return a.step("SaveMemory", func(v plView) bool { return a.shown == v.disk && v.disk != "none" && v.memory == "" }, func(plView) error {
		if err := a.call(http.MethodPut, "/api/projects/"+plSlug+"/orb/files/MEMORY.md", map[string]string{"text": "# Alpha\n\nremember the widget\n"}, nil); err != nil {
			return err
		}
		return a.refresh()
	})
}

// plValidYAML is a definition with nothing in it that touches the
// host: no repos to check and no identity (github would run `gh`).
const plValidYAML = "name: Alpha\nrepos: []\n"

func (a *plAdapter) FixYaml() error {
	return a.step("FixYaml", func(v plView) bool { return a.shown == v.disk && v.disk == "broken" }, func(plView) error {
		if err := a.call(http.MethodPut, "/api/projects/"+plSlug+"/orb/files/project.yml", map[string]string{"text": plValidYAML}, nil); err != nil {
			return err
		}
		return a.refresh()
	})
}

// SaveYamlInvalid parses but fails the host check: a repo path that is
// not a directory, the validation list's case.
func (a *plAdapter) SaveYamlInvalid() error {
	return a.step("SaveYamlInvalid", func(v plView) bool {
		return a.shown == v.disk && v.disk != "none" && v.memory == "" && v.convo == "loose"
	}, func(plView) error {
		bad := "name: Alpha\nrepos:\n  - path: " + filepath.Join(a.s.Root, "no-such-checkout") + "\n"
		if err := refused(http.StatusBadRequest, a.call(http.MethodPut, "/api/projects/"+plSlug+"/orb/files/project.yml", map[string]string{"text": bad}, nil)); err != nil {
			return err
		}
		return a.refresh()
	})
}

func (a *plAdapter) yamlPath() string {
	return filepath.Join(a.s.Home, ".bough", "projects", plSlug, "project.yml")
}

// AgentCreate and AgentBreak are the file tools: no request, and the
// page is not told.
func (a *plAdapter) AgentCreate() error {
	return a.step("AgentCreate", func(v plView) bool { return v.disk == "none" }, func(plView) error {
		if err := os.MkdirAll(filepath.Dir(a.yamlPath()), 0o755); err != nil {
			return err
		}
		return os.WriteFile(a.yamlPath(), []byte(plValidYAML), 0o644)
	})
}

func (a *plAdapter) AgentBreak() error {
	return a.step("AgentBreak", func(v plView) bool { return v.disk == "ok" }, func(plView) error {
		// Broken past the name: line. Rename splices that one line, so a
		// break on it is repaired by a rename rather than refused.
		return os.WriteFile(a.yamlPath(), []byte("name: Alpha\nrepos: [\n"), 0o644)
	})
}

// Refresh is the page's 30 s projects poll.
func (a *plAdapter) Refresh() error {
	return a.step("Refresh", func(v plView) bool { return a.shown != v.disk }, func(plView) error { return a.refresh() })
}

// call is one API request as the page makes it; a non-2xx answer is a
// *servetest.APIError carrying the status.
func (a *plAdapter) call(method, path string, body, out any) error {
	ctx, cancel := actionCtx()
	defer cancel()
	return apiCall(ctx, a.s, method, path, body, out)
}

func apiCall(ctx context.Context, s *servetest.Server, method, path string, body, out any) error {
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
	req.Header.Set("Content-Type", "application/json")
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
		if json.Unmarshal(raw, &e) != nil || e.Error == "" {
			e.Error = strings.TrimSpace(string(raw))
		}
		return &servetest.APIError{Status: resp.StatusCode, Msg: e.Error}
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(raw, out)
}

// refused wants err to be the API answering status: a refusal that
// came back as anything else, success included, is the finding.
func refused(status int, err error) error {
	var ae *servetest.APIError
	if !errors.As(err, &ae) {
		return fmt.Errorf("want HTTP %d, got %v", status, err)
	}
	if ae.Status != status {
		return fmt.Errorf("want HTTP %d, got %w", status, ae)
	}
	return nil
}

var projectLifecycleActions = map[string]map[string]fmbt.ActionFunc{"Project": {
	"Create":          action((*plAdapter).Create),
	"CreateTaken":     action((*plAdapter).CreateTaken),
	"CreateBadName":   action((*plAdapter).CreateBadName),
	"CreateFromRepo":  action((*plAdapter).CreateFromRepo),
	"File":            action((*plAdapter).File),
	"Rename":          action((*plAdapter).Rename),
	"RenameBroken":    action((*plAdapter).RenameBroken),
	"Delete":          action((*plAdapter).Delete),
	"DeleteMistyped":  action((*plAdapter).DeleteMistyped),
	"SaveMemory":      action((*plAdapter).SaveMemory),
	"FixYaml":         action((*plAdapter).FixYaml),
	"SaveYamlInvalid": action((*plAdapter).SaveYamlInvalid),
	"AgentCreate":     action((*plAdapter).AgentCreate),
	"AgentBreak":      action((*plAdapter).AgentBreak),
	"Refresh":         action((*plAdapter).Refresh),
}}

// No step here is a model turn, so walks are cheap: more and longer
// than the example's, because most of the graph is two or three
// actions past a Create or AgentCreate and 15 actions share the draw.
func projectLifecycleOptions() map[string]any {
	return map[string]any{"max-seq-runs": 300, "max-actions": 8, "max-parallel-runs": 0}
}

// projectLifecycleHistory reads what a transcript can say about this
// flow, which is only that it starts loose: filing is serve's meta.json,
// not the session's history, and nothing else in the spec is a session
// at all. The per-walk journal (see plAdapter) is this flow's trace.
func projectLifecycleHistory(entries []history.Entry) []tracecheck.Step {
	return []tracecheck.Step{{Action: "Init", State: map[string]any{"Project#0.convo": "loose"}}}
}

func init() { historyProjections["project_lifecycle"] = projectLifecycleHistory }

func TestProjectLifecycle(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newProjectLifecycleAdapter(t)
	if err := runMBT(t, "project_lifecycle", a, projectLifecycleActions, projectLifecycleOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "project_lifecycle"))
	if err != nil {
		t.Fatal(err)
	}
	steps := 0
	for i, walk := range a.journal {
		steps += len(walk) - 1
		if v := g.Check(walk); v != nil {
			b, _ := json.Marshal(walk)
			t.Errorf("walk %d is not a path in the model: %v\ntrace: %s", i, v, b)
		}
	}
	t.Logf("journal: %d walks, %d executed steps replayed on the graph", len(a.journal), steps)
	checkHistory(t, g, sessionHistory(t, a.s.Home, a.convo), projectLifecycleHistory)
}

// A Create that names the project after its slug lists "alpha" where
// the spec has "Alpha"; the run must see that.
func TestProjectLifecycleCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newProjectLifecycleAdapter(t)
	a.createLower = true
	if err := runMBT(t, "project_lifecycle", a, projectLifecycleActions, projectLifecycleOptions()); err == nil {
		t.Fatal("a run whose Create names the project alpha passed; the runner is not checking state")
	}
}

// TestProjectLifecyclePaths walks every path in testdata/project_lifecycle/
// paths.json — the ones the browser spec walks — against the same
// adapter. The runner draws among all 15 actions, most of them disabled
// at any node, so its walks rarely get three steps deep; these paths
// cover every transition of the graph. It needs no fizz tools.
func TestProjectLifecyclePaths(t *testing.T) {
	t.Parallel()
	b, err := pathsJSON("project_lifecycle")
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		t.Fatal(err)
	}
	a := newProjectLifecycleAdapter(t)
	actions := projectLifecycleActions["Project"]
	for i, p := range doc.Paths {
		for j, s := range p.Trace {
			var err error
			if j == 0 {
				err = a.Init()
			} else if f := actions[strings.TrimPrefix(s.Action, "Project#0.")]; f == nil {
				err = fmt.Errorf("no adapter action for %s", s.Action)
			} else if _, err = f(a, nil); err == nil && a.gate.off {
				err = fmt.Errorf("the adapter's require refused %s", s.Action)
			}
			if err == nil {
				err = plMatches(a, s.State)
			}
			if err != nil {
				t.Fatalf("path %d step %d (%s): %v", i, j, s.Action, err)
			}
		}
	}
	t.Logf("%d paths walked", len(doc.Paths))
}

// plMatches compares the role fields of a path's state with GetState.
func plMatches(a *plAdapter, want map[string]any) error {
	got, err := a.GetState()
	if err != nil {
		return err
	}
	var diff []string
	for k, w := range want {
		f, ok := strings.CutPrefix(k, "Project#0.")
		if !ok {
			continue
		}
		if got[f] != w {
			diff = append(diff, fmt.Sprintf("%s: spec %q, server %q", f, w, got[f]))
		}
	}
	if len(diff) > 0 {
		return fmt.Errorf("state mismatch: %s", strings.Join(diff, "; "))
	}
	return nil
}
