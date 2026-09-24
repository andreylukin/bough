//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/orb-image-build.fizz: one project's image build as the Projects
// -> Orb panel drives it (POST .../orb/build, the build log endpoint,
// the project list's orb summary), against serve's real API handler.
//
// Unlike the example this serve runs in-process (serve.NewSupervisor +
// serve.NewAPI behind httptest), not through servetest: the flow is
// about serve's build goroutine, and the only way to hold that
// goroutine at each of the spec's steps without a real container
// engine is a Runtime the test owns. A `bough serve` child picks
// container.Default(), the real Apple CLI on darwin. No model turn is
// part of this flow, so there is no llm row either.

// holdRuntime is container.Fake with the two calls serve's build
// goroutine makes parked until the adapter answers them: the first
// ImageExists of the project's tag (where the spec's SyncFail and
// Begin split) and the Commit that is the build itself (BuildOk,
// BuildFail). Nothing else holds: page reads carry a deadline, the
// goroutine's context.Background() does not, which is how they are
// told apart.
type holdRuntime struct {
	*container.Fake
	mu     sync.Mutex
	prefix string     // "bough-orb/<slug>:" of this walk's project
	armed  bool       // the next build ImageExists for prefix holds
	exists chan error // non-nil while that ImageExists is held
	commit chan error // non-nil while a Commit for prefix is held
}

func (h *holdRuntime) ImageExists(ctx context.Context, tag string) (bool, error) {
	_, deadline := ctx.Deadline()
	h.mu.Lock()
	if deadline || !h.armed || !strings.HasPrefix(tag, h.prefix) {
		h.mu.Unlock()
		return h.Fake.ImageExists(ctx, tag)
	}
	h.armed = false
	ch := make(chan error)
	h.exists = ch
	h.mu.Unlock()
	if err := <-ch; err != nil {
		return false, err
	}
	return h.Fake.ImageExists(ctx, tag)
}

func (h *holdRuntime) Commit(ctx context.Context, spec container.CommitSpec, log io.Writer) error {
	h.mu.Lock()
	// Never armed: every tag has the empty prefix, and nothing holds.
	if h.prefix == "" || !strings.HasPrefix(spec.Tag, h.prefix) {
		h.mu.Unlock()
		return h.Fake.Commit(ctx, spec, log)
	}
	ch := make(chan error)
	h.commit = ch
	h.mu.Unlock()
	if err := <-ch; err != nil {
		// A failed build leaves no tag behind: the spec assumes it (see
		// its gap B), so the fake's Commit, which would tag, is skipped.
		fmt.Fprintf(log, "fake build of %s: %v\n", spec.Tag, err)
		return err
	}
	return h.Fake.Commit(ctx, spec, log)
}

// held says where serve's build goroutine is parked: "syncing" in the
// first ImageExists, "building" in Commit, "idle" when in neither.
func (h *holdRuntime) held() string {
	h.mu.Lock()
	defer h.mu.Unlock()
	switch {
	case h.exists != nil:
		return "syncing"
	case h.commit != nil:
		return "building"
	}
	return "idle"
}

// release answers the held call as err says. The slot is cleared before
// the answer is sent, so held() never reports a call already let go.
func (h *holdRuntime) release(which string, err error) bool {
	h.mu.Lock()
	var ch chan error
	switch which {
	case "syncing":
		ch, h.exists = h.exists, nil
	case "building":
		ch, h.commit = h.commit, nil
	}
	h.mu.Unlock()
	if ch == nil {
		return false
	}
	ch <- err
	return true
}

func (h *holdRuntime) arm(slug string) {
	h.mu.Lock()
	h.prefix, h.armed = "bough-orb/"+slug+":", true
	h.mu.Unlock()
}

