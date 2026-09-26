//go:build !windows

package mbt

import (
	"encoding/json"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/orb_delete_project_vs_active_portal.fizz against internal/orb's
// real portal split (OpenPortalListen/RecordOpenedPortal, added for this
// flow so the spec's "opening" window — a listener already live and
// relaying but not yet in state.json — is the real gap between those two
// calls, not a fiction) and the real sweep the spec's TeardownSweep
// names: orb.CloseIfProjectGone, run every tick by
// internal/serve/reaper.go's reapVanishedProjectPortals. "project" is a
// real projectdef directory this walk creates and removes; "listener" is
// a real dial through the portal's own host-side listener, which accepts
// whether or not anything answers behind it (portal.serve only forwards
// once a client connects); "recorded" is state.json's own Portals list.
// "opening" and "serving" are adapter-tracked client state, like the
// example's `viewing`: real in this walk (opening is the true window
// above) but nothing external reads them back.
type odpAdapter struct {
	t    *testing.T
	sup  *serve.Supervisor
	api  *serve.API
	home string

	tag  string // unique per adapter (per test), see odpTag
	n    int
	id   string // this walk's orb session id
	slug string // this walk's project slug

	ps      iorb.PortalState // set once OpenListen creates the listener
	opening bool
	serving bool

	gate gate
}

const odpGuest = 8080

func newOdpAdapter(t *testing.T) *odpAdapter {
	home := t.TempDir()
	hist := filepath.Join(home, ".bough", "history")
	if err := os.MkdirAll(hist, 0o755); err != nil {
		t.Fatal(err)
	}
	sup, err := serve.NewSupervisor(serve.Options{
		HistDir:  hist,
		MetaPath: filepath.Join(home, ".bough", "serve", "meta.json"),
		Env:      []string{"HOME=" + home, "PATH=" + os.Getenv("PATH")},
	})
	if err != nil {
		t.Fatal(err)
	}
	a := &odpAdapter{t: t, home: home, sup: sup, api: serve.NewAPI(sup), tag: odpTag(t)}
	t.Cleanup(func() { sup.Close() })
	return a
}

// odpTag makes each adapter's session and project ids unique across the
// package: the portal listener the split calls make live is process-wide
// (internal/orb's own `portals` map, keyed by session id), so two
// adapters in two parallel tests both starting their counters at 1 would
// otherwise collide on the same session id and "open" each other's
// listener.
func odpTag(t *testing.T) string {
	var b strings.Builder
	for _, r := range strings.ToLower(t.Name()) {
		switch {
		case r >= 'a' && r <= 'z', r >= '0' && r <= '9':
			b.WriteRune(r)
		default:
			b.WriteByte('-')
		}
	}
	tag := "t-" + b.String() // "t-" keeps it starting with a letter even if Name() didn't
	if len(tag) > 40 {
		tag = tag[:40]
	}
	return tag
}

func (a *odpAdapter) projectDir() string { return filepath.Join(projectdef.Root(a.home), a.slug) }

// Init starts each walk with a fresh project directory and a running orb
// recorded against it, no portal open yet.
func (a *odpAdapter) Init() error {
	a.n++
	a.id = fmt.Sprintf("%s-%04d", a.tag, a.n)
	a.slug = fmt.Sprintf("%s-project-%04d", a.tag, a.n)
	a.ps = iorb.PortalState{}
	a.opening, a.serving = false, false
	a.gate.reset()

	if _, err := projectdef.CreateEmpty(a.home, a.slug, ""); err != nil {
		return err
	}
	if err := os.MkdirAll(iorb.Dir(a.home, a.id), 0o755); err != nil {
		return err
	}
	return iorb.Restore(a.home, iorb.State{
		Session: a.id, Project: a.slug, Status: iorb.StatusRunning, IP: "127.0.0.1",
	})
}

// Cleanup drops this walk's orb dir and project directory, so a walk
// left behind is not still found by the next one's reap sweep.
func (a *odpAdapter) Cleanup() error {
	iorb.CloseSessionPortals(a.home, a.id)
	os.RemoveAll(iorb.Dir(a.home, a.id))
	return os.RemoveAll(a.projectDir())
}

func (a *odpAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Portal", Index: 0}: a}, nil
}

func (a *odpAdapter) projectOK() bool {
	_, err := os.Stat(a.projectDir())
	return err == nil
}

// dialListener says whether the portal's host-side listener still
// accepts a connection: the real check, not a value the adapter
// remembers setting. A portal with no host port yet (never opened) is
// not listening.
func (a *odpAdapter) dialListener() bool {
	if a.ps.Host == 0 {
		return false
	}
	c, err := net.DialTimeout("tcp", fmt.Sprintf("127.0.0.1:%d", a.ps.Host), 500*time.Millisecond)
	if err != nil {
		return false
	}
	c.Close()
	return true
}

func (a *odpAdapter) recorded() bool {
	st, err := iorb.ReadState(a.home, a.id)
	if err != nil {
		return false
	}
	for _, p := range st.Portals {
		if p.Guest == odpGuest {
			return true
		}
	}
	return false
}

func (a *odpAdapter) GetState() (map[string]any, error) {
	project := "gone"
	if a.projectOK() {
		project = "ok"
	}
	return map[string]any{
		"project":  project,
		"listener": a.dialListener(),
		"recorded": a.recorded(),
		"opening":  a.opening,
		"serving":  a.serving,
	}, nil
}

func (a *odpAdapter) OpenListen() error {
	if !a.gate.pass(a.projectOK() && !a.dialListener()) {
		return nil
	}
	ps, opened, err := iorb.OpenPortalListen(a.home, a.id, odpGuest, "test")
	if err != nil {
		return err
	}
	if !opened {
		return fmt.Errorf("model: portal for guest %d was already open", odpGuest)
	}
	a.ps = ps
	a.opening = true
	return nil
}

func (a *odpAdapter) OpenRecord() error {
	if !a.gate.pass(a.opening && a.dialListener()) {
		return nil
	}
	if err := iorb.RecordOpenedPortal(a.home, a.id, a.ps); err != nil {
		return err
	}
	a.opening = false
	return nil
}

func (a *odpAdapter) Close() error {
	if !a.gate.pass(!a.opening && a.dialListener()) {
		return nil
	}
	if err := iorb.ClosePortal(a.home, a.id, odpGuest); err != nil {
		return err
	}
	a.ps = iorb.PortalState{}
	return nil
}

func (a *odpAdapter) ServeRequest() error {
	if !a.gate.pass(a.dialListener() && a.projectOK()) {
		return nil
	}
	a.serving = true
	return nil
}

func (a *odpAdapter) ServeDone() error {
	if !a.gate.pass(a.serving) {
		return nil
	}
	a.serving = false
	return nil
}

// ProjectVanish removes the project directory itself: a delete or rename
// out from under the orb that does not go through
// Supervisor.DeleteProject (which tears its sessions down first), the
// case this flow exists to check.
func (a *odpAdapter) ProjectVanish() error {
	if !a.gate.pass(a.projectOK() && !a.opening && !a.serving) {
		return nil
	}
	return os.RemoveAll(a.projectDir())
}

// TeardownSweep is the product's reactive half: one tick of the same
// sweep StartReaper runs, exercised through the API's own test seam so
// it is the real reapVanishedProjectPortals, not a copy of it.
func (a *odpAdapter) TeardownSweep() error {
	if !a.gate.pass(!a.projectOK() && !a.opening && a.dialListener()) {
		return nil
	}
	a.api.ReapVanishedProjectPortals()
	return nil
}

// Settled is a no-op: the spec keeps it enabled so a gone project with
// its portal already closed is a steady state, not a deadlock.
func (a *odpAdapter) Settled() error {
	if !a.gate.pass(!a.projectOK() && !a.dialListener()) {
		return nil
	}
	return nil
}

var odpActions = map[string]map[string]fmbt.ActionFunc{"Portal": {
	"OpenListen":    action((*odpAdapter).OpenListen),
	"OpenRecord":    action((*odpAdapter).OpenRecord),
	"Close":         action((*odpAdapter).Close),
	"ServeRequest":  action((*odpAdapter).ServeRequest),
	"ServeDone":     action((*odpAdapter).ServeDone),
	"ProjectVanish": action((*odpAdapter).ProjectVanish),
	"TeardownSweep": action((*odpAdapter).TeardownSweep),
	"Settled":       action((*odpAdapter).Settled),
}}

func odpOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 10, "max-parallel-runs": 0}
}

