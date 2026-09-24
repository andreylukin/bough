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
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/identity_lending.fizz against the real definition and orb code:
// serve's project editor (PUT .../orb/files/project.yml, which is
// projectdef.WriteFile, the same call `bough project add-identity`
// mutates through), serve's Stop orb and Remove orb, and orb.Prepare,
// Start, Command and Stop for the session's process.
//
// Not through servetest: a `bough serve` process and the session child
// it spawns use the host's container runtime, and this suite may never
// touch a real one. So serve runs in process over a recording
// container.Fake (as in orb_lifecycle_test.go), and the adapter plays the
// session's process by calling the orb package the way the child does.
// No turn is taken, so no llm row is involved.
//
// Read off the ground truth: d from project.yml, host from $HOME, ctr
// from the runtime, m from the RunSpec the runtime was given when it
// created the container. live is the adapter's own process. drift, stale
// and other are the spec's derived fields: the adapter keeps drift and
// stale by the spec's rules from the observed d, host and m, and reads
// other off the second project's definition.
//
// The relay itself is not driven: its proxy listens on the guest's
// gateway address, which a fake runtime has none of. RelayOtherSlug runs
// what the relay forwards unchecked, `bough project add-identity <other>
// gh`, through the same WriteFile.

// lendHazard is the hazard entry: a $HOME symlink to ~/.ssh, which
// CheckIdentity (exact top dir) and CheckHost and identityMounts
// (os.Stat follows links) all accept. Unlike a case variant (.SSH) it
// lends ~/.ssh on a case-sensitive filesystem too.
const lendHazard = "keys"

// lendRuntime is container.Fake that remembers, per container, the spec
// it was created with (a start of an existing one changes nothing), and
// the env of the last exec. Its guest has no gateway, so the orb starts
// no proxy: the lookup is answered with nothing instead of reading the
// host's resolv.conf.
type lendRuntime struct {
	*container.Fake
	mu      sync.Mutex
	created map[string]container.RunSpec
	execEnv map[string][]string
}

func newLendRuntime() *lendRuntime {
	return &lendRuntime{Fake: container.NewFake(), created: map[string]container.RunSpec{}, execEnv: map[string][]string{}}
}

func (r *lendRuntime) Start(ctx context.Context, spec container.RunSpec) error {
	st, err := r.Fake.Inspect(ctx, spec.Name)
	if err != nil {
		return err
	}
	if err := r.Fake.Start(ctx, spec); err != nil {
		return err
	}
	if st == container.StateMissing {
		r.mu.Lock()
		r.created[spec.Name] = spec
		r.mu.Unlock()
	}
	return nil
}

func (r *lendRuntime) Command(ctx context.Context, name string, opt container.ExecOptions, argv ...string) *exec.Cmd {
	if strings.Contains(strings.Join(argv, " "), "resolv.conf") {
		return r.Fake.Command(ctx, name, container.ExecOptions{}, "true")
	}
	r.mu.Lock()
	r.execEnv[name] = slices.Clone(opt.Env)
	r.mu.Unlock()
	return r.Fake.Command(ctx, name, opt, argv...)
}

func (r *lendRuntime) spec(name string) container.RunSpec {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.created[name]
}

// lastEnv is the env of the latest exec in name, and whether there was one.
func (r *lendRuntime) lastEnv(name string) ([]string, bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	env, ok := r.execEnv[name]
	return env, ok
}

// forgetEnv drops name's last exec: a new session has made none yet.
func (r *lendRuntime) forgetEnv(name string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	delete(r.execEnv, name)
}

// lendHost is the host the stubbed CLIs answer for: the gh login (a new
// token per login) for identity_lending_gh. Every orb in this package
// asks it, never the user's gh, gitconfig or ~/.zshrc.
var lendHost = struct {
	sync.Mutex
	token string
	n     int
}{}

var stubHostOnce sync.Once