// orbImageBuildAdapter plays the Orb panel and the new-session dialog
// against one serve. It is the fmbt.Model and the spec's Project role.
type orbImageBuildAdapter struct {
	t    *testing.T
	rt   *holdRuntime
	srv  *httptest.Server
	home string
	gate gate

	walk    int
	edits   int
	slug    string // this walk's project
	yml     string // its valid project.yml, to restore after a broken edit
	confirm bool   // the "image build failed" dialog is up (page state)
	last    string // how the last serve build ended: the spec's ghost

	steps []tracecheck.Step // this walk as it happened, for the trace check
	walks [][]tracecheck.Step

	// The deliberate wiring bugs the CatchWrongAdapter tests inject:
	// failAsOK lets BuildFail's build succeed, noHold does not park the
	// build goroutine, so a Build runs straight through.
	failAsOK, noHold bool
}

func newOrbImageBuildAdapter(t *testing.T) *orbImageBuildAdapter {
	home := t.TempDir()
	hist := filepath.Join(home, ".bough", "history")
	if err := os.MkdirAll(hist, 0o755); err != nil {
		t.Fatal(err)
	}
	rt := &holdRuntime{Fake: container.NewFake()}
	// The base image exists, so a build is the project's Commit alone.
	rt.AddImage(projectdef.BaseTag())
	sup, err := serve.NewSupervisor(serve.Options{
		Exe: "/bin/true", HistDir: hist, Home: home, Runtime: rt,
		MetaPath: filepath.Join(home, ".bough", "serve", "meta.json"),
	})
	if err != nil {
		t.Fatal(err)
	}
	srv := httptest.NewServer(serve.NewAPI(sup))
	a := &orbImageBuildAdapter{t: t, rt: rt, srv: srv, home: home}
	t.Cleanup(func() {
		// A walk left parked would keep its goroutine forever.
		a.rt.release("syncing", errors.New("test over"))
		a.rt.release("building", errors.New("test over"))
		srv.Close()
		sup.Close()
	})
	return a
}

