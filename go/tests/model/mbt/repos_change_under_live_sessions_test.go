//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"maps"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
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

// specs/repos_change_under_live_sessions.fizz: a project's repo list
// (project.yml) and setup.sh change while one of its sessions and that
// session's container exist.
//
// Not through servetest: a `bough serve` process, and the project child
// it spawns, use the host's container runtime, which this suite may
// never touch. So serve runs in process (the Supervisor and API `bough
// serve` builds, behind httptest) on a container.Fake, and the session
// child is played by the adapter with the real orb package on the same
// Fake: Prepare and Start are orb.Prepare and (*orb.Orb).Start, the
// orb row's two halves; ExecInGuest is an exec through (*orb.Orb).Command;
// Exit is the row's unmount (Stop when the start settled with an orb).
// The definition is edited through serve's orb file editor, Stop orb
// and Remove orb are serve's endpoints. What the adapter cannot run is
// the child's chdir into the primary (process-global), and the process
// exit itself: Exit records the owner as gone in state.json, which is
// all serve reads of a dead child.
//
// The two repos are real git checkouts, A and B, with B tracking the
// lockfile setup.sh can declare. One project serves every walk; each
// walk is a new session.

// rcRuntime is container.Fake that remembers the spec each container
// was created with: the Fake keeps only the last one, and the spec's
// mnt and img are fixed at create.
type rcRuntime struct {
	*container.Fake
	mu      sync.Mutex
	created map[string]container.RunSpec
	creates int
}

func (r *rcRuntime) Start(ctx context.Context, spec container.RunSpec) error {
	before, err := r.Fake.Inspect(ctx, spec.Name)
	if err != nil {
		return err
	}
	if err := r.Fake.Start(ctx, spec); err != nil {
		return err
	}
	if before == container.StateMissing {
		r.mu.Lock()
		r.created[spec.Name] = spec
		r.creates++
		r.mu.Unlock()
	}
	return nil
}

func (r *rcRuntime) Remove(ctx context.Context, name string) error {
	if err := r.Fake.Remove(ctx, name); err != nil {
		return err
	}
	r.mu.Lock()
	delete(r.created, name)
	r.mu.Unlock()
	return nil
}

func (r *rcRuntime) createdSpec(name string) (container.RunSpec, bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	s, ok := r.created[name]
	return s, ok
}

func (r *rcRuntime) createCount() int {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.creates
}

// rcGoneOwner is the pid Exit leaves in state.json: no process has it
// (above every pid_max), so serve reads the owner as gone, as it would
// the exited child's own pid.
const rcGoneOwner = 1 << 30

// rcLock is the file B tracks and setup.sh declares with `# bough:uses`.
const rcLock = "b.lock"

const rcSetup = "#!/bin/sh\nset -e\n# bough:step deps\necho deps\n"

func rcSetupText(uses bool) string {
	if !uses {
		return rcSetup
	}
	return strings.Replace(rcSetup, "# bough:step deps\n", "# bough:step deps\n# bough:uses "+rcLock+"\n", 1)
}

type reposChangeAdapter struct {
	t    *testing.T
	home string
	hist string
	rt   *rcRuntime
	sup  *serve.Supervisor
	srv  *httptest.Server
	gate gate

	slug  string
	repo  map[string]string // "A", "B" -> checkout path
	tag0  string            // the image of setup.sh without the declaration
	tagB  string            // with it; known once DeclareUses has run
	walks int

	id      string
	o       *orb.Orb // the child's orb; nil before a start settles or when it failed
	live    bool
	reused  bool
	removed string

	steps []tracecheck.Step
	done  [][]tracecheck.Step

	// The deliberate bugs the wrong-adapter tests inject:
	// exitKeepsContainer, a child that exits without stopping its orb
	// (deep: the walks reach it); addRepoFirst, an add-repo that puts the
	// new repo first (one step from Init, where the random runs, which
	// stop checking at their first disabled pick, still are).
	exitKeepsContainer, addRepoFirst bool
}

