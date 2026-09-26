//go:build !windows

package mbt

import (
	"fmt"
	"io"
	"net"
	"net/http"
	"strconv"
	"strings"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/orb_portal_open_close_race.fizz against internal/orb's real
// portal and state code, on a real serve's home directory
// (servetest.Server.Home) so state.json lives exactly where
// go/plugins/orb/portal.go and internal/orb read and write it.
//
// The orb itself is not a real container: StopOrb is orb.MarkStopped
// (the exact function serve's own "Stop orb" and the reaper call for a
// stop made outside the owning child), and RestartOrb writes the new
// state and calls orb.RetargetPortals(session, ip) itself, exactly as
// orb.Orb.Replace does after a real restart's new container gets its
// IP. Both are the real product functions the spec's header comment
// names; only the container underneath them is skipped.
//
// "orbIP" and "dial" are two real, differently-addressed loopback HTTP
// servers ("ip1" at 127.0.0.1:<port>, "ip2" at [::1]:<port>, same
// port): OpenPortal dials <orbIP>:<port>, so which one answers a
// request through the portal is the real dial, read back by an actual
// HTTP round trip through the portal's URL — not a value the adapter
// merely remembers it set.

type portalAdapter struct {
	t    *testing.T
	s    *servetest.Server
	gate gate

	guest      int    // the fixed "guest" port both fake backends share
	ip1, ip2   string // 127.0.0.2, 127.0.0.3: the two orb addresses
	closeFakes func()

	id string // this walk's session id (the state.json key)

	// wrongOrbIP is TestOrbPortalOpenCloseRaceCatchesWrongAdapter's bug:
	// GetState always reports "ip1", so a restart to ip2 is invisible.
	wrongOrbIP bool
}

func newPortalAdapter(t *testing.T) *portalAdapter {
	s := servetest.Start(t, servetest.Options{})
	guest, ip1, ip2, closeFakes := portalFakeBackends(t)
	return &portalAdapter{t: t, s: s, guest: guest, ip1: ip1, ip2: ip2, closeFakes: closeFakes}
}

// portalFakeBackends starts two HTTP servers on the same port on two
// different loopback addresses, each answering with its own label, so a
// connection through the portal says which orb address it actually
// reached. Binding the same port to both addresses can lose a race to
// another process; it retries a few times before giving up.
func portalFakeBackends(t *testing.T) (guest int, ip1, ip2 string, closeFn func()) {
	t.Helper()
	// 127.0.0.1 and ::1 are two genuinely different, always-bindable
	// loopback addresses (a sandbox may have no 127.0.0.2/.3 alias),
	// which is exactly what "two different orb IPs, same guest port"
	// needs.
	const host1, host2 = "127.0.0.1", "::1"
	for attempt := 0; attempt < 20; attempt++ {
		probe, err := net.Listen("tcp", host1+":0")
		if err != nil {
			t.Fatalf("portal fakes: %v", err)
		}
		port := probe.Addr().(*net.TCPAddr).Port
		probe.Close()

		ln1, err1 := net.Listen("tcp", net.JoinHostPort(host1, strconv.Itoa(port)))
		if err1 != nil {
			continue
		}
		ln2, err2 := net.Listen("tcp", net.JoinHostPort(host2, strconv.Itoa(port)))
		if err2 != nil {
			ln1.Close()
			continue
		}
		srv1 := &http.Server{Handler: http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.Write([]byte("ip1")) })}
		srv2 := &http.Server{Handler: http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.Write([]byte("ip2")) })}
		go srv1.Serve(ln1)
		go srv2.Serve(ln2)
		return port, host1, host2, func() { srv1.Close(); srv2.Close() }
	}
	t.Fatalf("portal fakes: no free port shared by %s and %s after 20 tries", host1, host2)
	return 0, "", "", nil
}

// Init starts each walk with a fresh session id and a running orb at
// ip1: the fake backends outlive every walk (Cleanup only closes
// leftover portals), so State always ends here without racing the
// listeners.
func (a *portalAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.id = row.ID
	if err := iorb.Restore(a.s.Home, portalState(a.id, iorb.StatusRunning, a.ip1)); err != nil {
		return err
	}
	a.gate.reset()
	return nil
}

// Cleanup closes a portal a walk left open, so the next walk's fake
// backends are not still relaying an old test's traffic on shared fds.
func (a *portalAdapter) Cleanup() error {
	if a.id == "" {
		return nil
	}
	return iorb.ClosePortal(a.s.Home, a.id, a.guest)
}

func portalState(session string, status iorb.Status, ip string) iorb.State {
	return iorb.State{Session: session, Status: status, IP: ip}
}

func (a *portalAdapter) view() (running, open bool, ip string, err error) {
	st, err := iorb.ReadState(a.s.Home, a.id)
	if err != nil {
		return false, false, "", err
	}
	for _, p := range st.Portals {
		if p.Guest == a.guest {
			open = true
		}
	}
	return st.Status == iorb.StatusRunning, open, st.IP, nil
}

// GetState is the Portal role's state: orbRunning and open come straight
// off state.json, and dial is read by actually connecting through the
// portal (when one is open) and asking which fake backend answered —
// the real product's dial pointer, not the adapter's memory of it.
func (a *portalAdapter) GetState() (map[string]any, error) {
	running, open, ip, err := a.view()
	if err != nil {
		return nil, err
	}
	orbIP := a.label(ip)
	if a.wrongOrbIP {
		orbIP = "ip1"
	}
	dial := ""
	if open {
		dial = a.probeDial()
	}
	return map[string]any{"orbRunning": running, "orbIP": orbIP, "open": open, "dial": dial}, nil
}

