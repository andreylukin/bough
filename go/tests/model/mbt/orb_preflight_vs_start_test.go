//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/cgi"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/secrets"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/orb_preflight_vs_start.fizz: the orb panel's preflight (GET
// /api/projects/<slug>/orb, orb.Preflight) against what a session start
// then does (orb.Prepare + Start), over one project with one repo, one
// secret and gh.
//
// Like orb-image-build, serve runs in process (serve.NewSupervisor +
// NewAPI behind httptest) with a fake container runtime: a `bough serve`
// child always opens the host's engine. The session start is played by
// the adapter the way plugins/orb does it (Prepare, then Start, under a
// context it cancels at the spec's 30-minute timeout), on the same
// runtime and home as serve. The panel is the page's state, kept by the
// adapter as projects.tsx keeps it: it re-reads when the list's orb
// summary changes.
//
// Everything the spec calls the world is real or a seam the adapter owns:
//   - the remote is a git smart-HTTP server (git http-backend over CGI)
//     the adapter can take offline (503) or put behind a credential
//     challenge (401). A git without GIT_TERMINAL_PROMPT=0 then asks
//     GIT_ASKPASS, which stands in for the terminal nobody answers: it
//     blocks until the walk ends, and says so in a marker file; with
//     prompts off it fails at once, as a closed terminal does.
//   - the path repo is a local checkout; the base ref is a branch the
//     definition names, created and deleted in the source (and in the
//     cache clone, which is a mirror of it: see the notes on the flow).
//   - the keychain (secrets.KeychainRead) and `gh auth token`
//     (orb.HostGitHubToken) are seams swapped once for the package; they
//     answer from the active adapter. Both are process-wide, so the
//     tests of this flow run one at a time (opvsMu).

// opvsMu serialises this flow's tests: the keychain, gh and GIT_ASKPASS
// they swap are process-wide.
var opvsMu sync.Mutex

// opvsActive is the adapter the keychain and gh seams answer for.
var opvsActive atomic.Pointer[opvsAdapter]

func init() {
	prevRead, prevGh := secrets.KeychainRead, orb.HostGitHubToken
	secrets.KeychainRead = func(service string) (string, error) {
		if a := opvsActive.Load(); a != nil && strings.HasPrefix(service, "bough/opvs-") {
			return a.keychain(service)
		}
		return prevRead(service)
	}
	orb.HostGitHubToken = func() string {
		if a := opvsActive.Load(); a != nil {
			return a.ghToken()
		}
		return prevGh()
	}
}

// opvsRuntime is the fake engine with a switch for "the runtime answers".
type opvsRuntime struct {
	*container.Fake
	down atomic.Bool
}

func (r *opvsRuntime) Available(ctx context.Context) error {
	if r.down.Load() {
		return errors.New("fake: the container system is not running (run: container system start)")
	}
	return r.Fake.Available(ctx)
}

// opvsRemote serves every bare repo under root over git's smart HTTP,
// or refuses as the spec's net says.
type opvsRemote struct {
	backend http.Handler
	mu      sync.Mutex
	net     string // online | offline | auth
}

func (r *opvsRemote) ServeHTTP(w http.ResponseWriter, req *http.Request) {
	r.mu.Lock()
	net := r.net
	r.mu.Unlock()
	switch net {
	case "offline":
		http.Error(w, "offline", http.StatusServiceUnavailable)
	case "auth":
		w.Header().Set("WWW-Authenticate", `Basic realm="opvs"`)
		http.Error(w, "credentials required", http.StatusUnauthorized)
	default:
		r.backend.ServeHTTP(w, req)
	}
}

func (r *opvsRemote) set(net string) {
	r.mu.Lock()
	r.net = net
	r.mu.Unlock()
}

// The credential a token URL carries; the leak check looks for it.
const (
	opvsUser  = "oauth2"
	opvsToken = "zq9TOKxv"
)

// opvsAskpass is the terminal nobody answers (see the file comment). A
// prompt for another server, or any prompt with prompts off, fails.
const opvsAskpass = `#!/bin/sh
[ "$GIT_TERMINAL_PROMPT" = 0 ] && exit 1
case "$1" in *"@HOST@"*) ;; *) exit 1 ;; esac
echo $$ > "@DIR@/$$"
exec sleep 3600
`

