//go:build !windows

package mbt

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/project_delete_with_project_sessions.fizz against a real serve:
// a project whose one repo is a remote (a bare repo under the serve's
// temp root, so the orb's cache clone, its bough/<session> branches and
// a push are all real git), its main thread and one thread, both in
// mode=project on the fake container runtime; then the project deleted
// and the slug created again.
//
// The spec's slug is "alpha". Every walk shares one serve per shard, so
// each walk takes its own project ("Alpha <n>" -> alpha-<n>) and its own
// repo (widget<n>): the sidebar, by-repo and the project page are read
// for this walk's sessions only, and a walk's leftovers are archived.
//
// DeleteProject is one HTTP call whose steps (EndProject, RemoveDirs,
// MetaUnassign) nothing can observe from outside. The adapter sends the
// DELETE at the spec's DeleteProject and reads serve before and after
// it; each phase then reports the fields its step changes from the
// after-read and the rest from the before-read, in the handler's order.
// Every field is observed, but the order inside the call
// (EndedBeforeRemoved) is fizz's and the code's, not this walk's.
//
// Fields the page owns (sidebar, page, byrepo, the from-repo counts,
// send) are the adapter's last read of the endpoint the page reads.
const pdConfig = "- id: llm\n  plugin: llm-control\n" +
	"- id: orb\n  plugin: orb\n  config:\n    runtime: fake\n"

// pdView is what the adapter reads off serve and git.
type pdView struct {
	disk, thread, meta, row, orbs, cache string
	lost                                 bool
}

type pdAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	walk, turn int
	slug, name string // this walk's "alpha"
	repo       string // the remote's name, which transcripts mention
	origin     string // the bare remote

	gen          int
	main, thread string
	held         string // the thread's turn in flight
	commits      []string

	phase                  string
	pre, post              pdView // around the DELETE
	sidebar, page, byrepo  string
	listed, moved, refusal int
	offered                int // the last by-repo read's group, which from-repo sets out to move
	send                   string

	kids []string // every thread a walk started, for the trace check

	// pushWrong is the deliberate wiring bug the wrong-adapter test
	// injects: ThreadPush pushes main's branch, not the thread's.
	pushWrong bool
}

func newPDAdapter(t *testing.T) *pdAdapter {
	s := servetest.Start(t, servetest.Options{Config: pdConfig})
	return &pdAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

// pdGit runs git for the adapter (the seed repo, the thread's agent)
// with no config but its own: the host's (signing, hooks, identity) is
// not the test's.
func pdGit(dir string, args ...string) (string, error) {
	cmd := exec.Command("git", append([]string{"-C", dir, "-c", "user.name=agent", "-c", "user.email=agent@example.invalid", "-c", "commit.gpgsign=false"}, args...)...)
	cmd.Env = append(os.Environ(), "GIT_CONFIG_NOSYSTEM=1", "GIT_CONFIG_GLOBAL=/dev/null")
	out, err := cmd.CombinedOutput()
	if err != nil {
		return "", fmt.Errorf("git %s: %w: %s", strings.Join(args, " "), err, out)
	}
	return strings.TrimSpace(string(out)), nil
}

// Init makes the walk's remote and its gen-1 project.
func (a *pdAdapter) Init() error {
	a.walk++
	a.name = fmt.Sprintf("Alpha %d", a.walk)
	a.repo = fmt.Sprintf("widget%d", a.walk)
	origins := filepath.Join(a.s.Root, "origins")
	seed := filepath.Join(origins, a.repo+"-seed")
	a.origin = filepath.Join(origins, a.repo+".git")
	if err := os.MkdirAll(seed, 0o755); err != nil {
		return err
	}
	if _, err := pdGit(seed, "init", "-q", "-b", "main"); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(seed, "README"), []byte("widget\n"), 0o644); err != nil {
		return err
	}
	if _, err := pdGit(seed, "add", "README"); err != nil {
		return err
	}
	if _, err := pdGit(seed, "commit", "-q", "-m", "seed"); err != nil {
		return err
	}
	if _, err := pdGit(origins, "clone", "-q", "--bare", seed, a.origin); err != nil {
		return err
	}
	var r struct {
		Project serve.Project `json:"project"`
	}
	if err := a.call(http.MethodPost, "/api/projects", map[string]string{"name": a.name}, &r); err != nil {
		return err
	}
	a.slug = r.Project.Slug
	yml := fmt.Sprintf("name: %s\nrepos:\n  - remote: %s\n", a.name, a.origin)
	if err := a.call(http.MethodPut, "/api/projects/"+a.slug+"/orb/files/project.yml", map[string]string{"text": yml}, nil); err != nil {
		return err
	}
	a.gen, a.main, a.thread, a.held, a.commits = 1, "", "", "", nil
	a.phase, a.pre, a.post = "", pdView{}, pdView{}
	a.sidebar, a.page, a.byrepo, a.send = "none", "", "", ""
	a.listed, a.moved, a.refusal, a.offered = 0, 0, 0, 0
	a.gate.reset()
	return nil
}

