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
	"os/exec"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/session_filing.fizz: filing sessions into projects, driven
// through a real serve. Session#0 is a local session in a repo
// checkout, Session#1 a project session in project "a" whose child is
// never started, Project#0 is project "b", which comes and goes.
//
// The adapter plays the web page and the other API client the spec
// names: Assign is Settings/the Projects page's move, Drop the sidebar
// drop (both one POST /api/sessions/{id}/project), FromRepo is what
// projects.tsx does for "Create project from <repo>" (POST
// /api/projects, then one assign per session in the repo's by-repo
// group), Delete is DELETE /api/projects/b, ApiMove an API client's
// move of a project session. Start is a message, which resumes the
// child. Stop is a crash: SIGKILL of the child, because nothing a
// person can press stops an idle local child (see the spec's Stop).

// sessionFilingAdapter is the fmbt.Model; its roles are the sessions
// and the project. It holds the one serve all walks share.
type sessionFilingAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	walk  int
	turn  int
	repo  string // this walk's checkout name, so its by-repo group is its own
	local *filingSession
	orb   *filingSession
	b     *filingProject
	ids   []string // every local session, for the history check

	// skipFromRepoAssign is the deliberate wiring bug
	// TestSessionFilingCatchesWrongAdapter injects: FromRepo creates the
	// project and files nothing.
	skipFromRepoAssign bool
}

// filingSession is one Session role. mode is the spec's role param.
type filingSession struct {
	a    *sessionFilingAdapter
	mode string
	id   string
}

// filingProject is the Project role: project "b".
type filingProject struct{ a *sessionFilingAdapter }

func newSessionFilingAdapter(t *testing.T) *sessionFilingAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &sessionFilingAdapter{t: t, s: s, dir: control.Dir(s.Home)}
	a.local = &filingSession{a: a, mode: "local"}
	a.orb = &filingSession{a: a, mode: "project"}
	a.b = &filingProject{a: a}
	// Project "a" exists for the whole run, as the spec assumes.
	ctx, cancel := actionCtx()
	defer cancel()
	if slug, err := a.newProject(ctx, "a"); err != nil || slug != "a" {
		t.Fatalf("create project a: slug %q, %v", slug, err)
	}
	return a
}

// Init starts a walk on two fresh sessions and no project "b". The
// sessions' histories are written straight to disk, as a finished
// session leaves them: the local one has never run a turn and has no
// child (live = False), and the project one was made by serve for "a"
// (mode project, BOUGH_PROJECT=a) and needs no container, since
// nothing in the spec starts it.
func (a *sessionFilingAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	if made, err := a.b.made(ctx); err != nil {
		return err
	} else if made {
		if err := a.api(ctx, http.MethodDelete, "/api/projects/b", nil, nil); err != nil {
			return fmt.Errorf("delete the last walk's b: %w", err)
		}
	}
	a.walk++
	a.repo = fmt.Sprintf("w%04d", a.walk)
	cwd := a.s.Dir(a.t, filepath.Join("repos", a.repo))
	local, err := a.seed(map[string]any{"cwd": cwd, "mode": "local", "origin": "web"})
	if err != nil {
		return err
	}
	// A cwd with no "repos/" in it: the by-repo pile is about local
	// checkouts, and this one is inside its orb.
	orb, err := a.seed(map[string]any{"cwd": a.s.Dir(a.t, "orb"), "mode": "project", "project": "a", "origin": "web"},
		history.Entry{Kind: "input", Data: map[string]any{"text": "work in a"}},
		history.Entry{Kind: "done", Data: map[string]any{}})
	if err != nil {
		return err
	}
	a.local.id, a.orb.id = local, orb
	a.ids = append(a.ids, local)
	a.gate.reset()
	return nil
}

// seed writes a history file serve lists on its next read. The rename
// keeps serve from reading half of it.
func (a *sessionFilingAdapter) seed(meta map[string]any, rest ...history.Entry) (string, error) {
	id := history.NewID()
	now := time.Now()
	var buf bytes.Buffer
	for i, e := range append([]history.Entry{{Kind: "meta", Data: meta}}, rest...) {
		e.Seq, e.At = int64(i+1), now
		b, err := json.Marshal(e)
		if err != nil {
			return "", err
		}
		buf.Write(append(b, '\n'))
	}
	dir := filepath.Join(a.s.Home, ".bough", "history")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return "", err
	}
	tmp := filepath.Join(dir, id+".seed")
	if err := os.WriteFile(tmp, buf.Bytes(), 0o644); err != nil {
		return "", err
	}
	return id, os.Rename(tmp, filepath.Join(dir, id+".jsonl"))
}