func stubOrbHost() {
	stubHostOnce.Do(func() {
		orb.StubHost(func(name string, args ...string) string {
			if name == "gh" && slices.Equal(args, []string{"auth", "token"}) {
				lendHost.Lock()
				defer lendHost.Unlock()
				return lendHost.token
			}
			return ""
		}, func() map[string]string { return nil })
	})
}

// lendServe is serve's API in process over rt, for one adapter.
type lendServe struct {
	home, hist string
	rt         *lendRuntime
	sup        *serve.Supervisor
	srv        *httptest.Server
}

func newLendServe(t *testing.T) *lendServe {
	stubOrbHost()
	root, err := os.MkdirTemp("", "blend-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(root) })
	root, _ = filepath.EvalSymlinks(root)
	s := &lendServe{home: filepath.Join(root, "home"), rt: newLendRuntime()}
	s.hist = filepath.Join(s.home, ".bough", "history")
	if err := os.MkdirAll(s.hist, 0o755); err != nil {
		t.Fatal(err)
	}
	// serve spawns no child here; a session is the adapter's process.
	exe := filepath.Join(root, "child.sh")
	if err := os.WriteFile(exe, []byte("#!/bin/sh\nexit 1\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	s.sup, err = serve.NewSupervisor(serve.Options{
		Exe:      exe,
		HistDir:  s.hist,
		MetaPath: filepath.Join(s.home, ".bough", "serve", "meta.json"),
		Env:      []string{"HOME=" + s.home, "PATH=" + os.Getenv("PATH")},
		Runtime:  s.rt,
	})
	if err != nil {
		t.Fatal(err)
	}
	s.srv = httptest.NewServer(serve.NewAPI(s.sup))
	t.Cleanup(func() {
		s.srv.Close()
		s.sup.Close()
	})
	return s
}

// call sends one request with a JSON body and requires code back.
func (s *lendServe) call(method, path string, body any, code int) error {
	ctx, cancel := actionCtx()
	defer cancel()
	var r io.Reader
	if body != nil {
		b, _ := json.Marshal(body)
		r = strings.NewReader(string(b))
	}
	req, err := http.NewRequestWithContext(ctx, method, s.srv.URL+path, r)
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := s.srv.Client().Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != code {
		var e struct {
			Error string `json:"error"`
		}
		json.NewDecoder(resp.Body).Decode(&e)
		return fmt.Errorf("%s %s = %d (%s), want %d", method, path, resp.StatusCode, e.Error, code)
	}
	return nil
}

// saveYAML is the editor's Save of project.yml.
func (s *lendServe) saveYAML(slug, text string, code int) error {
	return s.call(http.MethodPut, "/api/projects/"+slug+"/orb/files/"+projectdef.FileYAML, map[string]string{"text": text}, code)
}

// defYAML is a definition with no repos listing identity.
func defYAML(identity ...string) string {
	if len(identity) == 0 {
		return "repos: []\n"
	}
	return "repos: []\nidentity: [" + strings.Join(identity, ", ") + "]\n"
}

// newProjectSession makes a project and a session filed in it, as serve
// knows one: its history's meta entry.
func (s *lendServe) newProjectSession(slug, id string) error {
	if _, err := projectdef.Create(s.home, slug); err != nil {
		return err
	}
	if err := projectdef.WriteFile(s.home, slug, projectdef.FileYAML, defYAML()); err != nil {
		return err
	}
	b, _ := json.Marshal(history.Entry{Seq: 1, Kind: "meta", Data: map[string]any{"cwd": s.home, "mode": "project", "project": slug}})
	return os.WriteFile(filepath.Join(s.hist, id+".jsonl"), append(b, '\n'), 0o644)
}

// startSession is the session process's start: the definition as it is
// now, Prepare, Start.
func (s *lendServe) startSession(slug, id string) (*orb.Orb, projectdef.Project, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	p, err := projectdef.Load(s.home, slug)
	if err != nil {
		return nil, p, err
	}
	o, err := orb.Prepare(ctx, s.rt, s.home, id, p, "")
	if err != nil {
		return nil, p, err
	}
	if err := o.Start(ctx); err != nil {
		return nil, p, err
	}
	return o, p, nil
}

// exitSession is the session process ending: Orb.Stop, then the process
// is gone. The orb runs in this test's process, so state.json's pid is
// pointed at one that has exited, or serve would take this process for a
// live owner and refuse Remove orb.
func (s *lendServe) exitSession(o *orb.Orb, id string) error {
	ctx, cancel := actionCtx()
	defer cancel()
	if err := o.Stop(ctx); err != nil {
		return err
	}
	dead := exec.Command("true")
	if err := dead.Run(); err != nil {
		return err
	}
	path := filepath.Join(orb.Dir(s.home, id), "state.json")
	b, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	var st map[string]any
	if err := json.Unmarshal(b, &st); err != nil {
		return err
	}
	st["pid"] = dead.Process.Pid
	b, _ = json.Marshal(st)
	return writeFileAtomic(path, b)
}

// lendFields is the Lend role's state.
type lendFields struct {
	d, ctr, m                string
	host, live, drift, stale bool
	other                    bool
}

func (f lendFields) state() map[string]any {
	return map[string]any{"d": f.d, "host": f.host, "live": f.live, "ctr": f.ctr, "m": f.m,
		"drift": f.drift, "stale": f.stale, "other": f.other}
}

// want is the spec's want(): what identityMounts would mount now.
func (f lendFields) want() string {
	if f.d == "none" || !f.host {
		return "none"
	}
	return f.d
}

// identityKind classifies a definition's identity list as the spec's d.
func identityKind(ids []string) string {
	for _, e := range ids {
		switch dir, _ := projectdef.IdentityDir(e); dir {
		case ".aws":
			return "ok"
		case lendHazard:
			return "hazard"
		}
	}
	return "none"
}

// mountKind classifies a container's mounts as the spec's m.
func mountKind(ms []container.Mount) string {
	for _, m := range ms {
		switch m.Target {
		case "/root/.aws":
			return "ok"
		case "/root/" + lendHazard:
			return "hazard"
		}
	}
	return "none"
}

type lendAdapter struct {
	t *testing.T
	s *lendServe

	n               int
	id, slug, other string
	o               *orb.Orb
	drift, stale    bool
	gate            gate
	action          string
	trace           []tracecheck.Step
	journal         [][]tracecheck.Step
	did             map[string]int

	// startRecreates is the deliberate bug the wrong-adapter test
	// injects: a start that removes a stopped container first, so it
	// always gets the current mounts.
	startRecreates bool
}

func newLendAdapter(t *testing.T) *lendAdapter {
	a := &lendAdapter{t: t, s: newLendServe(t), did: map[string]int{}}
	if err := os.Symlink(".ssh", filepath.Join(a.s.home, lendHazard)); err != nil {
		t.Fatal(err)
	}
	return a
}

// Init starts each walk on a fresh project with nothing listed, its
// session down and never started, and the dirs present on the host.
func (a *lendAdapter) Init() error {
	a.n++
	a.id, a.slug, a.other = fmt.Sprintf("lend%03d", a.n), fmt.Sprintf("p%03d", a.n), fmt.Sprintf("q%03d", a.n)
	a.o, a.drift, a.stale = nil, false, false
	a.gate.reset()
	a.trace = nil
	if err := a.hostDirs(true); err != nil {
		return err
	}
	if err := a.s.newProjectSession(a.slug, a.id); err != nil {
		return err
	}
	if _, err := projectdef.Create(a.s.home, a.other); err != nil {
		return err
	}
	if err := projectdef.WriteFile(a.s.home, a.other, projectdef.FileYAML, defYAML()); err != nil {
		return err
	}
	a.action = "Init"
	return nil
}

// hostDirs makes or deletes ~/.aws and the ~/.ssh the hazard links to.
func (a *lendAdapter) hostDirs(present bool) error {
	for _, d := range []string{".aws", ".ssh"} {
		p := filepath.Join(a.s.home, d)
		var err error
		if present {
			err = os.MkdirAll(p, 0o755)
		} else {
			err = os.RemoveAll(p)
		}
		if err != nil {
			return err
		}
	}
	return nil
}

func (a *lendAdapter) Cleanup() error {
	if a.o != nil {
		ctx, cancel := actionCtx()
		a.o.Stop(ctx)
		cancel()
		a.o = nil
	}
	if len(a.trace) > 0 {
		a.journal = append(a.journal, a.trace)
		a.trace = nil
	}
	return nil
}

func (a *lendAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Lend", Index: 0}: a}, nil
}

func (a *lendAdapter) observe() (lendFields, error) {
	f := lendFields{live: a.o != nil, drift: a.drift, stale: a.stale}
	p, err := projectdef.Load(a.s.home, a.slug)
	if err != nil {
		return f, fmt.Errorf("load %s: %w", a.slug, err)
	}
	f.d = identityKind(p.Def.Identity)
	fi, err := os.Stat(filepath.Join(a.s.home, ".aws"))
	f.host = err == nil && fi.IsDir()
	cs, err := a.s.rt.Inspect(context.Background(), container.OrbName(a.id))
	if err != nil {
		return f, err
	}
	f.ctr, f.m = string(cs), "none"
	if cs != container.StateMissing {
		f.m = mountKind(a.s.rt.spec(container.OrbName(a.id)).Mounts)
	}
	q, err := projectdef.Load(a.s.home, a.other)
	if err != nil {
		return f, fmt.Errorf("load %s: %w", a.other, err)
	}
	f.other = slices.Contains(q.Def.Identity, projectdef.IdentityGitHub)
	return f, nil
}

func (a *lendAdapter) GetState() (map[string]any, error) {
	f, err := a.observe()
	if err != nil {
		return nil, err
	}
	if a.action != "" {
		name := a.action
		if name != "Init" {
			name = "Lend#0." + name
		}
		q := map[string]any{}
		for k, v := range f.state() {
			q["Lend#0."+k] = v
		}
		a.trace = append(a.trace, tracecheck.Step{Action: name, State: q})
		a.action = ""
	}
	return f.state(), nil
}

// step opens an action: its require on the observed state, the gate, and
// the name the journal records.
func (a *lendAdapter) step(name string, require func(lendFields) bool) (lendFields, bool, error) {
	f, err := a.observe()
	if err != nil {
		return f, false, err
	}
	if !a.gate.pass(require(f)) {
		return f, false, nil
	}
	a.action = name
	a.did[name]++
	return f, true, nil
}

// changed is the spec's changed(old): drift once a container exists and
// what should be mounted moved.
func (a *lendAdapter) changed(old string) error {
	f, err := a.observe()
	if err != nil {
		return err
	}
	if f.ctr != "missing" && f.want() != old {
		a.drift = true
	}
	return nil
}

// define is one definition edit through serve's editor, and its drift.
func (a *lendAdapter) define(f lendFields, text string) error {
	if err := a.s.saveYAML(a.slug, text, http.StatusOK); err != nil {
		return err
	}
	return a.changed(f.want())
}

func (a *lendAdapter) AddDir() error {
	f, ok, err := a.step("AddDir", func(f lendFields) bool { return f.d == "none" && f.host })
	if !ok || err != nil {
		return err
	}
	return a.define(f, defYAML(".aws"))
}

func (a *lendAdapter) AddHazard() error {
	f, ok, err := a.step("AddHazard", func(f lendFields) bool { return f.d == "none" && f.host })
	if !ok || err != nil {
		return err
	}
	return a.define(f, defYAML(lendHazard))
}

// AddDenied lists .ssh itself: the editor must refuse it.
func (a *lendAdapter) AddDenied() error {
	_, ok, err := a.step("AddDenied", func(f lendFields) bool { return f.d == "none" })
	if !ok || err != nil {
		return err
	}
	return a.s.saveYAML(a.slug, defYAML(".ssh:rw"), http.StatusBadRequest)
}

func (a *lendAdapter) gateOff() bool { return a.gate.off }

// stutter is AddDenied wherever d is none: fizz has no link for it, as
// it changes nothing, so the graph's walks never take it.
func (a *lendAdapter) stutter() error {
	before, err := a.observe()
	if err != nil || before.d != "none" {
		return err
	}
	if err := a.s.saveYAML(a.slug, defYAML(".ssh:rw"), http.StatusBadRequest); err != nil {
		return fmt.Errorf("AddDenied: %w", err)
	}
	a.did["AddDenied"]++
	after, err := a.observe()
	if err != nil {
		return err
	}
	if after != before {
		return fmt.Errorf("AddDenied changed the state: %v, was %v", after.state(), before.state())
	}
	return nil
}

// GuestWritesYaml is the guest editing project.yml through the project
// dir mounted into its container: a plain write, no WriteFile.
func (a *lendAdapter) GuestWritesYaml() error {
	f, ok, err := a.step("GuestWritesYaml", func(f lendFields) bool { return f.live && f.d == "none" })
	if !ok || err != nil {
		return err
	}
	p, err := projectdef.Load(a.s.home, a.slug)
	if err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(p.Dir, projectdef.FileYAML), []byte(defYAML(".aws:rw")), 0o644); err != nil {
		return err
	}
	return a.changed(f.want())
}