// Cleanup lets a held turn go and archives the walk's sessions, so no
// later walk's by-repo or turn queue sees them.
func (a *pdAdapter) Cleanup() error {
	if a.held != "" {
		control.Release(a.t, a.dir, a.held)
		a.held = ""
	}
	var errs []error
	for _, id := range []string{a.thread, a.main} {
		if id != "" {
			errs = append(errs, a.call(http.MethodPost, "/api/sessions/"+url.PathEscape(id)+"/archive", nil, nil))
		}
	}
	return errors.Join(errs...)
}

func (a *pdAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Project", Index: 0}: a}, nil
}

func (a *pdAdapter) call(method, path string, body, out any) error {
	ctx, cancel := actionCtx()
	defer cancel()
	return apiCall(ctx, a.s, method, path, body, out)
}

func (a *pdAdapter) histPath(id string) string {
	return filepath.Join(a.s.Home, ".bough", "history", id+".jsonl")
}

func (a *pdAdapter) cacheDir() string {
	return filepath.Join(a.s.Home, ".bough", "orbs", "cache", a.slug, a.repo+".git")
}

// mapSlug is a membership as the spec names it: "alpha" for this walk's
// slug, "" for none; anything else names itself and mismatches.
func (a *pdAdapter) mapSlug(p string) string {
	switch p {
	case a.slug:
		return "alpha"
	case "":
		return ""
	}
	return "in " + p
}

// both reads one value for main and the thread, which the spec keeps as
// one field: they must agree.
func both(what, m, t string) (string, error) {
	if m != t {
		return "", fmt.Errorf("%s: main says %q, the thread %q", what, m, t)
	}
	return m, nil
}

// view reads serve (and git) as the spec's fields.
func (a *pdAdapter) view() (pdView, error) {
	v := pdView{disk: "none", thread: "idle", orbs: "none", cache: "none"}
	var list struct {
		Projects []serve.Project `json:"projects"`
	}
	if err := a.call(http.MethodGet, "/api/projects", nil, &list); err != nil {
		return v, err
	}
	for _, p := range list.Projects {
		if p.Slug == a.slug {
			v.disk = "ok"
		}
	}
	if a.main != "" {
		ctx, cancel := actionCtx()
		defer cancel()
		mr, _, err := a.s.GetSession(ctx, a.main)
		if err != nil {
			return v, err
		}
		tr, _, err := a.s.GetSession(ctx, a.thread)
		if err != nil {
			return v, err
		}
		if tr.Status == serve.StatusRunning {
			v.thread = "running"
		}
		if v.row, err = both("row.project", a.mapSlug(mr.Project), a.mapSlug(tr.Project)); err != nil {
			return v, err
		}
		var m pmtMeta
		b, err := os.ReadFile(filepath.Join(a.s.Home, ".bough", "serve", "meta.json"))
		if err != nil {
			return v, err
		}
		if err := json.Unmarshal(b, &m); err != nil {
			return v, err
		}
		if v.meta, err = both("meta.json project", a.mapSlug(m.Sessions[a.main].Project), a.mapSlug(m.Sessions[a.thread].Project)); err != nil {
			return v, err
		}
		d, found, err := a.detail()
		if err != nil {
			return v, err
		}
		listed := map[string]bool{}
		if found {
			for _, o := range d.Orbs {
				listed[o.Session] = true
			}
		}
		switch {
		case listed[a.main] && listed[a.thread]:
			v.orbs = "alpha"
		case !listed[a.main] && !listed[a.thread]:
			v.orbs = "gone"
		default:
			return v, fmt.Errorf("orbs of %s list main %v, the thread %v", a.slug, listed[a.main], listed[a.thread])
		}
	}
	var err error
	if v.cache, v.lost, err = a.gitState(); err != nil {
		return v, err
	}
	return v, nil
}