func newReposChangeAdapter(t *testing.T) *reposChangeAdapter {
	// Short, like servetest's: git worktree paths land under it.
	root, err := os.MkdirTemp("", "brc-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(root) })
	root, _ = filepath.EvalSymlinks(root)
	a := &reposChangeAdapter{t: t, home: filepath.Join(root, "home"), rt: &rcRuntime{Fake: container.NewFake(), created: map[string]container.RunSpec{}}}
	a.hist = filepath.Join(a.home, ".bough", "history")
	if err := os.MkdirAll(a.hist, 0o755); err != nil {
		t.Fatal(err)
	}
	a.repo = map[string]string{"A": filepath.Join(root, "src", "a"), "B": filepath.Join(root, "src", "b")}
	for name, dir := range a.repo {
		file, text := "README", "repo "+name+"\n"
		if name == "B" {
			file, text = rcLock, "lock v1\n"
		}
		if err := git(dir, "init", "-q"); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(dir, file), []byte(text), 0o644); err != nil {
			t.Fatal(err)
		}
		if err := git(dir, "add", "-A"); err != nil {
			t.Fatal(err)
		}
		if err := git(dir, "-c", "user.name=model", "-c", "user.email=model@example.invalid", "commit", "-q", "-m", "init"); err != nil {
			t.Fatal(err)
		}
	}
	// The base exists, so a build is the project's Commit alone.
	a.rt.AddImage(projectdef.BaseTag())
	a.sup, err = serve.NewSupervisor(serve.Options{
		Exe: "/bin/true", HistDir: a.hist, Home: a.home, Runtime: a.rt,
		MetaPath: filepath.Join(a.home, ".bough", "serve", "meta.json"),
	})
	if err != nil {
		t.Fatal(err)
	}
	a.srv = httptest.NewServer(serve.NewAPI(a.sup))
	t.Cleanup(func() {
		a.srv.Close()
		a.sup.Close()
	})
	code, body, err := a.do(http.MethodPost, "/api/projects", `{"name":"rc"}`)
	if err != nil || code != http.StatusOK {
		t.Fatalf("create project = %d %s %v", code, body, err)
	}
	var created struct {
		Project struct {
			Slug string `json:"slug"`
		} `json:"project"`
	}
	if err := json.Unmarshal(body, &created); err != nil {
		t.Fatal(err)
	}
	a.slug = created.Project.Slug
	if err := a.writeRepos("A"); err != nil {
		t.Fatal(err)
	}
	if err := a.putFile(projectdef.FileSetup, rcSetupText(false)); err != nil {
		t.Fatal(err)
	}
	if a.tag0, err = a.tag(); err != nil {
		t.Fatal(err)
	}
	return a
}

// tag is the image tag the definition on disk hashes to.
func (a *reposChangeAdapter) tag() (string, error) {
	p, err := projectdef.Load(a.home, a.slug)
	if err != nil {
		return "", err
	}
	h, err := projectdef.ImageHash(a.home, p)
	if err != nil {
		return "", err
	}
	return projectdef.ImageTag(a.slug, h), nil
}

func (a *reposChangeAdapter) do(method, path, body string) (int, []byte, error) {
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
	b, err := io.ReadAll(resp.Body)
	return resp.StatusCode, b, err
}

// putFile saves a definition file through the orb page's editor, which
// validates it as `bough project` does.
func (a *reposChangeAdapter) putFile(name, text string) error {
	b, _ := json.Marshal(map[string]string{"text": text})
	code, body, err := a.do(http.MethodPut, "/api/projects/"+a.slug+"/orb/files/"+name, string(b))
	if err != nil {
		return err
	}
	if code != http.StatusOK {
		return fmt.Errorf("save %s = %d %s", name, code, body)
	}
	return nil
}

// writeRepos saves project.yml with the repos in order ("A", "AB", "BA").
func (a *reposChangeAdapter) writeRepos(order string) error {
	var y strings.Builder
	y.WriteString("repos:\n")
	for _, r := range order {
		fmt.Fprintf(&y, "  - path: %s\n", a.repo[string(r)])
	}
	return a.putFile(projectdef.FileYAML, y.String())
}