func (a *lendAdapter) RemoveDir() error {
	f, ok, err := a.step("RemoveDir", func(f lendFields) bool { return f.d != "none" })
	if !ok || err != nil {
		return err
	}
	return a.define(f, defYAML())
}

func (a *lendAdapter) HostDeletesDir() error {
	f, ok, err := a.step("HostDeletesDir", func(f lendFields) bool { return f.host })
	if !ok || err != nil {
		return err
	}
	if err := a.hostDirs(false); err != nil {
		return err
	}
	return a.changed(f.want())
}

func (a *lendAdapter) HostCreatesDir() error {
	f, ok, err := a.step("HostCreatesDir", func(f lendFields) bool { return !f.host })
	if !ok || err != nil {
		return err
	}
	if err := a.hostDirs(true); err != nil {
		return err
	}
	return a.changed(f.want())
}

func (a *lendAdapter) StartOrb() error {
	f, ok, err := a.step("StartOrb", func(f lendFields) bool { return !f.live })
	if !ok || err != nil {
		return err
	}
	if a.startRecreates && f.ctr != "missing" {
		ctx, cancel := actionCtx()
		err := a.s.rt.Remove(ctx, container.OrbName(a.id))
		cancel()
		if err != nil {
			return err
		}
	}
	o, _, err := a.s.startSession(a.slug, a.id)
	if err != nil {
		return err
	}
	a.o = o
	g, err := a.observe()
	if err != nil {
		return err
	}
	if f.ctr == "missing" {
		a.drift, a.stale = false, false
	} else {
		a.stale = g.m != g.want()
	}
	return nil
}