// odpHistory: nothing in this flow goes through a session's transcript
// (no model turn is ever run), so there is nothing to project off
// history.Read; registered only so TestHistoryTraces recognizes the
// spec's name if ever pointed at it.
func odpHistory([]history.Entry) []tracecheck.Step {
	return []tracecheck.Step{{Action: "Init", State: map[string]any{
		"Portal#0.project": "ok", "Portal#0.listener": false, "Portal#0.recorded": false,
		"Portal#0.opening": false, "Portal#0.serving": false,
	}}}
}

func init() {
	historyProjections["orb_delete_project_vs_active_portal"] = odpHistory
}

func (a *odpAdapter) compare(want map[string]any) error {
	got, err := a.GetState()
	if err != nil {
		return err
	}
	var diff []string
	for k, v := range want {
		f, ok := strings.CutPrefix(k, "Portal#0.")
		if !ok {
			continue
		}
		if fmt.Sprint(got[f]) != fmt.Sprint(v) {
			diff = append(diff, fmt.Sprintf("%s: spec %v, got %v", f, v, got[f]))
		}
	}
	if len(diff) > 0 {
		return fmt.Errorf("state: %s", strings.Join(diff, "; "))
	}
	return nil
}

func walkOdpPaths(t *testing.T, a *odpAdapter, paths []genPath, firstOnly bool) (mismatches []string) {
	t.Helper()
	for wi, w := range paths {
		var names []string
		for si, step := range w.Trace {
			name := strings.TrimPrefix(step.Action, "Portal#0.")
			names = append(names, name)
			var err error
			if si == 0 {
				err = a.Init()
			} else {
				f, ok := odpActions["Portal"][name]
				if !ok {
					t.Fatalf("walk %d: no adapter action %q", wi, name)
				}
				_, err = f(a, nil)
				if err == nil && a.gate.off {
					err = fmt.Errorf("the adapter refused a step the spec enables (its require disagrees)")
				}
			}
			if err == nil {
				err = a.compare(step.State)
			}
			if err != nil {
				mismatches = append(mismatches, fmt.Sprintf("walk %d step %d (%s): %v", wi, si, strings.Join(names, " "), err))
				break
			}
		}
		if err := a.Cleanup(); err != nil {
			mismatches = append(mismatches, fmt.Sprintf("walk %d: cleanup: %v", wi, err))
		}
		if firstOnly && len(mismatches) > 0 {
			return mismatches
		}
	}
	return mismatches
}