// Init starts a walk on a new session of the project, whose definition
// is back to A alone with no declaration.
func (a *reposChangeAdapter) Init() error {
	a.walks++
	a.id = fmt.Sprintf("rc%05d", a.walks)
	a.o, a.live, a.reused, a.removed = nil, false, false, ""
	a.gate.reset()
	if err := a.writeRepos("A"); err != nil {
		return err
	}
	if err := a.putFile(projectdef.FileSetup, rcSetupText(false)); err != nil {
		return err
	}
	// The session serve lists: Stop orb answers only for a known one.
	meta, _ := json.Marshal(map[string]any{"seq": 1, "at": time.Now(), "kind": "meta",
		"data": map[string]any{"cwd": a.home, "mode": "project", "project": a.slug}})
	if err := os.WriteFile(filepath.Join(a.hist, a.id+".jsonl"), append(meta, '\n'), 0o644); err != nil {
		return err
	}
	st, err := a.GetState()
	if err != nil {
		return err
	}
	a.steps = []tracecheck.Step{{Action: "Init", State: rcQualify(st)}}
	return nil
}

// Cleanup ends the walk's child and removes its orb (worktrees, dir,
// container) so the next walk's project listing stays small.
func (a *reposChangeAdapter) Cleanup() error {
	a.done = append(a.done, a.steps)
	a.steps = nil
	ctx, cancel := actionCtx()
	defer cancel()
	if a.o != nil {
		a.o.Stop(ctx)
		a.o = nil
	}
	a.live = false
	return orb.Remove(ctx, a.rt, a.home, a.id)
}

func (a *reposChangeAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Orb", Index: 0}: a}, nil
}

// rcFields is the Orb role's state.
type rcFields struct {
	repos, file, st, primary, ctr, mnt, img, removed string
	uses, live, orphan, reused, dirty                bool
}

func (f rcFields) state() map[string]any {
	return map[string]any{
		"repos": f.repos, "uses": f.uses, "live": f.live, "file": f.file, "orphan": f.orphan,
		"st": f.st, "primary": f.primary, "ctr": f.ctr, "mnt": f.mnt, "img": f.img,
		"reused": f.reused, "dirty": f.dirty, "removed": f.removed,
	}
}

// has is the spec's `r in s` for a repo-set string.
func has(s, r string) bool { return strings.Contains(s, r) }

// repoSet is the spec's canonical set string of the session's worktree
// dirs among dirs; the container's other mounts (git dirs, the project
// dir) are skipped, and any other dir in the orb dir is an error.
func (a *reposChangeAdapter) repoSet(dirs []string) (string, error) {
	set := ""
	for _, r := range []string{"A", "B"} {
		if slices.Contains(dirs, filepath.Join(orb.Dir(a.home, a.id), strings.ToLower(r))) {
			set += r
		}
	}
	for _, d := range dirs {
		if base := filepath.Base(d); filepath.Dir(d) == orb.Dir(a.home, a.id) && base != "a" && base != "b" {
			return "", fmt.Errorf("worktree %s is neither repo", d)
		}
	}
	return set, nil
}