// StopOrb is serve's Stop orb while the session lives.
func (a *lendAdapter) StopOrb() error {
	_, ok, err := a.step("StopOrb", func(f lendFields) bool { return f.live && f.ctr == "running" })
	if !ok || err != nil {
		return err
	}
	return a.s.call(http.MethodPost, "/api/sessions/"+a.id+"/orb/stop", nil, http.StatusOK)
}

// ExecRestarts is the session's next command after a stop.
func (a *lendAdapter) ExecRestarts() error {
	_, ok, err := a.step("ExecRestarts", func(f lendFields) bool { return f.live && f.ctr == "stopped" })
	if !ok || err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if out, err := a.o.Command(ctx, "true").CombinedOutput(); err != nil {
		return fmt.Errorf("exec: %w: %s", err, out)
	}
	return nil
}

func (a *lendAdapter) Exit() error {
	_, ok, err := a.step("Exit", func(f lendFields) bool { return f.live })
	if !ok || err != nil {
		return err
	}
	if err := a.s.exitSession(a.o, a.id); err != nil {
		return err
	}
	a.o, a.stale = nil, false
	return nil
}

// RemoveOrb is serve's Remove orb (DELETE) with the session down.
func (a *lendAdapter) RemoveOrb() error {
	_, ok, err := a.step("RemoveOrb", func(f lendFields) bool { return !f.live && f.ctr == "stopped" })
	if !ok || err != nil {
		return err
	}
	if err := a.s.call(http.MethodDelete, "/api/sessions/"+url.PathEscape(a.id)+"/orb", nil, http.StatusOK); err != nil {
		return err
	}
	a.drift = false
	return nil
}