func (a *portalAdapter) label(ip string) string {
	switch ip {
	case a.ip1:
		return "ip1"
	case a.ip2:
		return "ip2"
	}
	return ""
}

// probeDial makes a real HTTP request through the portal's own
// listener, using the host port state.json records for it, and returns
// which fake backend answered ("" on any failure, including nothing
// listening, which fails the assertion it is meant to check).
func (a *portalAdapter) probeDial() string {
	st, err := iorb.ReadState(a.s.Home, a.id)
	if err != nil {
		return ""
	}
	host := -1
	for _, p := range st.Portals {
		if p.Guest == a.guest {
			host = p.Host
		}
	}
	if host < 0 {
		return ""
	}
	// DisableKeepAlives: a portal only re-points a connection that has
	// not dialed out yet ("connections already open are left alone",
	// portal.go); a reused keep-alive connection here would probe the
	// pipe an earlier check opened, not the dial a restart just set.
	cl := http.Client{Timeout: 2 * time.Second, Transport: &http.Transport{DisableKeepAlives: true}}
	resp, err := cl.Get("http://127.0.0.1:" + strconv.Itoa(host))
	if err != nil {
		return ""
	}
	defer resp.Body.Close()
	b, _ := io.ReadAll(resp.Body)
	return a.label(map[string]string{"ip1": a.ip1, "ip2": a.ip2}[strings.TrimSpace(string(b))])
}

func (a *portalAdapter) StopOrb() error {
	running, _, _, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(running) {
		return nil
	}
	_, err = iorb.MarkStopped(a.s.Home, a.id)
	return err
}

// RestartOrb is orb.Orb.Replace's contract after a real restart: the new
// state lands first, and RetargetPortals re-points every open portal at
// the new address in the same step, whether or not one happens to be
// open right now.
func (a *portalAdapter) RestartOrb() error {
	running, _, _, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(!running) {
		return nil
	}
	// Carries st.Portals over as writeState always does (restart.go's
	// comment on the real call): a restart must not forget an open
	// portal is recorded, only re-point where it dials.
	st, err := iorb.ReadState(a.s.Home, a.id)
	if err != nil {
		return err
	}
	st.Status, st.IP = iorb.StatusRunning, a.ip2
	if err := iorb.Restore(a.s.Home, st); err != nil {
		return err
	}
	iorb.RetargetPortals(a.id, a.ip2)
	return nil
}

func (a *portalAdapter) Open() error {
	running, open, _, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(!open && running) {
		return nil
	}
	_, err = iorb.OpenPortal(a.s.Home, a.id, a.guest, "test")
	return err
}

// Close has no require in the spec: an already-closed portal is a
// no-op in the product, which is what makes two racing closers safe.
func (a *portalAdapter) Close() error {
	if !a.gate.pass(true) {
		return nil
	}
	return iorb.ClosePortal(a.s.Home, a.id, a.guest)
}

var portalActions = map[string]map[string]fmbt.ActionFunc{"Portal": {
	"StopOrb":    action((*portalAdapter).StopOrb),
	"RestartOrb": action((*portalAdapter).RestartOrb),
	"Open":       action((*portalAdapter).Open),
	"Close":      action((*portalAdapter).Close),
}}

func portalOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

// orbPortalOpenCloseRaceHistory: nothing in this flow goes through a
// session's transcript (no model turn is ever run), so there is nothing
// to project off history.Read; it is registered only so
// TestHistoryTraces recognizes the spec's name if ever pointed at it.
func orbPortalOpenCloseRaceHistory([]history.Entry) []tracecheck.Step {
	return []tracecheck.Step{{Action: "Init", State: map[string]any{
		"Portal#0.orbRunning": true, "Portal#0.orbIP": "ip1", "Portal#0.open": false, "Portal#0.dial": "",
	}}}
}

func init() {
	historyProjections["orb_portal_open_close_race"] = orbPortalOpenCloseRaceHistory
}

func (a *portalAdapter) compare(want map[string]any) error {
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

// walkPortalPaths drives one generated path from Init, failing on the
// first step whose action errs, is refused by the adapter's own
// require, or leaves a state other than the path's.
func walkPortalPaths(t *testing.T, a *portalAdapter, paths []genPath, firstOnly bool) (mismatches []string) {
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
				f, ok := portalActions["Portal"][name]
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

func TestOrbPortalOpenCloseRacePaths(t *testing.T) {
	t.Parallel()
	a := newPortalAdapter(t)
	defer a.closeFakes()
	for _, m := range walkPortalPaths(t, a, loadPaths(t, "orb_portal_open_close_race"), false) {
		t.Error(m)
	}
}

// The random runs, in the exhaustive job only (runMBT skips otherwise).
func TestOrbPortalOpenCloseRace(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPortalAdapter(t)
	defer a.closeFakes()
	if err := runMBT(t, "orb_portal_open_close_race", a, portalActions, portalOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// The walks above prove nothing unless a wrong adapter fails them: this
// one always reports orbIP as "ip1", so a restart to ip2 is invisible.
func TestOrbPortalOpenCloseRaceCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newPortalAdapter(t)
	defer a.closeFakes()
	a.wrongOrbIP = true
	if len(walkPortalPaths(t, a, loadPaths(t, "orb_portal_open_close_race"), true)) == 0 {
		t.Fatal("walks whose adapter always reports orbIP=ip1 passed; the adapter is not checking state")
	}
}

// GetRoles/model plumbing: portalAdapter is both the fmbt.Model and the
// spec's one Portal role.
func (a *portalAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Portal", Index: 0}: a}, nil
}