// observe reads the role's fields off the ground truth: project.yml and
// setup.sh, state.json, the runtime, the worktrees' git status. live,
// reused and removed are the child's and the walk's own.
func (a *reposChangeAdapter) observe() (rcFields, error) {
	f := rcFields{live: a.live, reused: a.reused, removed: a.removed}
	p, err := projectdef.Load(a.home, a.slug)
	if err != nil {
		return f, err
	}
	for _, r := range p.Def.Repos {
		switch r.Path {
		case a.repo["A"]:
			f.repos += "A"
		case a.repo["B"]:
			f.repos += "B"
		default:
			return f, fmt.Errorf("project.yml names %q", r.Path)
		}
	}
	setup, err := os.ReadFile(filepath.Join(p.Dir, projectdef.FileSetup))
	if err != nil {
		return f, err
	}
	f.uses = strings.Contains(string(setup), "# bough:uses")
	st, err := orb.ReadState(a.home, a.id)
	if err != nil {
		return f, err
	}
	f.file = "none"
	if st.Session != "" {
		switch {
		case st.Status == orb.StatusStarting && st.Phase == orb.PhaseWorktree:
			f.file = "prepared"
		case st.Status == orb.StatusRunning || st.Status == orb.StatusFailed || st.Status == orb.StatusStopped:
			f.file = string(st.Status)
		default:
			return f, fmt.Errorf("state.json settled at %s/%s", st.Status, st.Phase)
		}
	}
	var wts []string
	for _, d := range st.Worktrees {
		wts = append(wts, d)
	}
	if f.st, err = a.repoSet(wts); err != nil {
		return f, err
	}
	bwt := filepath.Join(orb.Dir(a.home, a.id), "b")
	if _, err := os.Stat(filepath.Join(bwt, ".git")); err == nil {
		f.orphan = !has(f.st, "B")
		out, err := exec.Command("git", "-C", bwt, "status", "--porcelain").Output()
		if err != nil {
			return f, fmt.Errorf("git status %s: %w", bwt, err)
		}
		f.dirty = strings.TrimSpace(string(out)) != ""
	}
	if a.live {
		if f.primary, err = a.repoSet([]string{st.Primary}); err != nil {
			return f, err
		}
	}
	name := container.OrbName(a.id)
	cs, err := a.rt.Inspect(context.Background(), name)
	if err != nil {
		return f, err
	}
	f.ctr = string(cs)
	if spec, ok := a.rt.createdSpec(name); ok {
		var ms []string
		for _, m := range spec.Mounts {
			ms = append(ms, m.Target)
		}
		if f.mnt, err = a.repoSet(ms); err != nil {
			return f, err
		}
		switch spec.Image {
		case a.tag0:
		case a.tagB:
			f.img = "B"
		default:
			return f, fmt.Errorf("container %s runs %s, neither the plain tag %s nor the declared one %s", name, spec.Image, a.tag0, a.tagB)
		}
	}
	return f, nil
}

func (a *reposChangeAdapter) GetState() (map[string]any, error) {
	f, err := a.observe()
	if err != nil {
		return nil, err
	}
	return f.state(), nil
}

func rcQualify(st map[string]any) map[string]any {
	out := map[string]any{}
	for k, v := range st {
		out["Orb#0."+k] = v
	}
	return out
}

// step gates an action on the spec's require, read off the ground
// truth, and runs do.
func (a *reposChangeAdapter) step(require func(rcFields) bool, do func(rcFields) error) error {
	f, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(require(f)) {
		return nil
	}
	return do(f)
}

// --- the definition ---

func (a *reposChangeAdapter) AddRepo() error {
	return a.step(func(f rcFields) bool { return !has(f.repos, "B") }, func(f rcFields) error {
		if a.addRepoFirst {
			return a.writeRepos("B" + f.repos)
		}
		return a.writeRepos(f.repos + "B")
	})
}

func (a *reposChangeAdapter) RemoveRepoB() error {
	return a.step(func(f rcFields) bool { return len(f.repos) == 2 }, func(rcFields) error {
		return a.writeRepos("A")
	})
}

func (a *reposChangeAdapter) Reorder() error {
	return a.step(func(f rcFields) bool { return len(f.repos) == 2 }, func(f rcFields) error {
		return a.writeRepos(f.repos[1:] + f.repos[:1])
	})
}

func (a *reposChangeAdapter) DeclareUses() error {
	return a.step(func(f rcFields) bool { return !f.uses && has(f.repos, "B") }, func(rcFields) error {
		if err := a.putFile(projectdef.FileSetup, rcSetupText(true)); err != nil {
			return err
		}
		tag, err := a.tag()
		if err != nil {
			return err
		}
		if tag == a.tag0 {
			return fmt.Errorf("declaring %s did not change the tag %s", rcLock, tag)
		}
		a.tagB = tag
		return nil
	})
}

func (a *reposChangeAdapter) DropUses() error {
	return a.step(func(f rcFields) bool { return f.uses }, func(rcFields) error {
		return a.putFile(projectdef.FileSetup, rcSetupText(false))
	})
}

// --- the session's child ---

func (a *reposChangeAdapter) Prepare() error {
	return a.step(func(f rcFields) bool { return !f.live }, func(rcFields) error {
		ctx, cancel := actionCtx()
		defer cancel()
		p, err := projectdef.Load(a.home, a.slug)
		if err != nil {
			return err
		}
		o, err := orb.Prepare(ctx, a.rt, a.home, a.id, p, "")
		if err != nil {
			return err
		}
		a.o, a.live, a.removed = o, true, ""
		return nil
	})
}