// RelayOtherSlug runs what the relay forwards for a guest: `bough
// project add-identity <other> gh`, a WriteFile of the other project.
func (a *lendAdapter) RelayOtherSlug() error {
	_, ok, err := a.step("RelayOtherSlug", func(f lendFields) bool { return f.live && f.ctr == "running" && !f.other })
	if !ok || err != nil {
		return err
	}
	return a.s.saveYAML(a.other, defYAML(projectdef.IdentityGitHub), http.StatusOK)
}

var lendActions = map[string]map[string]fmbt.ActionFunc{"Lend": {
	"AddDir":          action((*lendAdapter).AddDir),
	"AddHazard":       action((*lendAdapter).AddHazard),
	"AddDenied":       action((*lendAdapter).AddDenied),
	"GuestWritesYaml": action((*lendAdapter).GuestWritesYaml),
	"RemoveDir":       action((*lendAdapter).RemoveDir),
	"HostDeletesDir":  action((*lendAdapter).HostDeletesDir),
	"HostCreatesDir":  action((*lendAdapter).HostCreatesDir),
	"StartOrb":        action((*lendAdapter).StartOrb),
	"StopOrb":         action((*lendAdapter).StopOrb),
	"ExecRestarts":    action((*lendAdapter).ExecRestarts),
	"Exit":            action((*lendAdapter).Exit),
	"RemoveOrb":       action((*lendAdapter).RemoveOrb),
	"RelayOtherSlug":  action((*lendAdapter).RelayOtherSlug),
}}