func TestOrbDeleteProjectVsActivePortalPaths(t *testing.T) {
	t.Parallel()
	a := newOdpAdapter(t)
	for _, m := range walkOdpPaths(t, a, loadPaths(t, "orb_delete_project_vs_active_portal"), false) {
		t.Error(m)
	}
}

// The random runs, in the exhaustive job only (runMBT skips otherwise).
func TestOrbDeleteProjectVsActivePortal(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newOdpAdapter(t)
	if err := runMBT(t, "orb_delete_project_vs_active_portal", a, odpActions, odpOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// The walks above prove nothing unless a wrong adapter fails them: this
// one never sweeps, so a vanished project's portal is never closed. It
// needs every transition covered, not just every state: a walk that
// closes the portal with Close before ProjectVanish reaches the same
// (project=gone, listener=false) state without ever calling
// TeardownSweep, so on the default state cover this bug can hide behind
// a path that never exercises it.
func TestOrbDeleteProjectVsActivePortalCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newOdpAdapter(t)
	orig := odpActions["Portal"]["TeardownSweep"]
	odpActions["Portal"]["TeardownSweep"] = action((*odpAdapter).noopTeardown)
	t.Cleanup(func() { odpActions["Portal"]["TeardownSweep"] = orig })
	b, err := pathsJSONCover("orb_delete_project_vs_active_portal", tracecheck.CoverTransitions)
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Paths []genPath `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		t.Fatal(err)
	}
	if len(walkOdpPaths(t, a, f.Paths, true)) == 0 {
		t.Fatal("walks whose adapter never sweeps a vanished project's portal passed; the runner is not checking state")
	}
}

// noopTeardown is TestOrbDeleteProjectVsActivePortalCatchesWrongAdapter's
// deliberate wiring bug: it passes the gate (so the walk does not stop
// early) but never actually closes anything, which HandleReleasedAfterVanish
// requires eventually happens.
func (a *odpAdapter) noopTeardown() error {
	if !a.gate.pass(!a.projectOK() && !a.opening && a.dialListener()) {
		return nil
	}
	return nil
}