// Start is the container half. A start that fails leaves the child
// without an orb, as the row's handle settles it; the only failure the
// spec has is the declared lockfile no repo tracks.
func (a *reposChangeAdapter) Start() error {
	return a.step(func(f rcFields) bool { return f.live && f.file == "prepared" }, func(f rcFields) error {
		ctx, cancel := actionCtx()
		defer cancel()
		before := a.rt.createCount()
		if err := a.o.Start(ctx); err != nil {
			if f.uses && !has(f.repos, "B") && strings.Contains(err.Error(), "no repo tracks it") {
				a.o = nil
				return nil
			}
			return err
		}
		a.reused = a.rt.createCount() == before
		return nil
	})
}

// ExecInGuest is a tool's exec through the orb's seam, which starts the
// stopped container first.
func (a *reposChangeAdapter) ExecInGuest() error {
	return a.step(func(f rcFields) bool { return f.live && f.file == "stopped" && f.ctr == "stopped" }, func(rcFields) error {
		ctx, cancel := actionCtx()
		defer cancel()
		if out, err := a.o.Command(ctx, "true").CombinedOutput(); err != nil {
			return fmt.Errorf("exec in the orb: %w: %s", err, out)
		}
		return nil
	})
}

func (a *reposChangeAdapter) bWorktree() string { return filepath.Join(orb.Dir(a.home, a.id), "b") }

func (a *reposChangeAdapter) EditB() error {
	return a.step(func(f rcFields) bool { return f.live && f.file == "running" && has(f.st, "B") && !f.dirty }, func(rcFields) error {
		return os.WriteFile(filepath.Join(a.bWorktree(), fmt.Sprintf("work-%d.txt", time.Now().UnixNano())), []byte("work\n"), 0o644)
	})
}

func (a *reposChangeAdapter) CommitB() error {
	return a.step(func(f rcFields) bool { return f.live && f.file == "running" && has(f.st, "B") && f.dirty }, func(rcFields) error {
		if err := git(a.bWorktree(), "add", "-A"); err != nil {
			return err
		}
		return git(a.bWorktree(), "-c", "user.name=model", "-c", "user.email=model@example.invalid", "commit", "-q", "-m", "work")
	})
}

// StopOrb is the page's Stop orb (or the reaper's stop): the child stays.
func (a *reposChangeAdapter) StopOrb() error {
	return a.step(func(f rcFields) bool { return f.live && f.ctr == "running" }, func(rcFields) error {
		code, body, err := a.do(http.MethodPost, "/api/sessions/"+a.id+"/orb/stop", "")
		if err != nil {
			return err
		}
		if code != http.StatusOK {
			return fmt.Errorf("stop orb = %d %s", code, body)
		}
		return nil
	})
}

// Exit is the orb row's unmount (handle.close: Stop an orb whose start
// settled), then the process going.
func (a *reposChangeAdapter) Exit() error {
	return a.step(func(f rcFields) bool { return f.live && f.file != "prepared" }, func(rcFields) error {
		ctx, cancel := actionCtx()
		defer cancel()
		if a.o != nil && !a.exitKeepsContainer {
			if err := a.o.Stop(ctx); err != nil {
				return err
			}
		}
		st, err := orb.ReadState(a.home, a.id)
		if err != nil {
			return err
		}
		st.PID = rcGoneOwner
		b, err := json.Marshal(st)
		if err != nil {
			return err
		}
		if err := writeFileAtomic(filepath.Join(orb.Dir(a.home, a.id), "state.json"), b); err != nil {
			return err
		}
		a.o, a.live, a.reused = nil, false, false
		return nil
	})
}

