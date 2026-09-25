//go:build !windows

package mbt

import (
	"fmt"
	"net/http"
	"slices"
	"strings"
	"sync"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/identity_lending_gh.fizz against serve's project editor and the
// orb's per-exec env, over the same in-process serve and recording
// runtime as identity_lending_test.go. The host's gh login is the
// stubbed `gh auth token` (lendHost): a new token per login, "" logged
// out.
//
// gh is read off project.yml, host off the stub, env off the GH_TOKEN
// the runtime was handed on the session's latest exec. live, s_gh and
// tok are the session process's own: the adapter is that process, so it
// keeps which definition it started with, and the token it holds is the
// one its execs got (githubToken caches exactly what it hands out; an
// empty answer is not cached). tok is live while that token is still the
// host's, stale once the host logged out or in again.
//
// githubToken's cache is the process's, and every orb here runs in this
// one: the gh walks hold ghWalkMu, and a session start or exit empties
// the cache as a new process has it empty (orb.ForgetGitHubToken).

var ghWalkMu sync.Mutex

type ghFields struct {
	gh, host, live, sgh bool
	tok, env            string
}

func (f ghFields) state() map[string]any {
	return map[string]any{"gh": f.gh, "host": f.host, "live": f.live, "s_gh": f.sgh, "tok": f.tok, "env": f.env}
}

type ghAdapter struct {
	t *testing.T
	s *lendServe

	n        int
	id, slug string
	o        *orb.Orb
	sgh      bool
	tokVal   string // the token this session's process holds
	gate     gate
	action   string
	trace    []tracecheck.Step
	journal  [][]tracecheck.Step
	did      map[string]int

	// keepCache is the deliberate bug the wrong-adapter test injects: a
	// session that starts in the process the last one ran in, its token
	// cache still full.
	keepCache bool
}

func newGhAdapter(t *testing.T) *ghAdapter {
	ghWalkMu.Lock()
	t.Cleanup(ghWalkMu.Unlock)
	return &ghAdapter{t: t, s: newLendServe(t), did: map[string]int{}}
}

func hostToken() string {
	lendHost.Lock()
	defer lendHost.Unlock()
	return lendHost.token
}

// hostLogin is `gh auth login`: a new token, never an earlier one.
func hostLogin() {
	lendHost.Lock()
	defer lendHost.Unlock()
	lendHost.n++
	lendHost.token = fmt.Sprintf("gho_model%04d", lendHost.n)
}

func hostLogout() {
	lendHost.Lock()
	defer lendHost.Unlock()
	lendHost.token = ""
}

// tokenKind is the spec's none | live | stale for a token value.
func tokenKind(v string) string {
	switch {
	case v == "":
		return "none"
	case v == hostToken():
		return "live"
	}
	return "stale"
}

func (a *ghAdapter) Init() error {
	a.n++
	a.id, a.slug = fmt.Sprintf("gh%03d", a.n), fmt.Sprintf("g%03d", a.n)
	a.o, a.sgh, a.tokVal = nil, false, ""
	a.gate.reset()
	a.trace = nil
	hostLogin()
	orb.ForgetGitHubToken()
	if err := a.s.newProjectSession(a.slug, a.id); err != nil {
		return err
	}
	a.action = "Init"
	return nil
}

func (a *ghAdapter) Cleanup() error {
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

func (a *ghAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "GhLend", Index: 0}: a}, nil
}

// execToken is GH_TOKEN of the session's latest exec, "" when that exec
// had none or there was none since the session started.
func (a *ghAdapter) execToken() string {
	env, _ := a.s.rt.lastEnv(container.OrbName(a.id))
	for _, kv := range env {
		if v, ok := strings.CutPrefix(kv, "GH_TOKEN="); ok {
			return v
		}
	}
	return ""
}