// identityLendingHistory: a session's transcript says nothing of its
// orb's mounts (they are the runtime's and project.yml's), only that the
// walk starts with the session down. The journal of each walk, replayed
// on the graph, is this flow's trace.
func identityLendingHistory([]history.Entry) []tracecheck.Step {
	return []tracecheck.Step{{Action: "Init", State: map[string]any{"Lend#0.live": false}}}
}

func init() { historyProjections["identity_lending"] = identityLendingHistory }

// No step is a model turn: walks are cheap.
func identityLendingOptions() map[string]any {
	return map[string]any{"max-seq-runs": 300, "max-actions": 10, "max-parallel-runs": 0}
}

func TestIdentityLending(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newLendAdapter(t)
	if err := runMBT(t, "identity_lending", a, lendActions, identityLendingOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkJournal(t, loadGraph(t, "identity_lending"), a.journal)
	t.Logf("steps taken: %v", a.did)
}

// lendPathModel is an adapter walkModelPaths drives.
type lendPathModel interface {
	fmbt.Model
	gateOff() bool
	// stutter takes, wherever they are enabled, the actions that change
	// nothing (a refused edit) and so have no link in the graph, and
	// fails unless the state stayed as it was.
	stutter() error
}

// walkModelPaths drives init and actions down every walk of the spec's
// graph, comparing the role's whole state after every step.
func walkModelPaths(t *testing.T, spec, role string, cover tracecheck.Cover, m lendPathModel, actions map[string]fmbt.ActionFunc) error {
	t.Helper()
	b, err := pathsJSONCover(spec, cover)
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
		t.Fatalf("no walks over testdata/%s", spec)
	}
	var errs []error
	for i, p := range doc.Paths {
		if err := walkModelPath(m, role, actions, p.Trace); err != nil {
			var acts []string
			for _, s := range p.Trace[1:] {
				acts = append(acts, strings.TrimPrefix(s.Action, role+"#0."))
			}
			errs = append(errs, fmt.Errorf("walk %d: %w\n  its actions: %v", i, err, acts))
		}
	}
	t.Logf("%d walks over testdata/%s (%s)", len(doc.Paths), spec, cover)
	return errors.Join(errs...)
}