// RemoveOrb is the page's Remove orb: DELETE, which answers 409 while a
// worktree state.json lists is dirty. Whether B's uncommitted work went
// with it is read off the host afterwards.
func (a *reposChangeAdapter) RemoveOrb() error {
	return a.step(func(f rcFields) bool { return !f.live && f.file != "none" }, func(f rcFields) error {
		want := http.StatusOK
		if has(f.st, "B") && f.dirty {
			want = http.StatusConflict
		}
		code, body, err := a.do(http.MethodDelete, "/api/sessions/"+a.id+"/orb", "")
		if err != nil {
			return err
		}
		if code != want {
			return fmt.Errorf("remove orb = %d %s, want %d", code, body, want)
		}
		if _, err := os.Stat(a.bWorktree()); f.dirty && errors.Is(err, os.ErrNotExist) {
			a.removed = "lost"
			if has(f.st, "B") {
				a.removed = "lost_listed"
			}
		}
		return nil
	})
}

// rcRecorded writes down every step the gate let through with the state
// read right after it: the walk as the server lived it, replayed on the
// graph by the tests.
func rcRecorded(name string, f func(*reposChangeAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*reposChangeAdapter)
		if err := f(a); err != nil || a.gate.off {
			return nil, err
		}
		st, err := a.GetState()
		if err != nil {
			return nil, err
		}
		a.steps = append(a.steps, tracecheck.Step{Action: "Orb#0." + name, State: rcQualify(st)})
		return nil, nil
	}
}

var reposChangeActions = map[string]map[string]fmbt.ActionFunc{"Orb": {
	"AddRepo":     rcRecorded("AddRepo", (*reposChangeAdapter).AddRepo),
	"RemoveRepoB": rcRecorded("RemoveRepoB", (*reposChangeAdapter).RemoveRepoB),
	"Reorder":     rcRecorded("Reorder", (*reposChangeAdapter).Reorder),
	"DeclareUses": rcRecorded("DeclareUses", (*reposChangeAdapter).DeclareUses),
	"DropUses":    rcRecorded("DropUses", (*reposChangeAdapter).DropUses),
	"Prepare":     rcRecorded("Prepare", (*reposChangeAdapter).Prepare),
	"Start":       rcRecorded("Start", (*reposChangeAdapter).Start),
	"ExecInGuest": rcRecorded("ExecInGuest", (*reposChangeAdapter).ExecInGuest),
	"EditB":       rcRecorded("EditB", (*reposChangeAdapter).EditB),
	"CommitB":     rcRecorded("CommitB", (*reposChangeAdapter).CommitB),
	"StopOrb":     rcRecorded("StopOrb", (*reposChangeAdapter).StopOrb),
	"Exit":        rcRecorded("Exit", (*reposChangeAdapter).Exit),
	"RemoveOrb":   rcRecorded("RemoveOrb", (*reposChangeAdapter).RemoveOrb),
}}

// Each step is a few git commands and file writes, no model turn, so
// walks can be long; the runner's picks are uniform over thirteen
// actions and a walk is checked up to its first disabled one.
func reposChangeOptions() map[string]any {
	return map[string]any{"max-seq-runs": 200, "max-actions": 12, "max-parallel-runs": 0}
}

// checkRecorded replays every walk the adapter recorded on the spec's
// graph: the trace check for a flow that leaves no transcript (the
// session's history holds only its meta; the flow is files, git and the
// runtime).
func (a *reposChangeAdapter) checkRecorded(g *tracecheck.Graph) (int, error) {
	steps := 0
	for _, w := range a.done {
		if v := g.Check(w); v != nil {
			b, _ := json.Marshal(w)
			return steps, fmt.Errorf("walk is not a path in the model: %v\ntrace: %s", v, b)
		}
		steps += len(w) - 1
	}
	return steps, nil
}

func reposChangeGraph(t *testing.T) *tracecheck.Graph {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("repos_change_under_live_sessions")), "..", "testdata", "repos_change_under_live_sessions"))
	if err != nil {
		t.Fatal(err)
	}
	return g
}