type opvsAdapter struct {
	t    *testing.T
	root string
	home string
	rt   *opvsRuntime
	sup  *serve.Supervisor
	srv  *httptest.Server // serve's API
	git  *httptest.Server // the remote
	rem  *opvsRemote
	gits string // bare repos served by git
	work string // the path repo's checkout
	ask  string // askpass marker dir
	gate gate

	n      int // names: walks, remotes, branches, sessions
	slug   string
	remote string // the current remote's bare repo name (r0001)
	branch string // the base branch the definition names

	// the world the adapter owns
	creds string

	// the page
	panel  string
	leak   bool
	loaded string // the list's orb summary the panel last loaded with

	// the latest start
	start, cause string
	session      string
	cancel       context.CancelFunc
	done         chan error
	orb          *orb.Orb
	asked        int // askpass markers when the start began

	steps []tracecheck.Step
	walks [][]tracecheck.Step

	// runtimeBlind is the deliberate wiring bug the wrong-adapter tests
	// inject: the start is handed a runtime that always answers.
	runtimeBlind bool
}

func newOpvsAdapter(t *testing.T) *opvsAdapter {
	opvsMu.Lock()
	t.Cleanup(opvsMu.Unlock)
	// A short root, like servetest: git and the proxy are fine with long
	// paths, the unix sockets under HOME are not.
	root, err := os.MkdirTemp("", "bopvs-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(root) })
	root, _ = filepath.EvalSymlinks(root)
	a := &opvsAdapter{t: t, root: root, home: filepath.Join(root, "home"),
		gits: filepath.Join(root, "gits"), work: filepath.Join(root, "src", "work"), ask: filepath.Join(root, "asked")}
	hist := filepath.Join(a.home, ".bough", "history")
	for _, d := range []string{hist, a.gits, a.ask} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	execPath, err := exec.Command("git", "--exec-path").Output()
	if err != nil {
		t.Fatal(err)
	}
	a.rem = &opvsRemote{net: "online", backend: &cgi.Handler{
		Path: filepath.Join(strings.TrimSpace(string(execPath)), "git-http-backend"),
		Env:  []string{"GIT_PROJECT_ROOT=" + a.gits, "GIT_HTTP_EXPORT_ALL=1"},
	}}
	a.git = httptest.NewServer(a.rem)
	askpass := filepath.Join(root, "askpass.sh")
	script := strings.NewReplacer("@HOST@", strings.TrimPrefix(a.git.URL, "http://"), "@DIR@", a.ask).Replace(opvsAskpass)
	if err := os.WriteFile(askpass, []byte(script), 0o755); err != nil {
		t.Fatal(err)
	}
	os.Setenv("GIT_ASKPASS", askpass)
	// No credential helper answers for this server: the host's (macOS
	// git's system config names osxkeychain) handed git a stored
	// 127.0.0.1 login, and on the 401 git asked the helper to erase it.
	// An empty helper resets the list; command-line scope comes last.
	os.Setenv("GIT_CONFIG_COUNT", "1")
	os.Setenv("GIT_CONFIG_KEY_0", "credential."+a.git.URL+".helper")
	os.Setenv("GIT_CONFIG_VALUE_0", "")
	a.git_(root, "init", "-q", "-b", "main", a.work)
	a.git_(a.work, "-c", "user.email=t@t", "-c", "user.name=t", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "base")

	a.rt = &opvsRuntime{Fake: container.NewFake()}
	a.rt.AddImage(projectdef.BaseTag())
	a.sup, err = serve.NewSupervisor(serve.Options{
		Exe: "/bin/true", HistDir: hist, Home: a.home, Runtime: a.rt,
		MetaPath: filepath.Join(a.home, ".bough", "serve", "meta.json"),
	})
	if err != nil {
		t.Fatal(err)
	}
	a.srv = httptest.NewServer(serve.NewAPI(a.sup))
	opvsActive.Store(a)
	t.Cleanup(func() {
		opvsActive.Store(nil)
		a.endStart()
		a.killAsked()
		for _, k := range []string{"GIT_ASKPASS", "GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"} {
			os.Unsetenv(k)
		}
		a.srv.Close()
		a.git.Close()
		a.sup.Close()
	})
	return a
}

// git_ runs git and fails the test: setup only, never inside an action.
func (a *opvsAdapter) git_(dir string, args ...string) {
	a.t.Helper()
	if out, err := exec.Command("git", append([]string{"-C", dir}, args...)...).CombinedOutput(); err != nil {
		a.t.Fatalf("git %v: %v\n%s", args, err, out)
	}
}

func gitIn(dir string, args ...string) error {
	out, err := exec.Command("git", append([]string{"-C", dir}, args...)...).CombinedOutput()
	if err != nil {
		return fmt.Errorf("git -C %s %v: %w: %s", dir, args, err, out)
	}
	return nil
}

func (a *opvsAdapter) keychain(service string) (string, error) {
	switch a.creds {
	case "secret_missing":
		return "", fmt.Errorf("%w: keychain service %q", secrets.ErrNotFound, service)
	case "secret_empty":
		return "", nil
	}
	return "k-value-1", nil
}

func (a *opvsAdapter) ghToken() string {
	if a.creds == "gh_missing" {
		return ""
	}
	return "gho_opvs"
}

func (a *opvsAdapter) name(prefix string) string {
	a.n++
	return fmt.Sprintf("%s%04d", prefix, a.n)
}

func (a *opvsAdapter) do(method, path, body string) (int, []byte, error) {
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
	raw, err := io.ReadAll(resp.Body)
	return resp.StatusCode, raw, err
}

// --- the definition ---

func (a *opvsAdapter) projectFile() string {
	return filepath.Join(projectdef.Root(a.home), a.slug, projectdef.FileYAML)
}

func (a *opvsAdapter) def() (projectdef.Project, error) {
	return projectdef.Load(a.home, a.slug)
}

func (a *opvsAdapter) repo() (projectdef.Repo, error) {
	p, err := a.def()
	if err != nil {
		return projectdef.Repo{}, err
	}
	if len(p.Def.Repos) != 1 {
		return projectdef.Repo{}, fmt.Errorf("project %s has %d repos", a.slug, len(p.Def.Repos))
	}
	return p.Def.Repos[0], nil
}

// writeDef saves project.yml the way an agent with the file tools does.
func (a *opvsAdapter) writeDef(r projectdef.Repo) error {
	var b strings.Builder
	b.WriteString("repos:\n")
	if r.Remote != "" {
		fmt.Fprintf(&b, "  - remote: %s\n", r.Remote)
	} else {
		fmt.Fprintf(&b, "  - path: %s\n", r.Path)
	}
	fmt.Fprintf(&b, "    branch: %s\n", r.Branch)
	b.WriteString("identity: [gh]\n")
	fmt.Fprintf(&b, "secrets:\n  API_KEY: keychain:bough/%s/API_KEY\n", a.slug)
	return os.WriteFile(a.projectFile(), []byte(b.String()), 0o644)
}

func (a *opvsAdapter) remoteURL(name string, token bool) string {
	host := strings.TrimPrefix(a.git.URL, "http://")
	if token {
		host = opvsUser + ":" + opvsToken + "@" + host
	}
	return "http://" + host + "/" + name + ".git"
}

// newRemote makes a bare repo with main and a fresh base branch.
func (a *opvsAdapter) newRemote() error {
	a.remote = a.name("r")
	bare := filepath.Join(a.gits, a.remote+".git")
	if err := gitIn(a.gits, "init", "-q", "--bare", "-b", "main", bare); err != nil {
		return err
	}
	if err := gitIn(a.work, "push", "-q", bare, "main:refs/heads/main"); err != nil {
		return err
	}
	a.branch = a.name("b")
	return gitIn(bare, "branch", a.branch, "main")
}

// sources are the git dirs the base branch lives in: the path checkout,
// or the remote and, once the start cloned it, its cache.
func (a *opvsAdapter) sources(r projectdef.Repo) []string {
	if r.Path != "" {
		return []string{r.Path}
	}
	out := []string{filepath.Join(a.gits, a.remote+".git")}
	if c := projectdef.CacheGitDir(a.home, a.slug, r); exists(c) {
		out = append(out, c)
	}
	return out
}

func exists(p string) bool { _, err := os.Stat(p); return err == nil }

// --- fmbt.Model ---

// Init starts each walk on a new project: a remote, online, never
// cloned, with a base branch; the runtime up, the keychain item and gh
// login there, the panel closed and no start.
func (a *opvsAdapter) Init() error {
	a.endStart()
	a.killAsked()
	name := a.name("opvs-w")
	code, body, err := a.do("POST", "/api/projects", `{"name":"`+name+`"}`)
	if err != nil {
		return err
	}
	if code != http.StatusOK {
		return fmt.Errorf("create project %s = %d %s", name, code, body)
	}
	var created struct {
		Project struct {
			Slug string `json:"slug"`
		} `json:"project"`
	}
	if err := json.Unmarshal(body, &created); err != nil {
		return err
	}
	a.slug = created.Project.Slug
	// resume.sh would run on the host through the fake's Command.
	os.Remove(filepath.Join(projectdef.Root(a.home), a.slug, projectdef.FileResume))
	if err := a.newRemote(); err != nil {
		return err
	}
	if err := a.writeDef(projectdef.Repo{Remote: a.remoteURL(a.remote, false), Branch: a.branch}); err != nil {
		return err
	}
	a.rt.down.Store(false)
	a.rem.set("online")
	a.creds = "ok"
	a.panel, a.leak, a.loaded = "closed", false, ""
	a.start, a.cause, a.session = "none", "", ""
	a.gate.reset()
	st, err := a.GetState()
	if err != nil {
		return err
	}
	a.steps = []tracecheck.Step{{Action: "Init", State: qualify(st)}}
	return nil
}

// Cleanup ends a start the walk left, then deletes the project: the
// list computes a summary for every project, and leftovers slow it.
func (a *opvsAdapter) Cleanup() error {
	a.walks = append(a.walks, a.steps)
	a.steps = nil
	a.endStart()
	a.killAsked()
	code, body, err := a.do("DELETE", "/api/projects/"+a.slug, "")
	if err == nil && code != http.StatusOK {
		err = fmt.Errorf("delete project %s = %d %s", a.slug, code, body)
	}
	return err
}

func (a *opvsAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Project", Index: 0}: a}, nil
}

// GetState is the Project role: the definition off disk, the base
// branch and the cache off git and the file system, the world's
// switches, the page, and the start as the adapter watched it.
func (a *opvsAdapter) GetState() (map[string]any, error) {
	r, err := a.repo()
	if err != nil {
		return nil, err
	}
	st := map[string]any{
		"rt": a.rt.Available(context.Background()) == nil, "creds": a.creds,
		"panel": a.panel, "leak": a.leak, "start": a.start, "cause": a.cause,
	}
	if r.Path != "" {
		st["repo"], st["token"], st["cached"], st["net"] = "path", false, false, "online"
	} else {
		st["repo"] = "remote"
		st["token"] = strings.Contains(r.Remote, "@")
		st["cached"] = exists(projectdef.CacheGitDir(a.home, a.slug, r))
		a.rem.mu.Lock()
		st["net"] = a.rem.net
		a.rem.mu.Unlock()
	}
	src := a.sources(r)[0]
	st["base"] = exec.Command("git", "-C", src, "rev-parse", "--verify", "--quiet", "refs/heads/"+r.Branch).Run() == nil
	return st, nil
}

// view is GetState for the gates.
func (a *opvsAdapter) view() map[string]any {
	st, err := a.GetState()
	if err != nil {
		return map[string]any{}
	}
	return st
}

// summary is the list's orb summary for the project, as projects.tsx
// keys its re-read on it: JSON.stringify(project.orb).
func (a *opvsAdapter) summary() (string, error) {
	code, body, err := a.do("GET", "/api/projects", "")
	if err != nil {
		return "", err
	}
	if code != http.StatusOK {
		return "", fmt.Errorf("projects = %d %s", code, body)
	}
	var list struct {
		Projects []struct {
			Slug string          `json:"slug"`
			Orb  json.RawMessage `json:"orb"`
		} `json:"projects"`
	}
	if err := json.Unmarshal(body, &list); err != nil {
		return "", err
	}
	for _, p := range list.Projects {
		if p.Slug == a.slug {
			return string(p.Orb), nil
		}
	}
	return "", fmt.Errorf("project %s not listed", a.slug)
}

// poll is the list poll: a panel showing a detail goes stale when the
// summary moved since it loaded, which is when the page's effect
// re-reads.
func (a *opvsAdapter) poll() error {
	if a.panel != "pass" && a.panel != "fail" {
		return nil
	}
	s, err := a.summary()
	if err != nil {
		return err
	}
	if s != a.loaded {
		a.panel = "stale"
	}
	return nil
}

// --- the panel ---

func (a *opvsAdapter) OpenOrbPanel() error {
	if !a.gate.pass(a.start == "none" && a.panel == "closed") {
		return nil
	}
	a.panel = "loading"
	return nil
}

func (a *opvsAdapter) ClosePanel() error {
	if !a.gate.pass(a.start == "none" && a.panel != "closed") {
		return nil
	}
	a.panel = "closed"
	return nil
}

func (a *opvsAdapter) ClickReCheck() error {
	if !a.gate.pass(a.start == "none" && a.panel == "fail") {
		return nil
	}
	a.panel = "loading"
	return nil
}

func (a *opvsAdapter) Reload() error {
	if !a.gate.pass(a.panel == "stale") {
		return nil
	}
	a.panel = "loading"
	return nil
}

// Answer is the detail GET. Its checks are compared, one by one, with
// what the spec's clone_check and failing say of the state before it.
func (a *opvsAdapter) Answer() error {
	if !a.gate.pass(a.panel == "loading") {
		return nil
	}
	v := a.view()
	code, body, err := a.do("GET", "/api/projects/"+a.slug+"/orb", "")
	if err != nil {
		return err
	}
	if code != http.StatusOK {
		return fmt.Errorf("orb detail = %d %s", code, body)
	}
	var d struct {
		Preflight []orb.PreflightCheck `json:"preflight"`
	}
	if err := json.Unmarshal(body, &d); err != nil {
		return err
	}
	got := map[string]string{}
	fail := false
	a.leak = false
	for _, c := range d.Preflight {
		got[c.Kind] = string(c.Status)
		fail = fail || c.Status == orb.PreflightFail
		a.leak = a.leak || strings.Contains(c.Detail, opvsToken)
	}
	want := opvsChecks(v)
	for k, w := range want {
		if got[k] != w {
			return fmt.Errorf("preflight %s check = %q, the spec says %q (checks %+v, state %v)", k, got[k], w, d.Preflight, v)
		}
	}
	if len(got) != len(want) {
		return fmt.Errorf("preflight checks %+v, want kinds %v", d.Preflight, want)
	}
	a.panel = "pass"
	if fail {
		a.panel = "fail"
	}
	a.loaded, err = a.summary()
	return err
}

// opvsChecks is each check's status as the spec computes it
// (clone_check, failing).
func opvsChecks(v map[string]any) map[string]string {
	c := map[string]string{"runtime": "ok", "clone": "ok", "gh": "ok", "secret": "ok"}
	if v["rt"] != true {
		c["runtime"] = "fail"
	}
	if v["repo"] == "remote" && v["net"] != "online" {
		c["clone"] = "fail"
		if v["cached"] == true {
			c["clone"] = "warn"
		}
	}
	switch v["creds"] {
	case "gh_missing":
		c["gh"] = "fail"
	case "secret_empty", "secret_missing":
		c["secret"] = "fail"
	}
	return c
}

// --- project.yml edits ---

func (a *opvsAdapter) EditRepoToPath() error {
	v := a.view()
	if !a.gate.pass(a.start == "none" && v["repo"] == "remote") {
		return nil
	}
	a.branch = a.name("b")
	if err := gitIn(a.work, "branch", a.branch, "main"); err != nil {
		return err
	}
	// A path repo keeps "online": the remote's switch is left there.
	a.rem.set("online")
	if err := a.writeDef(projectdef.Repo{Path: a.work, Branch: a.branch}); err != nil {
		return err
	}
	return a.poll()
}

// EditRepoToRemote names a remote never cloned (another name), as the
// spec's cached stays False; the cache is keyed by the repo's name.
func (a *opvsAdapter) EditRepoToRemote() error {
	v := a.view()
	if !a.gate.pass(a.start == "none" && v["repo"] == "path") {
		return nil
	}
	if err := a.newRemote(); err != nil {
		return err
	}
	if err := a.writeDef(projectdef.Repo{Remote: a.remoteURL(a.remote, false), Branch: a.branch}); err != nil {
		return err
	}
	return a.poll()
}

func (a *opvsAdapter) EditRemoteToken() error {
	v := a.view()
	if !a.gate.pass(a.start == "none" && v["repo"] == "remote") {
		return nil
	}
	tok := v["token"] != true
	if err := a.writeDef(projectdef.Repo{Remote: a.remoteURL(a.remote, tok), Branch: a.branch}); err != nil {
		return err
	}
	return a.poll()
}

// EditBaseRef points the definition at a branch that exists.
func (a *opvsAdapter) EditBaseRef() error {
	v := a.view()
	if !a.gate.pass(a.start == "none" && v["base"] != true) {
		return nil
	}
	r, err := a.repo()
	if err != nil {
		return err
	}
	a.branch = a.name("b")
	for _, src := range a.sources(r) {
		if err := gitIn(src, "branch", a.branch, "main"); err != nil {
			return err
		}
	}
	r.Branch = a.branch
	if err := a.writeDef(r); err != nil {
		return err
	}
	return a.poll()
}

// --- the world ---

func (a *opvsAdapter) world(enabled bool, move func() error) error {
	if !a.gate.pass(a.start == "none" && enabled) {
		return nil
	}
	if err := move(); err != nil {
		return err
	}
	return a.poll()
}

func (a *opvsAdapter) RuntimeStops() error {
	return a.world(!a.rt.down.Load(), func() error { a.rt.down.Store(true); return nil })
}

func (a *opvsAdapter) RuntimeStarts() error {
	return a.world(a.rt.down.Load(), func() error { a.rt.down.Store(false); return nil })
}

func (a *opvsAdapter) netMove(to string, enabled func(net string) bool) error {
	v := a.view()
	return a.world(v["repo"] == "remote" && enabled(fmt.Sprint(v["net"])), func() error { a.rem.set(to); return nil })
}

func (a *opvsAdapter) RemoteOffline() error {
	return a.netMove("offline", func(n string) bool { return n != "offline" })
}

func (a *opvsAdapter) RemoteOnline() error {
	return a.netMove("online", func(n string) bool { return n != "online" })
}

func (a *opvsAdapter) RemoteNeedsAuth() error {
	return a.netMove("auth", func(n string) bool { return n != "auth" })
}

// BaseRefMissing deletes the named branch where a start would look.
func (a *opvsAdapter) BaseRefMissing() error {
	v := a.view()
	return a.world(v["base"] == true, func() error {
		r, err := a.repo()
		if err != nil {
			return err
		}
		for _, src := range a.sources(r) {
			if err := gitIn(src, "branch", "-D", r.Branch); err != nil {
				return err
			}
		}
		return nil
	})
}

func (a *opvsAdapter) creds_(from []string, to string) error {
	ok := false
	for _, f := range from {
		ok = ok || a.creds == f
	}
	return a.world(ok, func() error { a.creds = to; return nil })
}

func (a *opvsAdapter) GhLoggedOut() error   { return a.creds_([]string{"ok"}, "gh_missing") }
func (a *opvsAdapter) GhLoggedIn() error    { return a.creds_([]string{"gh_missing"}, "ok") }
func (a *opvsAdapter) SecretEmptied() error { return a.creds_([]string{"ok"}, "secret_empty") }
func (a *opvsAdapter) SecretRemoved() error { return a.creds_([]string{"ok"}, "secret_missing") }
func (a *opvsAdapter) SecretStored() error {
	return a.creds_([]string{"secret_empty", "secret_missing"}, "ok")
}

// --- the session start ---

// askedCount is how many prompts the askpass has taken so far.
func (a *opvsAdapter) askedCount() int {
	es, _ := os.ReadDir(a.ask)
	return len(es)
}

// StartSession runs Prepare then Start as plugins/orb does, under a
// context the adapter ends at PromptTimesOut, and waits until the start
// ends or sits in a credential prompt.
func (a *opvsAdapter) StartSession() error {
	if !a.gate.pass(a.start == "none") {
		return nil
	}
	a.panel = "closed"
	p, err := a.def()
	if err != nil {
		return err
	}
	var rt container.Runtime = a.rt
	if a.runtimeBlind {
		rt = a.rt.Fake
	}
	a.session = a.name("s")
	ctx, cancel := context.WithCancel(context.Background())
	a.cancel, a.done, a.asked, a.orb = cancel, make(chan error, 1), a.askedCount(), nil
	scratch := filepath.Join(a.root, "scratch", a.session)
	go func() {
		o, err := orb.Prepare(ctx, rt, a.home, a.session, p, scratch)
		if err == nil {
			err = o.Start(ctx)
		}
		if err == nil {
			a.orb = o
		}
		a.done <- err
	}()
	deadline := time.Now().Add(actionTimeout)
	for {
		select {
		case err := <-a.done:
			a.done <- err
			return a.settled(err)
		default:
		}
		if a.askedCount() > a.asked {
			a.start, a.cause = "hang", "prompt"
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("start %s neither ended nor prompted in %s", a.session, actionTimeout)
		}
		time.Sleep(5 * time.Millisecond)
	}
}

// settled reads how the start ended: ok, or failed at the phase
// state.json says (none yet: the runtime; sync: the clone or fetch;
// worktree: addWorktree), or at the prompt if it asked for one.
func (a *opvsAdapter) settled(err error) error {
	if err == nil {
		a.start, a.cause = "ok", ""
		return nil
	}
	a.start = "fail"
	if a.askedCount() > a.asked {
		a.cause = "prompt"
		return nil
	}
	st, rerr := orb.ReadState(a.home, a.session)
	if rerr != nil {
		return fmt.Errorf("start failed (%v) and left no state: %w", err, rerr)
	}
	switch st.Phase {
	case "":
		a.cause = "runtime"
	case orb.PhaseSync:
		a.cause = "clone"
	case orb.PhaseWorktree:
		a.cause = "worktree"
	default:
		return fmt.Errorf("start failed at phase %q: %v", st.Phase, err)
	}
	return nil
}

// PromptTimesOut is plugins/orb's 30-minute context running out: the
// start must then end, however its git was stuck.
func (a *opvsAdapter) PromptTimesOut() error {
	if !a.gate.pass(a.start == "hang") {
		return nil
	}
	a.cancel()
	select {
	case err := <-a.done:
		a.done <- err
		if err == nil {
			return fmt.Errorf("a start stuck in a prompt succeeded once its context ended")
		}
		a.start = "fail"
		return nil
	case <-time.After(10 * time.Second):
		return fmt.Errorf("start %s did not end within 10s of its context ending", a.session)
	}
}

func (a *opvsAdapter) SessionEnds() error {
	if !a.gate.pass(a.start == "ok" || a.start == "fail") {
		return nil
	}
	a.endStart()
	a.killAsked()
	a.start, a.cause = "none", ""
	return nil
}

// endStart cancels the latest start, waits for it and stops its orb.
func (a *opvsAdapter) endStart() {
	if a.cancel == nil {
		return
	}
	a.cancel()
	select {
	case <-a.done:
	case <-time.After(10 * time.Second):
		a.killAsked() // a prompt still holding git's pipes
		<-a.done
	}
	if a.orb != nil {
		ctx, cancel := actionCtx()
		a.orb.Stop(ctx)
		cancel()
	}
	a.cancel, a.done, a.orb = nil, nil, nil
}

// killAsked ends every prompt the askpass is holding.
func (a *opvsAdapter) killAsked() {
	es, _ := os.ReadDir(a.ask)
	for _, e := range es {
		var pid int
		if _, err := fmt.Sscan(e.Name(), &pid); err == nil && pid > 0 {
			syscall.Kill(pid, syscall.SIGKILL)
		}
	}
}

// opvsRecorded journals every step the gate let through with the state
// read right after it, for the replay on the graph.
func opvsRecorded(name string, f func(*opvsAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*opvsAdapter)
		if err := f(a); err != nil || a.gate.off {
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

var opvsActions = func() map[string]map[string]fmbt.ActionFunc {
	fs := map[string]func(*opvsAdapter) error{
		"OpenOrbPanel": (*opvsAdapter).OpenOrbPanel, "ClosePanel": (*opvsAdapter).ClosePanel,
		"Answer": (*opvsAdapter).Answer, "ClickReCheck": (*opvsAdapter).ClickReCheck, "Reload": (*opvsAdapter).Reload,
		"EditRepoToPath": (*opvsAdapter).EditRepoToPath, "EditRepoToRemote": (*opvsAdapter).EditRepoToRemote,
		"EditRemoteToken": (*opvsAdapter).EditRemoteToken, "EditBaseRef": (*opvsAdapter).EditBaseRef,
		"RuntimeStops": (*opvsAdapter).RuntimeStops, "RuntimeStarts": (*opvsAdapter).RuntimeStarts,
		"RemoteOffline": (*opvsAdapter).RemoteOffline, "RemoteOnline": (*opvsAdapter).RemoteOnline,
		"RemoteNeedsAuth": (*opvsAdapter).RemoteNeedsAuth, "BaseRefMissing": (*opvsAdapter).BaseRefMissing,
		"GhLoggedOut": (*opvsAdapter).GhLoggedOut, "GhLoggedIn": (*opvsAdapter).GhLoggedIn,
		"SecretEmptied": (*opvsAdapter).SecretEmptied, "SecretRemoved": (*opvsAdapter).SecretRemoved,
		"SecretStored": (*opvsAdapter).SecretStored, "StartSession": (*opvsAdapter).StartSession,
		"PromptTimesOut": (*opvsAdapter).PromptTimesOut, "SessionEnds": (*opvsAdapter).SessionEnds,
	}
	out := map[string]fmbt.ActionFunc{}
	for n, f := range fs {
		out[n] = opvsRecorded(n, f)
	}
	return map[string]map[string]fmbt.ActionFunc{"Project": out}
}()

// Twenty-three actions share the runner's draw, most disabled at any
// node, so its walks stay shallow: the path walks below are the cover.
func opvsOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

// opvsHistory: no session transcript comes out of this flow (the start
// is the adapter's, and a session's history says nothing of preflight),
// so a transcript only says the walk began; the adapter's journal is
// the trace the graph replays.
func opvsHistory(entries []history.Entry) []tracecheck.Step {
	return []tracecheck.Step{{Action: "Init", State: map[string]any{"Project#0.start": "none"}}}
}

func init() { historyProjections["orb_preflight_vs_start"] = opvsHistory }

// replayJournal checks every walk the adapter journaled against the
// graph: the walk as the server lived it, not only the runner's view.
func (a *opvsAdapter) replayJournal(t *testing.T, g *tracecheck.Graph) {
	t.Helper()
	steps := 0
	for i, w := range a.walks {
		steps += len(w) - 1
		if v := g.Check(w); v != nil {
			b, _ := json.Marshal(w)
			t.Errorf("walk %d is not a path in the model: %v\ntrace: %s", i, v, b)
		}
	}
	if steps == 0 {
		t.Fatal("no walk took an enabled step")
	}
	t.Logf("%d walks, %d steps replayed on the graph", len(a.walks), steps)
}

func TestOrbPreflightVsStart(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	if envCover() != tracecheck.CoverTransitions {
		t.Skip("fizzbee-mbt random runs are part of the exhaustive run: MODEL_COVER=transitions")
	}
	a := newOpvsAdapter(t)
	if err := runMBT(t, "orb_preflight_vs_start", a, opvsActions, opvsOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "orb_preflight_vs_start"))
	if err != nil {
		t.Fatal(err)
	}
	a.replayJournal(t, g)
}

// walkOpvsPaths drives the adapter down every derived walk and returns
// the first step whose state is not the spec's.
func walkOpvsPaths(t *testing.T, a *opvsAdapter, cover tracecheck.Cover) error {
	t.Helper()
	b, err := pathsJSONCover("orb_preflight_vs_start", cover)
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
		t.Fatal("no walks")
	}
	steps := 0
	for pi, p := range doc.Paths {
		if err := a.Init(); err != nil {
			return fmt.Errorf("path %d: Init: %w", pi, err)
		}
		for si, s := range p.Trace {
			if si > 0 {
				name := strings.TrimPrefix(s.Action, "Project#0.")
				f := opvsActions["Project"][name]
				if f == nil {
					return fmt.Errorf("path %d step %d: no adapter action for %s", pi, si, s.Action)
				}
				if _, err := f(a, nil); err != nil {
					return fmt.Errorf("path %d step %d (%s): %w", pi, si, s.Action, err)
				}
				if a.gate.off {
					return fmt.Errorf("path %d step %d (%s): the adapter's view says it is not enabled (state %v)", pi, si, s.Action, a.view())
				}
				steps++
			}
			got, err := a.GetState()
			if err != nil {
				return fmt.Errorf("path %d step %d (%s): %w", pi, si, s.Action, err)
			}
			if diff := stateDiff(s.State, qualify(got)); diff != "" {
				return fmt.Errorf("path %d step %d (%s): %s\nthe walk so far: %v", pi, si, s.Action, diff, p.Trace[:si+1])
			}
		}
		if err := a.Cleanup(); err != nil {
			return fmt.Errorf("path %d: Cleanup: %w", pi, err)
		}
	}
	t.Logf("%d walks, %d steps", len(doc.Paths), steps)
	return nil
}

// TestOrbPreflightVsStartPaths walks every settled state (every
// transition under MODEL_COVER=transitions) against the server, then
// replays its journal on the graph.
func TestOrbPreflightVsStartPaths(t *testing.T) {
	t.Parallel()
	a := newOpvsAdapter(t)
	if err := walkOpvsPaths(t, a, envCover()); err != nil {
		t.Fatal(err)
	}
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("orb_preflight_vs_start")), "..", "testdata", "orb_preflight_vs_start"))
	if err != nil {
		t.Fatal(err)
	}
	a.replayJournal(t, g)
}

// The walks prove nothing unless a wrong wiring fails them. Here the
// start is handed a runtime that always answers, so a start after
// RuntimeStops gets past the runtime check: the spec's cause "runtime"
// is a settled state, which every walk cover reaches.
func TestOrbPreflightVsStartPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newOpvsAdapter(t)
	a.runtimeBlind = true
	err := walkOpvsPaths(t, a, tracecheck.CoverStates)
	if err == nil {
		t.Fatal("walks whose start ignores a stopped runtime passed; the walk is not checking state")
	}
	t.Logf("caught: %v", err)
}

// The random run must catch it too: RuntimeStops then StartSession is
// two enabled picks from Init.
func TestOrbPreflightVsStartCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newOpvsAdapter(t)
	a.runtimeBlind = true
	if err := runMBT(t, "orb_preflight_vs_start", a, opvsActions, opvsOptions()); err == nil {
		t.Fatal("a run whose start ignores a stopped runtime passed; the runner is not checking state")
	}
}