// gitState is the cache clone ("none", "clean" when every bough/ branch
// in it is in the remote, "unpushed" otherwise) and whether a commit the
// thread made is now nowhere at all.
func (a *pdAdapter) gitState() (string, bool, error) {
	inOrigin := func(sha string) bool {
		_, err := pdGit(a.origin, "cat-file", "-e", sha+"^{commit}")
		return err == nil
	}
	cache := "none"
	gd := a.cacheDir()
	if _, err := os.Stat(gd); err == nil {
		out, err := pdGit(gd, "for-each-ref", "--format=%(objectname)", "refs/heads/bough/")
		if err != nil {
			return "", false, err
		}
		cache = "clean"
		for _, sha := range strings.Fields(out) {
			if !inOrigin(sha) {
				cache = "unpushed"
			}
		}
	}
	lost := false
	for _, sha := range a.commits {
		if inOrigin(sha) {
			continue
		}
		if _, err := pdGit(gd, "cat-file", "-e", sha+"^{commit}"); err != nil {
			lost = true
		}
	}
	return cache, lost, nil
}

// detail is GET /api/projects/<slug>; found is false on its 404.
func (a *pdAdapter) detail() (serve.ProjectDetail, bool, error) {
	var d serve.ProjectDetail
	err := a.call(http.MethodGet, "/api/projects/"+a.slug, nil, &d)
	var ae *servetest.APIError
	if errors.As(err, &ae) && ae.Status == http.StatusNotFound {
		return d, false, nil
	}
	return d, err == nil, err
}

// GetState is the Project role. Inside the DELETE's phases the fields
// each phase changes come from the read after the call; see pdConfig.
func (a *pdAdapter) GetState() (map[string]any, error) {
	var v pdView
	switch a.phase {
	case "":
		var err error
		if v, err = a.view(); err != nil {
			return nil, err
		}
	case "ending":
		v = a.pre
	case "removing":
		v = a.pre
		v.thread = a.post.thread
	case "unassign":
		v = a.pre
		v.thread, v.disk, v.cache, v.lost = a.post.thread, a.post.disk, a.post.cache, a.post.lost
	}
	sessions := "none"
	if a.main != "" {
		sessions = "started"
	}
	return map[string]any{
		"disk": v.disk, "gen": a.gen, "sessions": sessions, "thread": v.thread,
		"meta": v.meta, "row": v.row, "orbs": v.orbs, "cache": v.cache, "lost": v.lost,
		"phase": a.phase, "sidebar": a.sidebar, "page": a.page, "byrepo": a.byrepo,
		"listed": a.listed, "moved": a.moved, "refused": a.refusal, "send": a.send,
	}, nil
}

// live is the view outside a DELETE, for an action's require.
func (a *pdAdapter) live() pdView {
	v, err := a.view()
	if err != nil {
		a.t.Logf("walk %d: read for a require: %v", a.walk, err)
	}
	return v
}

// --- waits

// waitHistory polls a transcript until ok holds and no turn is open.
func (a *pdAdapter) waitHistory(id, what string, ok func([]history.Entry) bool) error {
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		es, err := history.Read(a.histPath(id))
		if err == nil && ok(es) && !turnOpen(es) {
			return nil
		}
		time.Sleep(20 * time.Millisecond)
	}
	return fmt.Errorf("%s %s: not settled after %s", what, id, actionTimeout)
}

func hasInput(text string) func([]history.Entry) bool {
	return func(es []history.Entry) bool {
		for _, e := range es {
			if e.Kind == "input" && e.Data["text"] == text {
				return true
			}
		}
		return false
	}
}