// Cleanup ends a child the walk left running, so the next walk's
// sessions are the only live ones.
func (a *sessionFilingAdapter) Cleanup() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.local.id)
	if err != nil || !row.Live {
		return err
	}
	return a.local.crash()
}

func (a *sessionFilingAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{
		{RoleName: "Session", Index: 0}: a.local,
		{RoleName: "Session", Index: 1}: a.orb,
		{RoleName: "Project", Index: 0}: a.b,
	}, nil
}

// GetState is the spec's global state, which holds only the roles.
func (a *sessionFilingAdapter) GetState() (map[string]any, error) { return map[string]any{}, nil }

// GetState is one session as the page and the child see it:
//
//	project  the row's project (GET /api/sessions/{id})
//	page     the project page whose threads list it
//	byrepo   listed in GET /api/projects/by-repo
//	live     the row says a child is running
//	injected the project directory the running child was started with,
//	         read off the child process's own environment
func (s *filingSession) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	a := s.a
	row, _, err := a.s.GetSession(ctx, s.id)
	if err != nil {
		return nil, err
	}
	var pages []string
	for _, slug := range []string{"a", "b"} {
		var d serve.ProjectDetail
		err := a.api(ctx, http.MethodGet, "/api/projects/"+slug, nil, &d)
		if isStatus(err, http.StatusNotFound) {
			continue
		}
		if err != nil {
			return nil, err
		}
		if slices.ContainsFunc(d.Threads, func(r serve.Row) bool { return r.ID == s.id }) {
			pages = append(pages, slug)
		}
	}
	var groups struct {
		Groups []serve.RepoGroup `json:"groups"`
	}
	if err := a.api(ctx, http.MethodGet, "/api/projects/by-repo", nil, &groups); err != nil {
		return nil, err
	}
	byrepo := false
	for _, g := range groups.Groups {
		byrepo = byrepo || slices.Contains(g.Sessions, s.id)
	}
	injected := ""
	if row.Live {
		if injected, err = s.injected(); err != nil {
			return nil, err
		}
	}
	return map[string]any{
		"project":  row.Project,
		"page":     strings.Join(pages, "+"), // two pages is a state no spec value matches
		"byrepo":   byrepo,
		"live":     row.Live,
		"injected": injected,
	}, nil
}

func (p *filingProject) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	made, err := p.made(ctx)
	return map[string]any{"made": made}, err
}

func (p *filingProject) made(ctx context.Context) (bool, error) {
	err := p.a.api(ctx, http.MethodGet, "/api/projects/b", nil, nil)
	if isStatus(err, http.StatusNotFound) {
		return false, nil
	}
	return err == nil, err
}

// view is the adapter's own reading of the fields a require needs.
func (s *filingSession) view() (project string, live bool, err error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := s.a.s.GetSession(ctx, s.id)
	return row.Project, row.Live, err
}

// Assign is Settings' select or the Projects page's move: to any
// project the page lists, or out of the one it is in. The spec's
// `oneof` is the runner's choice, passed in as the argument "to".
func (s *filingSession) Assign(to string) error {
	project, _, err := s.view()
	if err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	made, err := s.a.b.made(ctx)
	if err != nil {
		return err
	}
	if !s.a.gate.pass(s.mode == "local" && to != project && (to != "b" || made)) {
		return nil
	}
	return s.assign(ctx, to)
}

// Drop is dropping the row on "a"'s sidebar group.
func (s *filingSession) Drop() error {
	project, _, err := s.view()
	if err != nil {
		return err
	}
	if !s.a.gate.pass(s.mode == "local" && project != "a") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	return s.assign(ctx, "a")
}

// FromRepo is projects.tsx's fromRepo: a new project, then every
// session in the repo's unfiled group assigned to it.
func (s *filingSession) FromRepo() error {
	project, _, err := s.view()
	if err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	made, err := s.a.b.made(ctx)
	if err != nil {
		return err
	}
	if !s.a.gate.pass(s.mode == "local" && project == "" && !made) {
		return nil
	}
	var groups struct {
		Groups []serve.RepoGroup `json:"groups"`
	}
	if err := s.a.api(ctx, http.MethodGet, "/api/projects/by-repo", nil, &groups); err != nil {
		return err
	}
	var ids []string
	for _, g := range groups.Groups {
		if g.Repo == s.a.repo {
			ids = g.Sessions
		}
	}
	if !slices.Contains(ids, s.id) {
		return fmt.Errorf("by-repo has no group %q holding %s: %+v", s.a.repo, s.id, groups.Groups)
	}
	slug, err := s.a.newProject(ctx, "b")
	if err != nil {
		return err
	}
	if slug != "b" {
		return fmt.Errorf("project b got slug %q", slug)
	}
	if s.a.skipFromRepoAssign {
		return nil
	}
	for _, id := range ids {
		if err := s.a.api(ctx, http.MethodPost, "/api/sessions/"+url.PathEscape(id)+"/project", map[string]string{"project": slug}, nil); err != nil {
			return err
		}
	}
	return nil
}