func (a *orbImageBuildAdapter) do(method, path, body string) (int, map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	var rdr io.Reader
	if body != "" {
		rdr = strings.NewReader(body)
	}
	req, err := http.NewRequestWithContext(ctx, method, a.srv.URL+path, rdr)
	if err != nil {
		return 0, nil, err
	}
	resp, err := a.srv.Client().Do(req)
	if err != nil {
		return 0, nil, err
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	var out map[string]any
	if len(raw) > 0 {
		if err := json.Unmarshal(raw, &out); err != nil {
			return resp.StatusCode, nil, fmt.Errorf("%s %s: %q is not JSON", method, path, raw)
		}
	}
	return resp.StatusCode, out, nil
}

// Init starts each walk on a new project: no build.json, no image.
func (a *orbImageBuildAdapter) Init() error {
	a.walk++
	name := fmt.Sprintf("w%04d", a.walk)
	code, body, err := a.do("POST", "/api/projects", `{"name":"`+name+`"}`)
	if err != nil {
		return err
	}
	if code != http.StatusOK {
		return fmt.Errorf("create project %s = %d %v", name, code, body)
	}
	p, _ := body["project"].(map[string]any)
	a.slug, _ = p["slug"].(string)
	b, err := os.ReadFile(a.file(projectdef.FileYAML))
	if err != nil {
		return err
	}
	a.yml, a.confirm, a.last = string(b), false, ""
	a.gate.reset()
	st, err := a.GetState()
	if err != nil {
		return err
	}
	a.steps = []tracecheck.Step{{Action: "Init", State: qualify(st)}}
	return nil
}

// Cleanup lets a build the walk left parked end, so its goroutine does
// not hold the runtime's slot into the next walk, then deletes the
// project: the list the adapter reads computes an orb summary for every
// project, and each walk's leftover made every later read slower.
func (a *orbImageBuildAdapter) Cleanup() error {
	a.walks = append(a.walks, a.steps)
	a.steps = nil
	stop := errors.New("walk over")
	if a.rt.release("syncing", stop) || a.rt.release("building", stop) {
		if err := a.settle(func(o obs) bool { return o.log != "building" }); err != nil {
			return err
		}
	}
	code, body, err := a.do("DELETE", "/api/projects/"+a.slug, "")
	if err == nil && code != http.StatusOK {
		err = fmt.Errorf("delete project %s = %d %v", a.slug, code, body)
	}
	return err
}

func (a *orbImageBuildAdapter) file(name string) string {
	return filepath.Join(projectdef.Root(a.home), a.slug, name)
}

func (a *orbImageBuildAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Project", Index: 0}: a}, nil
}

// obs is one reading of the server: the log endpoint, the project
// list's orb summary (what the page's failedBuild reads), build.json on
// disk, and where the build goroutine is parked.
type obs struct {
	log, logErr string // the log endpoint's state and error
	build       string // the list's orb.build
	built       bool
	valid       bool
	json        string
	phase       string
}

func (a *orbImageBuildAdapter) observe() (obs, error) {
	o := obs{phase: a.rt.held()}
	code, lb, err := a.do("GET", "/api/projects/"+a.slug+"/orb/build/log?offset=0", "")
	if err != nil {
		return o, err
	}
	if code != http.StatusOK {
		return o, fmt.Errorf("build log = %d %v", code, lb)
	}
	o.log, _ = lb["state"].(string)
	o.logErr, _ = lb["error"].(string)
	code, lp, err := a.do("GET", "/api/projects", "")
	if err != nil {
		return o, err
	}
	if code != http.StatusOK {
		return o, fmt.Errorf("projects = %d %v", code, lp)
	}
	found := false
	ps, _ := lp["projects"].([]any)
	for _, p := range ps {
		if m, _ := p.(map[string]any); m["slug"] == a.slug {
			sum, _ := m["orb"].(map[string]any)
			o.build, _ = sum["build"].(string)
			o.built, _ = sum["built"].(bool)
			e, _ := sum["error"].(string)
			o.valid, found = e == "", true
		}
	}
	if !found {
		return o, fmt.Errorf("project %s not listed", a.slug)
	}
	if b, err := orb.ReadBuild(a.home, a.slug); err == nil {
		o.json = b.State
	}
	return o, nil
}

// GetState is the Project role's state. runs is 1 while serve's
// goroutine is parked in the runtime (after every action it is either
// parked or gone). The log endpoint's state and the list's build are
// not role fields: the spec derives them (log_state, failed_view), so
// they are checked against that derivation here, and a mismatch is an
// error the runner reports on the step.
func (a *orbImageBuildAdapter) GetState() (map[string]any, error) {
	o, err := a.observe()
	if err != nil {
		return nil, err
	}
	runs := 0
	if o.phase != "idle" {
		runs = 1
	}
	st := map[string]any{
		"runs": runs, "phase": o.phase, "json": o.json, "built": o.built,
		"err": o.logErr != "", "last": a.last, "valid": o.valid, "confirm": a.confirm,
	}
	want := logState(o.json, o.logErr != "", runs)
	if o.log != want {
		return nil, fmt.Errorf("log endpoint state %q, the spec's log_state says %q (state %v)", o.log, want, st)
	}
	// orbSummary: the log's overrides, then built wins over "failed"
	// (a failure for another tag); a broken definition stops before.
	wantBuild := want
	if o.valid && o.built && wantBuild == "failed" {
		wantBuild = "ok"
	}
	if o.build != wantBuild {
		return nil, fmt.Errorf("project list orb.build %q, want %q (state %v)", o.build, wantBuild, st)
	}
	return st, nil
}

// logState is the spec's log_state.
func logState(json string, err bool, runs int) string {
	s := json
	if err {
		s = "failed"
	}
	if runs > 0 {
		s = "building"
	}
	return s
}

// settle polls until ok holds of the server; every action ends with it,
// so the next read is not of a goroutine halfway between two holds.
func (a *orbImageBuildAdapter) settle(ok func(obs) bool) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		o, err := a.observe()
		if err != nil {
			return err
		}
		if ok(o) {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("orb build did not settle: %+v", o)
		}
		time.Sleep(5 * time.Millisecond)
	}
}