// reported waits for main to have woken on a report of every turn the
// thread closed, and for that turn of main's to close: an open one
// would take the next queued turn.
func (a *pdAdapter) reported() error {
	es, err := history.Read(a.histPath(a.thread))
	if err != nil {
		return err
	}
	closed := closedTurns(es)
	return a.waitHistory(a.main, "main", func(ms []history.Entry) bool {
		n := 0
		for _, e := range ms {
			if e.Kind == "input" && e.Data["reason"] == "notice" {
				if text, _ := e.Data["text"].(string); strings.Contains(text, a.thread) {
					n++
				}
			}
		}
		return n >= closed
	})
}

func (a *pdAdapter) queue(turn control.Turn) string {
	a.turn++
	name := fmt.Sprintf("d%05d", a.turn)
	control.Queue(a.t, a.dir, name, turn)
	return name
}

func (a *pdAdapter) waitTaken(name string) error {
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(filepath.Join(a.dir, name+".taken")); err == nil {
			return nil
		}
		time.Sleep(10 * time.Millisecond)
	}
	return fmt.Errorf("turn %s not taken after %s", name, actionTimeout)
}

// worktree is the thread's checkout of the repo, from its state.json.
func (a *pdAdapter) worktree(id string) (string, error) {
	st, err := orb.ReadState(a.s.Home, id)
	if err != nil {
		return "", err
	}
	wt := st.Worktrees[a.repo]
	if wt == "" {
		return "", fmt.Errorf("orb of %s has no worktree for %s: %+v", id, a.repo, st.Worktrees)
	}
	return wt, nil
}

// --- actions

// StartMainAndThread is a message to the project (main) and 'New
// thread' with a task; both name the repo, which is how by-repo later
// finds them.
func (a *pdAdapter) StartMainAndThread() error {
	if !a.gate.pass(a.phase == "" && a.gen == 1 && a.main == "" && a.live().disk == "ok") {
		return nil
	}
	text := "look at ~/repos/" + a.repo
	a.queue(control.Turn{Mode: "ok", Text: "looked"})
	var m struct {
		Main string `json:"main"`
	}
	if err := a.call(http.MethodPost, "/api/projects/"+a.slug+"/message", map[string]string{"text": text}, &m); err != nil {
		return err
	}
	a.main = m.Main
	if err := a.waitHistory(a.main, "main", hasInput(text)); err != nil {
		return err
	}
	task := "work in ~/repos/" + a.repo
	a.queue(control.Turn{Mode: "ok", Text: "worked"})
	var out struct {
		Session serve.Row `json:"session"`
	}
	if err := a.call(http.MethodPost, "/api/sessions", map[string]any{"mode": "project", "project": a.slug, "prompt": task}, &out); err != nil {
		return err
	}
	a.thread = out.Session.ID
	if a.thread == "" {
		return fmt.Errorf("new thread: no session id")
	}
	a.kids = append(a.kids, a.thread)
	if err := a.waitHistory(a.thread, "thread", func(es []history.Entry) bool { return hasInput(task)(es) && closedTurns(es) > 0 }); err != nil {
		return err
	}
	if err := a.reported(); err != nil {
		return err
	}
	for _, id := range []string{a.main, a.thread} {
		if _, err := a.worktree(id); err != nil {
			return err
		}
	}
	return nil
}

func (a *pdAdapter) ThreadRun() error {
	if !a.gate.pass(a.phase == "" && a.gen == 1 && a.main != "" && a.held == "" && a.live().disk == "ok") {
		return nil
	}
	name := a.queue(control.Turn{Mode: "block", Text: "finished"})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.thread, "turn "+name); err != nil {
		return err
	}
	if err := a.waitTaken(name); err != nil {
		return err
	}
	a.held = name
	_, err := waitRow(a.s, a.thread, "the thread running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
	return err
}