// ApiMove is another client taking a project session out of its
// project. serve must refuse it with 409 and change nothing.
func (s *filingSession) ApiMove() error {
	if !s.a.gate.pass(s.mode == "project") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	err := s.assign(ctx, "")
	if !isStatus(err, http.StatusConflict) {
		return fmt.Errorf("moving a project session out of its project: want 409, got %v", err)
	}
	return nil
}

// Start is a message to the session: serve resumes its child, which
// runs the turn and stays up.
func (s *filingSession) Start() error {
	_, live, err := s.view()
	if err != nil {
		return err
	}
	if !s.a.gate.pass(s.mode == "local" && !live) {
		return nil
	}
	a := s.a
	a.turn++
	name := fmt.Sprintf("f%05d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "ok", Text: "done " + name})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, s.id, "turn "+name); err != nil {
		return err
	}
	control.WaitTaken(a.t, a.dir, name, actionTimeout)
	_, err = waitRow(a.s, s.id, "the turn to finish on a live child", func(r serve.Row) bool {
		return r.Status == serve.StatusDone && r.Live
	})
	return err
}

// Stop is the child going away: a crash, since serve offers a person no
// stop for an idle local child.
func (s *filingSession) Stop() error {
	_, live, err := s.view()
	if err != nil {
		return err
	}
	if !s.a.gate.pass(live) {
		return nil
	}
	return s.crash()
}

func (s *filingSession) crash() error {
	pid, err := s.pid()
	if err != nil {
		return err
	}
	if err := syscall.Kill(pid, syscall.SIGKILL); err != nil {
		return fmt.Errorf("kill %d: %w", pid, err)
	}
	_, err = waitRow(s.a.s, s.id, "the child to be gone", func(r serve.Row) bool { return !r.Live })
	return err
}

// Delete is the Projects page's delete of "b".
func (p *filingProject) Delete() error {
	ctx, cancel := actionCtx()
	defer cancel()
	made, err := p.made(ctx)
	if err != nil {
		return err
	}
	if !p.a.gate.pass(made) {
		return nil
	}
	return p.a.api(ctx, http.MethodDelete, "/api/projects/b", nil, nil)
}

func (s *filingSession) assign(ctx context.Context, slug string) error {
	return s.a.api(ctx, http.MethodPost, "/api/sessions/"+url.PathEscape(s.id)+"/project", map[string]string{"project": slug}, nil)
}

// pid is the session's child: the one bough process resumed with its
// id (Start always resumes; nothing here creates a child without one).
func (s *filingSession) pid() (int, error) {
	out, err := exec.Command("ps", "-A", "-ww", "-o", "pid=,args=").Output()
	if err != nil {
		return 0, fmt.Errorf("ps: %w", err)
	}
	var pids []int
	for _, line := range strings.Split(string(out), "\n") {
		f := strings.Fields(line)
		if len(f) < 2 || !strings.Contains(line, " --headless ") {
			continue
		}
		if i := slices.Index(f, "-r"); i > 0 && i+1 < len(f) && f[i+1] == s.id {
			if pid, err := strconv.Atoi(f[0]); err == nil {
				pids = append(pids, pid)
			}
		}
	}
	if len(pids) != 1 {
		return 0, fmt.Errorf("want one child resumed with -r %s, found %v", s.id, pids)
	}
	return pids[0], nil
}

// injected is the project whose directory the running child was given
// (BOUGH_PROJECT_DIR, the base name of ~/.bough/projects/<slug>), read
// from the environment the process was started with: the child unsets
// it from its own view once read, but the kernel's copy is what it was
// exec'd with, which is exactly the spec's "injected".
func (s *filingSession) injected() (string, error) {
	pid, err := s.pid()
	if err != nil {
		return "", err
	}
	var env []string
	switch runtime.GOOS {
	case "linux":
		b, err := os.ReadFile(fmt.Sprintf("/proc/%d/environ", pid))
		if err != nil {
			return "", err
		}
		env = strings.Split(string(b), "\x00")
	default:
		// BSD ps: -E appends the environment to the command. The
		// paths here have no spaces (temp dirs and slugs).
		b, err := exec.Command("ps", "-E", "-ww", "-o", "command=", "-p", strconv.Itoa(pid)).Output()
		if err != nil {
			return "", fmt.Errorf("ps -E %d: %w", pid, err)
		}
		env = strings.Fields(string(b))
	}
	for _, kv := range env {
		if dir, ok := strings.CutPrefix(kv, "BOUGH_PROJECT_DIR="); ok {
			return filepath.Base(dir), nil
		}
	}
	return "", nil
}