func (a *orbImageBuildAdapter) Build() error {
	if !a.gate.pass(!a.confirm) {
		return nil
	}
	o, err := a.observe()
	if err != nil {
		return err
	}
	want := http.StatusAccepted
	switch {
	case !o.valid:
		want = http.StatusBadRequest
	case o.phase != "idle":
		want = http.StatusConflict
	case !a.noHold:
		a.rt.arm(a.slug)
	}
	code, body, err := a.do("POST", "/api/projects/"+a.slug+"/orb/build", `{}`)
	if err != nil {
		return err
	}
	if code != want {
		return fmt.Errorf("build = %d %v, want %d", code, body, want)
	}
	if want != http.StatusAccepted {
		return nil
	}
	a.last = ""
	if a.noHold {
		return a.settle(func(o obs) bool { return o.log != "building" })
	}
	return a.settle(func(o obs) bool { return o.phase == "syncing" })
}

// SyncFail: the clone, the lock or the first image check failed, before
// anything touched build.json.
func (a *orbImageBuildAdapter) SyncFail() error {
	if !a.gate.pass(a.rt.held() == "syncing") {
		return nil
	}
	a.rt.release("syncing", errors.New("fake: clone failed"))
	a.last = "failed"
	return a.settle(func(o obs) bool { return o.log != "building" })
}

// Begin lets the image check answer: an image that exists and did not
// fail ends the build there, anything else goes on to Commit.
func (a *orbImageBuildAdapter) Begin() error {
	if !a.gate.pass(a.rt.held() == "syncing") {
		return nil
	}
	a.rt.release("syncing", nil)
	var end obs
	if err := a.settle(func(o obs) bool {
		end = o
		return o.phase == "building" || o.log != "building"
	}); err != nil {
		return err
	}
	if end.phase != "building" {
		a.last = "ok"
	}
	return nil
}

func (a *orbImageBuildAdapter) BuildOk() error {
	if !a.gate.pass(a.rt.held() == "building") {
		return nil
	}
	a.rt.release("building", nil)
	a.last = "ok"
	return a.settle(func(o obs) bool { return o.log != "building" })
}

func (a *orbImageBuildAdapter) BuildFail() error {
	if !a.gate.pass(a.rt.held() == "building") {
		return nil
	}
	var err error
	if !a.failAsOK {
		err = errors.New("fake: setup.sh exited 1")
	}
	a.rt.release("building", err)
	a.last = "failed"
	return a.settle(func(o obs) bool { return o.log != "building" })
}