func (a *pdAdapter) ThreadFinish() error {
	if !a.gate.pass(a.phase == "" && a.held != "") {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	if _, err := waitRow(a.s, a.thread, "the thread's turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning }); err != nil {
		return err
	}
	return a.reported()
}

// ThreadCommit and ThreadPush are the thread's agent at work in its
// worktree, played by git directly.
func (a *pdAdapter) ThreadCommit() error {
	if !a.gate.pass(a.phase == "" && a.gen == 1 && a.main != "" && a.live().disk == "ok" && a.live().cache == "clean") {
		return nil
	}
	wt, err := a.worktree(a.thread)
	if err != nil {
		return err
	}
	f := fmt.Sprintf("change%d", len(a.commits)+1)
	if err := os.WriteFile(filepath.Join(wt, f), []byte(f+"\n"), 0o644); err != nil {
		return err
	}
	if _, err := pdGit(wt, "add", f); err != nil {
		return err
	}
	if _, err := pdGit(wt, "commit", "-q", "-m", f); err != nil {
		return err
	}
	sha, err := pdGit(wt, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	a.commits = append(a.commits, sha)
	return nil
}

func (a *pdAdapter) ThreadPush() error {
	if !a.gate.pass(a.phase == "" && a.gen == 1 && a.main != "" && a.live().disk == "ok" && a.live().cache == "unpushed") {
		return nil
	}
	wt, err := a.worktree(a.thread)
	if err != nil {
		return err
	}
	branch := "bough/" + a.thread
	if a.pushWrong {
		branch = "bough/" + a.main
	}
	_, err = pdGit(wt, "push", "-q", "origin", branch)
	return err
}

// DeleteProject is the dialog's confirm: the whole DELETE runs here, and
// the three phase steps after it report it (see pdConfig).
func (a *pdAdapter) DeleteProject() error {
	v := a.live()
	if !a.gate.pass(a.phase == "" && a.gen == 1 && v.disk == "ok" && v.cache != "unpushed") {
		return nil
	}
	a.pre = v
	if err := a.call(http.MethodDelete, "/api/projects/"+a.slug, nil, nil); err != nil {
		return err
	}
	// EndProject cut the turn in flight with its process.
	a.held = ""
	post, err := a.view()
	if err != nil {
		return err
	}
	a.post, a.phase = post, "ending"
	return nil
}

// DeleteRefusedUnpushed: the DELETE with an unpushed bough/<thread>
// branch in the cache clone must be a 409 that names the branch.
func (a *pdAdapter) DeleteRefusedUnpushed() error {
	v := a.live()
	if !a.gate.pass(a.phase == "" && a.gen == 1 && v.disk == "ok" && v.cache == "unpushed") {
		return nil
	}
	err := a.call(http.MethodDelete, "/api/projects/"+a.slug, nil, nil)
	if rerr := refused(http.StatusConflict, err); rerr != nil {
		return rerr
	}
	if branch := "bough/" + a.thread; !strings.Contains(err.Error(), branch) {
		return fmt.Errorf("the refusal does not name %s: %v", branch, err)
	}
	return nil
}

func (a *pdAdapter) EndProject() error {
	if a.gate.pass(a.phase == "ending") {
		a.phase = "removing"
	}
	return nil
}

func (a *pdAdapter) RemoveDirs() error {
	if a.gate.pass(a.phase == "removing") {
		a.phase = "unassign"
	}
	return nil
}

func (a *pdAdapter) MetaUnassign() error {
	if a.gate.pass(a.phase == "unassign") {
		a.phase, a.page = "", ""
	}
	return nil
}

// SidebarPoll is the page's /api/sessions poll, grouping by row.project.
func (a *pdAdapter) SidebarPoll() error {
	if a.phase != "" || a.main == "" {
		a.gate.pass(false)
		return nil
	}
	v := a.live()
	want := "loose"
	if v.row == "alpha" {
		want = "alpha"
	}
	if a.gate.pass(a.sidebar != want) {
		a.sidebar = want
	}
	return nil
}

// ProjectPagePoll reads #/projects/alpha's endpoint: a gen-2 project
// inherits when it lists gen 1's main, thread or their orbs.
func (a *pdAdapter) ProjectPagePoll() error {
	if !a.gate.pass(a.phase == "") {
		return nil
	}
	d, found, err := a.detail()
	if err != nil {
		return err
	}
	switch {
	case !found:
		a.page = "404"
	case a.gen == 2 && a.main != "" && a.inherits(d):
		a.page = "inherits"
	default:
		a.page = "empty"
	}
	return nil
}

func (a *pdAdapter) inherits(d serve.ProjectDetail) bool {
	old := []string{a.main, a.thread}
	if slices.Contains(old, d.Main) {
		return true
	}
	for _, r := range d.Threads {
		if slices.Contains(old, r.ID) {
			return true
		}
	}
	for _, o := range d.Orbs {
		if slices.Contains(old, o.Session) {
			return true
		}
	}
	return false
}

func (a *pdAdapter) PersonSendsToOldThread() error {
	if a.gate.pass(a.phase == "" && a.main != "" && a.send == "" && (a.gen == 2 || a.live().disk == "none")) {
		a.send = "sent"
	}
	return nil
}

// SendAnswered posts the message: a refusal is "refused"; a message
// taken is "orbfailed" when the respawned thread's orb failed, and
// "accepted" (no state of the spec's) when it booted anyway.
func (a *pdAdapter) SendAnswered() error {
	if !a.gate.pass(a.send == "sent") {
		return nil
	}
	at := time.Now()
	ctx, cancel := actionCtx()
	defer cancel()
	err := a.s.Prompt(ctx, a.thread, "are you still there?")
	var ae *servetest.APIError
	switch {
	case errors.As(err, &ae) && ae.Status/100 == 4:
		a.send = "refused"
		return nil
	case err != nil:
		return err
	}
	a.send = "accepted"
	for deadline := time.Now().Add(5 * time.Second); time.Now().Before(deadline); time.Sleep(50 * time.Millisecond) {
		if st, err := orb.ReadState(a.s.Home, a.thread); err == nil && st.Status == orb.StatusFailed && st.UpdatedAt.After(at) {
			a.send = "orbfailed"
			break
		}
	}
	return nil
}

// ByRepoRead is GET /api/projects/by-repo, for this walk's two sessions.
func (a *pdAdapter) ByRepoRead() error {
	if !a.gate.pass(a.phase == "") {
		return nil
	}
	var r struct {
		Groups []serve.RepoGroup `json:"groups"`
	}
	if err := a.call(http.MethodGet, "/api/projects/by-repo", nil, &r); err != nil {
		return err
	}
	a.byrepo, a.offered = "empty", 0
	for _, g := range r.Groups {
		has := func(id string) bool { return id != "" && slices.Contains(g.Sessions, id) }
		switch {
		case has(a.main) && has(a.thread) && g.Repo == a.repo:
			a.byrepo, a.offered = "orphans", len(g.Sessions)
		case has(a.main) || has(a.thread):
			return fmt.Errorf("by-repo lists main %v and the thread %v under %q: %+v", has(a.main), has(a.thread), g.Repo, g)
		}
	}
	return nil
}

// ProjectFromRepo is 'Create project from <repo>' named like gen 1, so
// it lands on the slug. listed is what the by-repo read before it
// offered.
func (a *pdAdapter) ProjectFromRepo() error {
	if !a.gate.pass(a.phase == "" && a.byrepo == "orphans" && a.live().disk == "none") {
		return nil
	}
	var r struct {
		Project serve.Project `json:"project"`
		Moved   int           `json:"moved"`
		Refused []struct {
			ID    string `json:"id"`
			Error string `json:"error"`
		} `json:"refused"`
	}
	if err := a.call(http.MethodPost, "/api/projects/from-repo", map[string]any{"repos": []string{a.repo}, "name": a.name}, &r); err != nil {
		return err
	}
	if r.Project.Slug != a.slug {
		return fmt.Errorf("from-repo made %q, not %q", r.Project.Slug, a.slug)
	}
	a.gen, a.listed, a.moved, a.refusal, a.byrepo, a.page = 2, a.offered, r.Moved, len(r.Refused), "", ""
	return nil
}

func (a *pdAdapter) CreateProject() error {
	if !a.gate.pass(a.phase == "" && a.live().disk == "none") {
		return nil
	}
	var r struct {
		Project serve.Project `json:"project"`
	}
	if err := a.call(http.MethodPost, "/api/projects", map[string]string{"name": a.name}, &r); err != nil {
		return err
	}
	if r.Project.Slug != a.slug {
		return fmt.Errorf("create made %q, not %q", r.Project.Slug, a.slug)
	}
	a.gen, a.byrepo, a.page = 2, "", ""
	return nil
}

func pdAction(name string, f func(*pdAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*pdAdapter)
		if a.gate.off {
			return nil, f(a)
		}
		start := time.Now()
		err := f(a)
		if !a.gate.off {
			a.t.Logf("walk %d: %s (%s) err=%v", a.walk, name, time.Since(start).Round(time.Millisecond), err)
		}
		return nil, err
	}
}

var pdActions = map[string]map[string]fmbt.ActionFunc{"Project": {
	"StartMainAndThread":     pdAction("StartMainAndThread", (*pdAdapter).StartMainAndThread),
	"ThreadRun":              pdAction("ThreadRun", (*pdAdapter).ThreadRun),
	"ThreadFinish":           pdAction("ThreadFinish", (*pdAdapter).ThreadFinish),
	"ThreadCommit":           pdAction("ThreadCommit", (*pdAdapter).ThreadCommit),
	"ThreadPush":             pdAction("ThreadPush", (*pdAdapter).ThreadPush),
	"DeleteProject":          pdAction("DeleteProject", (*pdAdapter).DeleteProject),
	"DeleteRefusedUnpushed":  pdAction("DeleteRefusedUnpushed", (*pdAdapter).DeleteRefusedUnpushed),
	"EndProject":             pdAction("EndProject", (*pdAdapter).EndProject),
	"RemoveDirs":             pdAction("RemoveDirs", (*pdAdapter).RemoveDirs),
	"MetaUnassign":           pdAction("MetaUnassign", (*pdAdapter).MetaUnassign),
	"SidebarPoll":            pdAction("SidebarPoll", (*pdAdapter).SidebarPoll),
	"ProjectPagePoll":        pdAction("ProjectPagePoll", (*pdAdapter).ProjectPagePoll),
	"PersonSendsToOldThread": pdAction("PersonSendsToOldThread", (*pdAdapter).PersonSendsToOldThread),
	"SendAnswered":           pdAction("SendAnswered", (*pdAdapter).SendAnswered),
	"ByRepoRead":             pdAction("ByRepoRead", (*pdAdapter).ByRepoRead),
	"ProjectFromRepo":        pdAction("ProjectFromRepo", (*pdAdapter).ProjectFromRepo),
	"CreateProject":          pdAction("CreateProject", (*pdAdapter).CreateProject),
}}

// projectDeleteHistory reads a main's or a thread's transcript as a
// path. Neither sees the page, the git side or the other session, so
// only what it records is checked:
//
//   - a thread (meta names a parent): its first closed turn is the task
//     'New thread' started it with (StartMainAndThread); each later
//     input is a ThreadRun and each close a ThreadFinish, and a turn
//     cancelled rather than done was cut by a delete (DeleteProject,
//     EndProject).
//   - main: its first input is the message that started it and the
//     thread (StartMainAndThread, whose report is main's first notice);
//     every later notice is one more thread turn (ThreadRun,
//     ThreadFinish).
//
// A refused message leaves nothing in either, which is the point.
func projectDeleteHistory(entries []history.Entry) []tracecheck.Step {
	step := func(action string, kv ...any) tracecheck.Step {
		m := map[string]any{}
		for i := 0; i+1 < len(kv); i += 2 {
			m["Project#0."+kv[i].(string)] = kv[i+1]
		}
		return tracecheck.Step{Action: "Project#0." + action, State: m}
	}
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Project#0.sessions": "none"}}}
	start := step("StartMainAndThread", "sessions", "started", "thread", "idle")
	isThread, started, open, notices := false, false, false, 0
	for _, e := range entries {
		switch e.Kind {
		case "meta":
			by, _ := e.Data["spawned_by"].(string)
			isThread = by != ""
		case "input":
			switch {
			case !isThread && e.Data["reason"] == "notice":
				notices++
				if notices > 1 {
					steps = append(steps, step("ThreadRun", "thread", "running"), step("ThreadFinish", "thread", "idle"))
				}
			case !isThread && !started:
				steps, started = append(steps, start), true
			case isThread && started:
				steps = append(steps, step("ThreadRun", "thread", "running"))
			}
			open = true
		case "done", "cancelled":
			// A turn closes once: a cut turn may record both.
			if !isThread || !open {
				continue
			}
			open = false
			switch {
			case !started:
				steps, started = append(steps, start), true
			case e.Kind == "done":
				steps = append(steps, step("ThreadFinish", "thread", "idle"))
			default:
				steps = append(steps, step("DeleteProject", "phase", "ending"), step("EndProject", "thread", "idle", "phase", "removing"))
			}
		}
	}
	return steps
}

func init() { historyProjections["project_delete_with_project_sessions"] = projectDeleteHistory }

func pdOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 12, "max-parallel-runs": 0}
}