func walkModelPath(m lendPathModel, role string, actions map[string]fmbt.ActionFunc, path []tracecheck.Step) error {
	defer m.Cleanup()
	check := func(i int) error {
		got, err := m.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, path[i].Action, err)
		}
		var diff []string
		for k, w := range path[i].State {
			f, ok := strings.CutPrefix(k, role+"#0.")
			if !ok {
				continue
			}
			if fmt.Sprint(got[f]) != fmt.Sprint(w) {
				diff = append(diff, fmt.Sprintf("%s = %v, spec %v", f, got[f], w))
			}
		}
		if len(diff) > 0 {
			slices.Sort(diff)
			return fmt.Errorf("step %d (%s): %s", i, path[i].Action, strings.Join(diff, "; "))
		}
		return nil
	}
	if err := m.Init(); err != nil {
		return fmt.Errorf("Init: %w", err)
	}
	if err := check(0); err != nil {
		return err
	}
	if err := m.stutter(); err != nil {
		return fmt.Errorf("after Init: %w", err)
	}
	for i := 1; i < len(path); i++ {
		name := strings.TrimPrefix(path[i].Action, role+"#0.")
		fn, ok := actions[name]
		if !ok {
			return fmt.Errorf("step %d: no adapter action %q", i, name)
		}
		if _, err := fn(m, nil); err != nil {
			return fmt.Errorf("step %d (%s): %w", i, name, err)
		}
		if m.gateOff() {
			return fmt.Errorf("step %d (%s): the adapter found it disabled", i, name)
		}
		if err := check(i); err != nil {
			return err
		}
		if err := m.stutter(); err != nil {
			return fmt.Errorf("after step %d (%s): %w", i, name, err)
		}
	}
	return nil
}

func loadGraph(t *testing.T, spec string) *tracecheck.Graph {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath(spec)), "..", "testdata", spec))
	if err != nil {
		t.Fatal(err)
	}
	return g
}

// checkJournal replays every walk's own record (each action taken and
// the state observed after it) on the graph.
func checkJournal(t *testing.T, g *tracecheck.Graph, journal [][]tracecheck.Step) {
	t.Helper()
	for i, walk := range journal {
		if v := g.Check(walk); v != nil {
			b, _ := json.Marshal(walk)
			t.Errorf("walk %d is not a path in the model: %v\ntrace: %s", i, v, b)
		}
	}
}

// TestIdentityLendingPaths walks the spec's graph (every settled state,
// or every link under MODEL_COVER=transitions) against the definition
// editor, the orb and serve, then replays each walk's journal on the
// graph and a session transcript through the history projection.
func TestIdentityLendingPaths(t *testing.T) {
	t.Parallel()
	a := newLendAdapter(t)
	if err := walkModelPaths(t, "identity_lending", "Lend", envCover(), a, lendActions["Lend"]); err != nil {
		t.Fatal(err)
	}
	g := loadGraph(t, "identity_lending")
	checkJournal(t, g, a.journal)
	checkHistory(t, g, sessionHistory(t, a.s.home, a.id), identityLendingHistory)
	t.Logf("steps taken: %v", a.did)
	for name := range lendActions["Lend"] {
		if a.did[name] == 0 {
			t.Errorf("no walk took %s", name)
		}
	}
}

// A start that removes the stopped container first (the restart applying
// the definition, which G2 says it does not) must fail the walk: it shows
// on the StartOrb links that reuse a container after a change, so every
// link is walked.
func TestIdentityLendingPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newLendAdapter(t)
	a.startRecreates = true
	err := walkModelPaths(t, "identity_lending", "Lend", tracecheck.CoverTransitions, a, lendActions["Lend"])
	if err == nil {
		t.Fatal("every walk passed with a start that recreates the container; the walk is not checking state")
	}
	t.Logf("caught, as it must be: %.400s", err)
}