func TestReposChangeUnderLiveSessions(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newReposChangeAdapter(t)
	if err := runMBT(t, "repos_change_under_live_sessions", a, reposChangeActions, reposChangeOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	steps, err := a.checkRecorded(reposChangeGraph(t))
	if err != nil {
		t.Fatal(err)
	}
	if steps == 0 {
		t.Fatal("no walk took an enabled step")
	}
	t.Logf("%d walks, %d enabled steps replayed on the graph", len(a.done), steps)
}

// walkReposChangePaths drives the adapter down every walk over the
// checked-in graph and returns the first step whose state is not the
// spec's, then replays what it recorded on the graph.
func walkReposChangePaths(t *testing.T, a *reposChangeAdapter, cover tracecheck.Cover) error {
	t.Helper()
	b, err := pathsJSONCover("repos_change_under_live_sessions", cover)
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
	if len(doc.Paths) == 0 {
		t.Fatal("no walks over the graph")
	}
	for pi, p := range doc.Paths {
		if err := a.Init(); err != nil {
			return fmt.Errorf("walk %d: Init: %w", pi, err)
		}
		for si, s := range p.Trace {
			if si > 0 {
				name := strings.TrimPrefix(s.Action, "Orb#0.")
				act, ok := reposChangeActions["Orb"][name]
				if !ok {
					return fmt.Errorf("walk %d step %d: no action %s", pi, si, s.Action)
				}
				if _, err := act(a, nil); err != nil {
					return fmt.Errorf("walk %d step %d (%s): %w\nthe walk so far: %s", pi, si, s.Action, err, rcActions(p.Trace[:si]))
				}
				if a.gate.off {
					return fmt.Errorf("walk %d step %d (%s): the server's state says it is not enabled", pi, si, s.Action)
				}
			}
			// Init and every action recorded the state it read after.
			got := a.steps[len(a.steps)-1].State
			if diff := rcDiff(s.State, got); diff != "" {
				return fmt.Errorf("walk %d step %d (%s): %s\nthe walk so far: %s", pi, si, s.Action, diff, rcActions(p.Trace[:si]))
			}
		}
		if err := a.Cleanup(); err != nil {
			return fmt.Errorf("walk %d: Cleanup: %w", pi, err)
		}
	}
	if _, err := a.checkRecorded(reposChangeGraph(t)); err != nil {
		return err
	}
	t.Logf("%d walks (%s)", len(doc.Paths), cover)
	return nil
}

// rcActions names a walk's steps, for a failure's report.
func rcActions(trace []tracecheck.Step) string {
	var names []string
	for _, s := range trace {
		names = append(names, strings.TrimPrefix(s.Action, "Orb#0."))
	}
	return strings.Join(names, " ")
}

// rcDiff compares the spec's role fields with the adapter's through
// JSON, so numbers and strings compare as the spec wrote them.
func rcDiff(want, got map[string]any) string {
	var diffs []string
	for _, k := range slices.Sorted(maps.Keys(want)) {
		if !strings.HasPrefix(k, "Orb#0.") {
			continue // "orb": the role reference itself
		}
		wj, _ := json.Marshal(want[k])
		gj, _ := json.Marshal(got[k])
		if string(wj) != string(gj) {
			diffs = append(diffs, fmt.Sprintf("%s: spec %s, server %s", k, wj, gj))
		}
	}
	return strings.Join(diffs, "; ")
}

func TestReposChangeUnderLiveSessionsPaths(t *testing.T) {
	t.Parallel()
	a := newReposChangeAdapter(t)
	if err := walkReposChangePaths(t, a, envCover()); err != nil {
		t.Fatal(err)
	}
}

// A child that exits leaving its container running must fail the walks:
// the first Exit after a start already disagrees with the spec.
func TestReposChangeUnderLiveSessionsPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newReposChangeAdapter(t)
	a.exitKeepsContainer = true
	err := walkReposChangePaths(t, a, envCover())
	if err == nil {
		t.Fatal("walks whose Exit leaves the container running passed; the walk is not checking state")
	}
	t.Logf("caught: %v", err)
}

// The random runs rarely get past a start (13 uniform picks, checked to
// the first disabled one: 200 runs took about 35 enabled steps), so
// their wrong adapter is a shallow one: add-repo putting B first.
func TestReposChangeUnderLiveSessionsCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newReposChangeAdapter(t)
	a.addRepoFirst = true
	err := runMBT(t, "repos_change_under_live_sessions", a, reposChangeActions, reposChangeOptions())
	if err == nil {
		_, err = a.checkRecorded(reposChangeGraph(t))
	}
	if err == nil {
		t.Fatal("a run whose AddRepo puts the new repo first passed; the runner is not checking state")
	}
	t.Logf("caught: %.400v", err)
}