// walkPath drives one path through the adapter, comparing the state it
// reads with the path's after every step.
func (a *pdAdapter) walkPath(trace []tracecheck.Step) (err error) {
	if err := a.Init(); err != nil {
		return err
	}
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	for i, s := range trace {
		name := strings.TrimPrefix(s.Action, "Project#0.")
		if i > 0 {
			f, ok := pdActions["Project"][name]
			if !ok {
				return fmt.Errorf("step %d: no action %q", i, name)
			}
			if _, err := f(a, nil); err != nil {
				return fmt.Errorf("step %d (%s): %w", i, name, err)
			}
			if a.gate.off {
				return fmt.Errorf("step %d (%s): the adapter reads it as not enabled", i, name)
			}
		}
		have, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w", i, name, err)
		}
		if diff := pmtDiff(s.State, have); diff != "" {
			return fmt.Errorf("step %d (%s): state differs from the spec's:%s", i, name, diff)
		}
	}
	return nil
}

func pdPaths(t *testing.T, cover tracecheck.Cover) [][]tracecheck.Step {
	t.Helper()
	b, err := pathsJSONCover("project_delete_with_project_sessions", cover)
	if err != nil {
		t.Fatal(err)
	}
	var file struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &file); err != nil {
		t.Fatal(err)
	}
	var out [][]tracecheck.Step
	for _, p := range file.Paths {
		out = append(out, p.Trace)
	}
	return out
}