// newProject is POST /api/projects, the Projects page's create.
func (a *sessionFilingAdapter) newProject(ctx context.Context, name string) (string, error) {
	var r struct {
		Project serve.Project `json:"project"`
	}
	err := a.api(ctx, http.MethodPost, "/api/projects", map[string]string{"name": name}, &r)
	return r.Project.Slug, err
}

// api is one call to serve's HTTP API as the page makes it. servetest
// has no project calls; this flow is their only user so far.
func (a *sessionFilingAdapter) api(ctx context.Context, method, path string, body, out any) error {
	var rd io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return err
		}
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, a.s.URL+path, rd)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
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
		return &servetest.APIError{Status: resp.StatusCode, Msg: strings.TrimSpace(string(raw))}
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(raw, out)
}

func isStatus(err error, code int) bool {
	var e *servetest.APIError
	return errors.As(err, &e) && e.Status == code
}

var sessionFilingActions = map[string]map[string]fmbt.ActionFunc{
	"Session": {
		"Assign": func(m any, args []fmbt.Arg) (any, error) {
			for _, arg := range args {
				if to, ok := arg.Value.(string); ok && arg.Name == "to" {
					return nil, m.(*filingSession).Assign(to)
				}
			}
			return nil, fmt.Errorf("Assign: no string argument \"to\" in %+v", args)
		},
		"Drop":     action((*filingSession).Drop),
		"FromRepo": action((*filingSession).FromRepo),
		"ApiMove":  action((*filingSession).ApiMove),
		"Start":    action((*filingSession).Start),
		"Stop":     action((*filingSession).Stop),
	},
	"Project": {
		"Delete": action((*filingProject).Delete),
	},
}

// Most steps are a POST, a Start is one short turn: 100 walks of up to
// 8 steps reach every action often enough that the wrong-adapter test
// cannot miss.
func sessionFilingOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

// sessionFilingHistory reads the local session's transcript: an input
// is a message, which is the child starting (Start) when it was not
// running, and a later input means the child went away in between
// (Stop is the only way the spec has back to not live that a
// transcript can show). Filing leaves nothing in history, so the check
// is on live alone.
func sessionFilingHistory(entries []history.Entry) []tracecheck.Step {
	live := func(v bool) map[string]any { return map[string]any{"Session#0.live": v} }
	steps := []tracecheck.Step{{Action: "Init", State: live(false)}}
	started := false
	for _, e := range entries {
		if e.Kind != "input" {
			continue
		}
		if started {
			steps = append(steps, tracecheck.Step{Action: "Session#0.Stop", State: live(false)})
		}
		steps = append(steps, tracecheck.Step{Action: "Session#0.Start", State: live(true)})
		started = true
	}
	return steps
}

func init() { historyProjections["session_filing"] = sessionFilingHistory }

func TestSessionFiling(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSessionFilingAdapter(t)
	if err := runMBT(t, "session_filing", a, sessionFilingActions, sessionFilingOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "session_filing"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), sessionFilingHistory)
	}
}

// A run that files nothing on "Create project from <repo>" must fail,
// or TestSessionFiling proves nothing.
func TestSessionFilingCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSessionFilingAdapter(t)
	a.skipFromRepoAssign = true
	if err := runMBT(t, "session_filing", a, sessionFilingActions, sessionFilingOptions()); err == nil {
		t.Fatal("a run whose FromRepo files nothing passed; the runner is not checking state")
	}
}

// The projection above is only worth its check if the graph can refuse
// it: a transcript with two messages is a path only because a Stop is
// read in between (a live child takes a second message without
// restarting, which the spec has no step for).
func TestSessionFilingHistory(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(fizzCheck(t, "session_filing"))
	if err != nil {
		t.Fatal(err)
	}
	entries := []history.Entry{
		{Seq: 1, Kind: "meta", Data: map[string]any{"mode": "local"}},
		{Seq: 2, Kind: "input"}, {Seq: 3, Kind: "done"},
		{Seq: 4, Kind: "input"}, {Seq: 5, Kind: "done"},
	}
	checkHistory(t, g, entries, sessionFilingHistory)
	live := map[string]any{"Session#0.live": true}
	twice := []tracecheck.Step{{Action: "Init"}, {Action: "Session#0.Start", State: live}, {Action: "Session#0.Start", State: live}}
	if v := g.Check(twice); v == nil {
		t.Fatal("Start twice without a Stop is a path in the model; the history check cannot fail")
	}
}