// Edit saves the definition the way an agent with the file tools can:
// either a setup.sh change (a new tag, so no image for it) or a
// project.yml that no longer loads, as the runner's choice of the
// spec's `oneof ok` says. Written to disk, not through the
// editor's PUT, which refuses a broken project.yml.
func (a *orbImageBuildAdapter) Edit(ok bool) error {
	if !a.gate.pass(a.rt.held() == "idle" && !a.confirm) {
		return nil
	}
	if !ok {
		return os.WriteFile(a.file(projectdef.FileYAML), []byte("repos: [\n"), 0o644)
	}
	a.edits++
	if err := os.WriteFile(a.file(projectdef.FileYAML), []byte(a.yml), 0o644); err != nil {
		return err
	}
	f, err := os.OpenFile(a.file(projectdef.FileSetup), os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	_, err = fmt.Fprintf(f, "# edit %d\n", a.edits)
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	return err
}

// StartSession is the new-session button on a project whose last build
// failed with no image standing in: the page's failedBuild, read off
// the project list, puts up confirmFailedBuild's dialog. The dialog is
// the page's; the server only answers the list.
func (a *orbImageBuildAdapter) StartSession() error {
	if a.confirm {
		a.gate.pass(false)
		return nil
	}
	o, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(o.valid && o.build == "failed" && !o.built) {
		return nil
	}
	a.confirm = true
	return nil
}

// StartAnyway and OpenOrb close the dialog; what the started session
// then builds is the session-start flow's.
func (a *orbImageBuildAdapter) StartAnyway() error {
	if a.gate.pass(a.confirm) {
		a.confirm = false
	}
	return nil
}

func (a *orbImageBuildAdapter) OpenOrb() error {
	if a.gate.pass(a.confirm) {
		a.confirm = false
	}
	return nil
}

// qualify names a role state the way the graph does: Project#0.<field>.
func qualify(st map[string]any) map[string]any {
	out := map[string]any{}
	for k, v := range st {
		out["Project#0."+k] = v
	}
	return out
}

// recorded wraps an action so that every step the gate let through is
// written down with the state read right after it: the walk as the
// server lived it, which the test replays on the graph itself rather
// than trusting only the runner's check.
func recorded(name string, f func(*orbImageBuildAdapter, []fmbt.Arg) error) fmbt.ActionFunc {
	return func(m any, args []fmbt.Arg) (any, error) {
		a := m.(*orbImageBuildAdapter)
		if err := f(a, args); err != nil || a.gate.off {
			return nil, err
		}
		st, err := a.GetState()
		if err != nil {
			return nil, err
		}
		a.steps = append(a.steps, tracecheck.Step{Action: "Project#0." + name, State: qualify(st)})
		return nil, nil
	}
}

func noArgs(f func(*orbImageBuildAdapter) error) func(*orbImageBuildAdapter, []fmbt.Arg) error {
	return func(a *orbImageBuildAdapter, _ []fmbt.Arg) error { return f(a) }
}

// editArg hands Edit the runner's value for the spec's `oneof ok`.
func editArg(a *orbImageBuildAdapter, args []fmbt.Arg) error {
	for _, arg := range args {
		if ok, isBool := arg.Value.(bool); arg.Name == "ok" && isBool {
			return a.Edit(ok)
		}
	}
	return fmt.Errorf("Edit: no bool choice \"ok\" in %v", args)
}

var orbImageBuildActions = map[string]map[string]fmbt.ActionFunc{"Project": {
	"Build":        recorded("Build", noArgs((*orbImageBuildAdapter).Build)),
	"SyncFail":     recorded("SyncFail", noArgs((*orbImageBuildAdapter).SyncFail)),
	"Begin":        recorded("Begin", noArgs((*orbImageBuildAdapter).Begin)),
	"BuildOk":      recorded("BuildOk", noArgs((*orbImageBuildAdapter).BuildOk)),
	"BuildFail":    recorded("BuildFail", noArgs((*orbImageBuildAdapter).BuildFail)),
	"Edit":         recorded("Edit", editArg),
	"StartSession": recorded("StartSession", noArgs((*orbImageBuildAdapter).StartSession)),
	"StartAnyway":  recorded("StartAnyway", noArgs((*orbImageBuildAdapter).StartAnyway)),
	"OpenOrb":      recorded("OpenOrb", noArgs((*orbImageBuildAdapter).OpenOrb)),
}}

// The runner picks each action uniformly from all nine, disabled ones
// included, and a walk's check ends at its first disabled pick: the
// failed-build dialog is five enabled steps deep, about one walk in
// 9^5. So the random run is the shallow net (Build, SyncFail, Edit, the
// 409 and 400) and TestOrbImageBuildPaths walks every transition.
func orbImageBuildOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

func TestOrbImageBuild(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newOrbImageBuildAdapter(t)
	if err := runMBT(t, "orb-image-build", a, orbImageBuildActions, orbImageBuildOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	// No session transcript comes out of this flow, so the trace check
	// replays the walks the adapter recorded instead.
	g, err := tracecheck.Load(fizzCheck(t, "orb-image-build"))
	if err != nil {
		t.Fatal(err)
	}
	steps := 0
	for _, w := range a.walks {
		if v := g.Check(w); v != nil {
			b, _ := json.Marshal(w)
			t.Errorf("walk is not a path in the model: %v\ntrace: %s", v, b)
		}
		steps += len(w) - 1
	}
	if steps == 0 {
		t.Fatal("no walk took an enabled step")
	}
	t.Logf("%d walks, %d enabled steps replayed on the graph", len(a.walks), steps)
}

// orbImageBuildPath is one entry of testdata/orb-image-build/paths.json:
// the generator's cover of every transition, the walks the browser
// spec takes.
type orbImageBuildPath struct {
	Trace []tracecheck.Step `json:"trace"`
}

// walkOrbImageBuildPaths drives the adapter down every generated path
// and returns the first step whose state is not the spec's.
func walkOrbImageBuildPaths(t *testing.T, a *orbImageBuildAdapter) error {
	t.Helper()
	b, err := os.ReadFile(filepath.Join(filepath.Dir(specPath("orb-image-build")), "..", "testdata", "orb-image-build", "paths.json"))
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []orbImageBuildPath `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		t.Fatal(err)
	}
	if len(doc.Paths) == 0 {
		t.Fatal("paths.json has no paths")
	}
	for pi, p := range doc.Paths {
		if err := a.Init(); err != nil {
			return fmt.Errorf("path %d: Init: %w", pi, err)
		}
		for si, s := range p.Trace {
			if si > 0 {
				name := strings.TrimPrefix(s.Action, "Project#0.")
				var args []fmbt.Arg
				if name == "Edit" {
					valid, _ := s.State["Project#0.valid"].(bool)
					args = []fmbt.Arg{{Name: "ok", Value: valid}}
				}
				if _, err := orbImageBuildActions["Project"][name](a, args); err != nil {
					return fmt.Errorf("path %d step %d (%s): %w", pi, si, s.Action, err)
				}
				if a.gate.off {
					return fmt.Errorf("path %d step %d (%s): the adapter's view says it is not enabled", pi, si, s.Action)
				}
			}
			got, err := a.GetState()
			if err != nil {
				return fmt.Errorf("path %d step %d (%s): %w", pi, si, s.Action, err)
			}
			if diff := stateDiff(s.State, qualify(got)); diff != "" {
				return fmt.Errorf("path %d step %d (%s): %s", pi, si, s.Action, diff)
			}
		}
		if err := a.Cleanup(); err != nil {
			return fmt.Errorf("path %d: Cleanup: %w", pi, err)
		}
	}
	return nil
}

// stateDiff compares the spec's role fields with the adapter's, through
// JSON so the spec's numbers and the adapter's ints compare equal.
func stateDiff(want, got map[string]any) string {
	var diffs []string
	for k, w := range want {
		if !strings.HasPrefix(k, "Project#0.") {
			continue // "project": the role reference itself
		}
		wj, _ := json.Marshal(w)
		gj, _ := json.Marshal(got[k])
		if string(wj) != string(gj) {
			diffs = append(diffs, fmt.Sprintf("%s: spec %s, server %s", k, wj, gj))
		}
	}
	return strings.Join(diffs, "; ")
}

// Every transition of the spec, in the generator's 41 paths: what the
// random run above cannot reach.
func TestOrbImageBuildPaths(t *testing.T) {
	t.Parallel()
	a := newOrbImageBuildAdapter(t)
	if err := walkOrbImageBuildPaths(t, a); err != nil {
		t.Fatal(err)
	}
}

// The runs above prove nothing unless a wrong server fails them. Here
// Build does not hold serve's goroutine, so the build runs straight
// through: the first Build of a walk already disagrees with the spec,
// and the random run reaches a Build in almost every run.
func TestOrbImageBuildCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newOrbImageBuildAdapter(t)
	a.noHold = true
	if err := runMBT(t, "orb-image-build", a, orbImageBuildActions, orbImageBuildOptions()); err == nil {
		t.Fatal("a run whose builds are not held passed; the runner is not checking state")
	}
}

// The deep bug, a failed build that reads as ok: only the path walk
// reaches BuildFail every time, so it is the one that must catch it.
func TestOrbImageBuildPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newOrbImageBuildAdapter(t)
	a.failAsOK = true
	err := walkOrbImageBuildPaths(t, a)
	if err == nil {
		t.Fatal("paths whose BuildFail succeeds passed; the walk is not checking state")
	}
	t.Logf("caught: %v", err)
}