func TestProjectDeleteWithProjectSessions(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPDAdapter(t)
	if err := runMBT(t, "project_delete_with_project_sessions", a, pdActions, pdOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// TestProjectDeleteWithProjectSessionsPaths walks every generated path
// against a real serve: four serves share them, and every thread's and
// main's transcript is then replayed on the graph.
func TestProjectDeleteWithProjectSessionsPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	paths := pdPaths(t, envCover())
	g, err := tracecheck.Load(fizzCheck(t, "project_delete_with_project_sessions"))
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("%d paths", len(paths))
	const shards = 4
	for n := range shards {
		t.Run(fmt.Sprintf("serve%d", n), func(t *testing.T) {
			t.Parallel()
			a := newPDAdapter(t)
			var mains []string
			for i := n; i < len(paths); i += shards {
				if err := a.walkPath(paths[i]); err != nil {
					t.Errorf("path %d: %v", i, err)
				}
				if a.main != "" {
					mains = append(mains, a.main)
				}
			}
			checked := 0
			for _, id := range append(mains, a.kids...) {
				if _, err := os.Stat(a.histPath(id)); err == nil {
					checkHistory(t, g, sessionHistory(t, a.s.Home, id), projectDeleteHistory)
					checked++
				}
			}
			t.Logf("trace-checked %d transcripts", checked)
		})
	}
}

// A ThreadPush that pushes main's branch leaves the thread's commits
// unpushed; the walk must say so on that transition. The state walks
// need not take ThreadPush at all (its target state is reached by other
// links), so this walks the first transition path that does.
func TestProjectDeleteWithProjectSessionsPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPDAdapter(t)
	a.pushWrong = true
	for i, p := range pdPaths(t, tracecheck.CoverTransitions) {
		if !slices.ContainsFunc(p, func(s tracecheck.Step) bool { return s.Action == "Project#0.ThreadPush" }) {
			continue
		}
		if err := a.walkPath(p); err != nil {
			t.Logf("path %d caught it: %v", i, err)
			return
		}
		t.Fatalf("path %d takes ThreadPush and passed with main's branch pushed; the walk is not checking state", i)
	}
	t.Fatal("no transition path takes ThreadPush")
}