func (a *ghAdapter) observe() (ghFields, error) {
	f := ghFields{live: a.o != nil, sgh: a.sgh, host: hostToken() != ""}
	p, err := projectdef.Load(a.s.home, a.slug)
	if err != nil {
		return f, fmt.Errorf("load %s: %w", a.slug, err)
	}
	f.gh = slices.Contains(p.Def.Identity, projectdef.IdentityGitHub)
	f.tok, f.env = tokenKind(a.tokVal), tokenKind(a.execToken())
	return f, nil
}

func (a *ghAdapter) GetState() (map[string]any, error) {
	f, err := a.observe()
	if err != nil {
		return nil, err
	}
	if a.action != "" {
		name := a.action
		if name != "Init" {
			name = "GhLend#0." + name
		}
		q := map[string]any{}
		for k, v := range f.state() {
			q["GhLend#0."+k] = v
		}
		a.trace = append(a.trace, tracecheck.Step{Action: name, State: q})
		a.action = ""
	}
	return f.state(), nil
}

func (a *ghAdapter) step(name string, require func(ghFields) bool) (ghFields, bool, error) {
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

func (a *ghAdapter) gateOff() bool { return a.gate.off }

// stutter is Exec wherever the spec's Exec changes nothing, which fizz
// writes no link for: a session without gh execs again with GH_TOKEN
// still unset, or one with gh gets the token it already had.
func (a *ghAdapter) stutter() error {
	before, err := a.observe()
	if err != nil || !before.live {
		return err
	}
	next := before
	if before.sgh {
		if next.tok == "none" && before.host {
			next.tok = "live"
		}
		next.env = next.tok
	} else {
		next.env = "none"
	}
	if next != before {
		return nil
	}
	if err := a.exec(); err != nil {
		return fmt.Errorf("Exec: %w", err)
	}
	a.did["Exec"]++
	after, err := a.observe()
	if err != nil {
		return err
	}
	if after != before {
		return fmt.Errorf("Exec changed the state: %v, was %v", after.state(), before.state())
	}
	return nil
}

func (a *ghAdapter) AddGh() error {
	_, ok, err := a.step("AddGh", func(f ghFields) bool { return !f.gh })
	if !ok || err != nil {
		return err
	}
	return a.s.saveYAML(a.slug, defYAML(projectdef.IdentityGitHub), http.StatusOK)
}

func (a *ghAdapter) RemoveGh() error {
	_, ok, err := a.step("RemoveGh", func(f ghFields) bool { return f.gh })
	if !ok || err != nil {
		return err
	}
	return a.s.saveYAML(a.slug, defYAML(), http.StatusOK)
}

func (a *ghAdapter) HostLogout() error {
	_, ok, err := a.step("HostLogout", func(f ghFields) bool { return f.host })
	if !ok || err != nil {
		return err
	}
	hostLogout()
	return nil
}

func (a *ghAdapter) HostLogin() error {
	_, ok, err := a.step("HostLogin", func(f ghFields) bool { return !f.host })
	if !ok || err != nil {
		return err
	}
	hostLogin()
	return nil
}

// newProcess is what a session process starts or ends with: no token.
func (a *ghAdapter) newProcess() {
	if !a.keepCache {
		orb.ForgetGitHubToken()
	}
	a.tokVal = ""
	a.s.rt.forgetEnv(container.OrbName(a.id))
}

func (a *ghAdapter) StartSession() error {
	_, ok, err := a.step("StartSession", func(f ghFields) bool { return !f.live })
	if !ok || err != nil {
		return err
	}
	a.newProcess()
	o, p, err := a.s.startSession(a.slug, a.id)
	if err != nil {
		return err
	}
	a.o, a.sgh = o, slices.Contains(p.Def.Identity, projectdef.IdentityGitHub)
	return nil
}

func (a *ghAdapter) Exit() error {
	_, ok, err := a.step("Exit", func(f ghFields) bool { return f.live })
	if !ok || err != nil {
		return err
	}
	if err := a.s.exitSession(a.o, a.id); err != nil {
		return err
	}
	a.o, a.sgh = nil, false
	a.newProcess()
	return nil
}

func (a *ghAdapter) exec() error {
	ctx, cancel := actionCtx()
	defer cancel()
	if out, err := a.o.Command(ctx, "true").CombinedOutput(); err != nil {
		return fmt.Errorf("exec: %w: %s", err, out)
	}
	// What a gh session's exec got is what its cache holds now.
	if a.sgh {
		a.tokVal = a.execToken()
	}
	return nil
}

func (a *ghAdapter) Exec() error {
	_, ok, err := a.step("Exec", func(f ghFields) bool { return f.live })
	if !ok || err != nil {
		return err
	}
	return a.exec()
}

// CacheExpires is five minutes passing.
func (a *ghAdapter) CacheExpires() error {
	_, ok, err := a.step("CacheExpires", func(f ghFields) bool { return f.tok != "none" })
	if !ok || err != nil {
		return err
	}
	orb.ForgetGitHubToken()
	a.tokVal = ""
	return nil
}

var ghLendActions = map[string]map[string]fmbt.ActionFunc{"GhLend": {
	"AddGh":        action((*ghAdapter).AddGh),
	"RemoveGh":     action((*ghAdapter).RemoveGh),
	"HostLogout":   action((*ghAdapter).HostLogout),
	"HostLogin":    action((*ghAdapter).HostLogin),
	"StartSession": action((*ghAdapter).StartSession),
	"Exit":         action((*ghAdapter).Exit),
	"Exec":         action((*ghAdapter).Exec),
	"CacheExpires": action((*ghAdapter).CacheExpires),
}}

// identityLendingGhHistory: a transcript does not carry GH_TOKEN (it is
// per exec env, never output), only that the walk starts with no
// session. The walks' journals are this flow's trace.
func identityLendingGhHistory([]history.Entry) []tracecheck.Step {
	return []tracecheck.Step{{Action: "Init", State: map[string]any{"GhLend#0.live": false}}}
}

func init() { historyProjections["identity_lending_gh"] = identityLendingGhHistory }

func TestIdentityLendingGh(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newGhAdapter(t)
	opts := map[string]any{"max-seq-runs": 300, "max-actions": 10, "max-parallel-runs": 0}
	if err := runMBT(t, "identity_lending_gh", a, ghLendActions, opts); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkJournal(t, loadGraph(t, "identity_lending_gh"), a.journal)
	t.Logf("steps taken: %v", a.did)
}

// TestIdentityLendingGhPaths walks the spec's graph against the editor
// and the orb's exec env, then replays each walk's journal on the graph
// and a session transcript through the history projection.
func TestIdentityLendingGhPaths(t *testing.T) {
	t.Parallel()
	a := newGhAdapter(t)
	if err := walkModelPaths(t, "identity_lending_gh", "GhLend", envCover(), a, ghLendActions["GhLend"]); err != nil {
		t.Fatal(err)
	}
	g := loadGraph(t, "identity_lending_gh")
	checkJournal(t, g, a.journal)
	checkHistory(t, g, sessionHistory(t, a.s.home, a.id), identityLendingGhHistory)
	t.Logf("steps taken: %v", a.did)
	for name := range ghLendActions["GhLend"] {
		if a.did[name] == 0 {
			t.Errorf("no walk took %s", name)
		}
	}
}

// A session that starts with the last one's token still cached must fail
// the walk: it shows only on an Exec after a restart with a stale token
// left behind, so every link is walked.
func TestIdentityLendingGhPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newGhAdapter(t)
	a.keepCache = true
	err := walkModelPaths(t, "identity_lending_gh", "GhLend", tracecheck.CoverTransitions, a, ghLendActions["GhLend"])
	if err == nil {
		t.Fatal("every walk passed with a session keeping the last one's token cache; the walk is not checking state")
	}
	t.Logf("caught, as it must be: %.400s", err)
}
