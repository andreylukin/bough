//go:build !windows

package mbt

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"maps"
	mrand "math/rand/v2"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"slices"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/orb_portals_consistency.fizz: one portal seen four ways — the
// owner's listener, state.json's record, the container's address and
// the Portal pane — against the real code on each side of it.
//
// The owner's side runs in this process: a real orb.Orb opened on a
// runtime built on container.Fake, and the real orb.OpenPortal,
// ClosePortal and CloseSessionPortals, so the listener is the product's
// own. serve is the Supervisor and API `bough serve` builds, behind an
// httptest server (a `bough serve` process would use the host's
// container runtime). The pane is what portal.tsx reads: GET
// /api/sessions/<id>/orb, and whatever answers on a live portal's URL.
//
// The "container" is two loopback responders on one guest port, one per
// address the fake hands out (127.0.0.1, and 127.0.0.2 or ::1). Only the
// responder at the container's current address answers ("up" while it
// runs, "down" while stopped); the other closes at once, like an address
// nothing holds. So what a listener dials is observed by going through
// it, never assumed.
//
// Two splits the product makes in one call are held open here:
//   - OpenPortal's listen and its recordPortal (AgentOpen / OpenRecord):
//     state.json is swapped for a named pipe while the open runs, so
//     recordPortal's re-read blocks until the walk's next action feeds
//     it the file as it is then. The listener's port is found among the
//     process's loopback sockets and confirmed through it.
//   - ensureRunningLocked's "starting" write and its start (Restart /
//     Resumed): the runtime parks Start.
//
// A killed owner is played by closing its listeners and putting
// state.json back byte for byte: the OS closes a SIGKILLed process's
// sockets and nothing writes the file.

const (
	portalsSpec  = "orb_portals_consistency"
	portalsRole  = "Portal#0."
	portalsProto = "web"
)

// portalsRuntime is container.Fake with an address that moves on each
// start and a Start that can be parked.
type portalsRuntime struct {
	*container.Fake
	token string // in every answer, so a probe knows it reached us
	addrs [2]string
	guest int

	mu     sync.Mutex
	name   string // this walk's container
	addr   string // its current address
	hold   bool   // park the next Start
	parked chan error
	lns    []net.Listener
}

func newPortalsRuntime(t *testing.T) *portalsRuntime {
	b := make([]byte, 8)
	rand.Read(b)
	r := &portalsRuntime{Fake: container.NewFake(), token: hex.EncodeToString(b)}
	// A second loopback address: 127.0.0.2 where the host routes all of
	// 127/8 (Linux), else ::1 (macOS has only 127.0.0.1 on lo0).
	second := "::1"
	if ln, err := net.Listen("tcp", "127.0.0.2:0"); err == nil {
		ln.Close()
		second = "127.0.0.2"
	}
	r.addrs = [2]string{"127.0.0.1", second}
	for try := 0; ; try++ {
		a, err := net.Listen("tcp", "127.0.0.1:0")
		if err != nil {
			t.Fatal(err)
		}
		port := a.Addr().(*net.TCPAddr).Port
		b, err := net.Listen("tcp", net.JoinHostPort(second, strconv.Itoa(port)))
		if err != nil {
			a.Close()
			if try > 20 {
				t.Fatalf("no port free on both 127.0.0.1 and %s: %v", second, err)
			}
			continue
		}
		r.guest, r.lns = port, []net.Listener{a, b}
		break
	}
	for i, ln := range r.lns {
		go r.respond(ln, r.addrs[i])
	}
	t.Cleanup(func() {
		for _, ln := range r.lns {
			ln.Close()
		}
	})
	return r
}

// respond is the guest's server at one address.
func (r *portalsRuntime) respond(ln net.Listener, at string) {
	for {
		c, err := ln.Accept()
		if err != nil {
			return
		}
		r.mu.Lock()
		cur, name := r.addr, r.name
		r.mu.Unlock()
		if at == cur {
			st, _ := r.Fake.Inspect(context.Background(), name)
			word := "down"
			if st == container.StateRunning {
				word = "up"
			}
			fmt.Fprintf(c, "%s %s %s\n", word, r.token, at)
		}
		c.Close()
	}
}

func (r *portalsRuntime) Address(ctx context.Context, name string) (string, error) {
	if st, _ := r.Fake.Inspect(ctx, name); st != container.StateRunning {
		return "", nil
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.addr, nil
}

func (r *portalsRuntime) Start(ctx context.Context, spec container.RunSpec) error {
	r.mu.Lock()
	if !r.hold {
		r.mu.Unlock()
		return r.Fake.Start(ctx, spec)
	}
	r.hold = false
	ch := make(chan error)
	r.parked = ch
	r.mu.Unlock()
	if err := <-ch; err != nil {
		return err
	}
	return r.Fake.Start(ctx, spec)
}

func (r *portalsRuntime) isParked() bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.parked != nil
}

// release lets the parked Start go, at addr (ignored on an error).
func (r *portalsRuntime) release(addr string, err error) {
	r.mu.Lock()
	ch := r.parked
	r.parked = nil
	if err == nil {
		r.addr = addr
	}
	r.mu.Unlock()
	if ch != nil {
		ch <- err
	}
}

func (r *portalsRuntime) other(addr string) string {
	if addr == r.addrs[0] {
		return r.addrs[1]
	}
	return r.addrs[0]
}

// portalsFields is the Portal role's state.
type portalsFields struct {
	owner, file, ctr, pane                               string
	ln, dialOK, rec, other, pending, moved, stale, errLn bool
	lns                                                  int
}

func (f portalsFields) state() map[string]any {
	return map[string]any{
		"owner": f.owner, "file": f.file, "ctr": f.ctr, "pane": f.pane,
		"ln": f.ln, "lns": f.lns, "dial_ok": f.dialOK, "rec": f.rec, "other": f.other,
		"pending": f.pending, "moved": f.moved, "stale": f.stale, "err_ln": f.errLn,
	}
}

// The spec's reaches and view, line for line.
func (f portalsFields) reaches() bool { return f.ln && f.dialOK && f.ctr == "running" }

func (f portalsFields) view() string {
	switch {
	case !f.rec:
		return "empty"
	case f.ln && f.reaches():
		return "live"
	case f.ln:
		return "stalled"
	case f.other:
		return "foreign"
	}
	return "dead"
}

type portalsOpenResult struct {
	ps  orb.PortalState
	err error
}

// portalsPending is an open whose listener is up and whose recordPortal
// is blocked reading the pipe w.
type portalsPending struct {
	w    *os.File
	done chan portalsOpenResult
	host int
}

// portalsAbandoned is a killed owner's restart, parked in Start.
type portalsAbandoned struct {
	start chan error
	done  chan struct{}
}

type portalsAdapter struct {
	t    *testing.T
	home string
	hist string
	rt   *portalsRuntime
	sup  *serve.Supervisor
	api  *serve.API
	srv  *httptest.Server
	proj projectdef.Project

	n  int
	id string
	o  *orb.Orb

	owner   string
	hosts   map[int]bool // ports OpenPortal gave this walk, while they accept
	lnDial  string       // the address the current listener was opened on
	dialOK  bool         // the spec keeps it after AgentClose
	stale   bool         // kept while the owner is gone
	last    int          // the host port state.json last recorded
	foreign map[int]net.Listener
	errHost int // the listener whose open returned an error
	pane    string
	pend    *portalsPending
	restart chan struct{} // closed when the restart's Command returns

	// Starts a killed owner left parked: released when the test ends.
	abandoned []portalsAbandoned

	action   string
	trace    []tracecheck.Step
	walks    [][]tracecheck.Step
	stutters [][]tracecheck.Step

	// killForgets is the wrong-adapter bug: a killed owner's state.json
	// loses its portals, as if the process had cleaned up.
	killForgets bool
}

func newPortalsAdapter(t *testing.T) *portalsAdapter {
	root, err := os.MkdirTemp("", "bptl-")
	if err != nil {
		t.Fatal(err)
	}
	root, _ = filepath.EvalSymlinks(root)
	a := &portalsAdapter{t: t, home: filepath.Join(root, "home"), rt: newPortalsRuntime(t)}
	a.hist = filepath.Join(a.home, ".bough", "history")
	if err := os.MkdirAll(a.hist, 0o755); err != nil {
		t.Fatal(err)
	}
	if a.proj, err = projectdef.CreateEmpty(a.home, "ptl", "portals"); err != nil {
		t.Fatal(err)
	}
	a.sup, err = serve.NewSupervisor(serve.Options{
		Exe: "/bin/true", HistDir: a.hist, Home: a.home, Runtime: a.rt,
		MetaPath: filepath.Join(a.home, ".bough", "serve", "meta.json"),
	})
	if err != nil {
		t.Fatal(err)
	}
	a.api = serve.NewAPI(a.sup)
	a.srv = httptest.NewServer(a.api)
	t.Cleanup(func() {
		a.endWalk()
		for _, ab := range a.abandoned {
			if ab.start != nil {
				ab.start <- errors.New("test over")
			}
			<-ab.done
		}
		a.srv.Close()
		a.sup.Close()
		os.RemoveAll(root)
	})
	return a
}

func (a *portalsAdapter) statePath() string {
	return filepath.Join(orb.Dir(a.home, a.id), "state.json")
}

// Init opens a fresh session's orb: running, no portal, the pane empty.
func (a *portalsAdapter) Init() (map[string]any, error) {
	a.endWalk()
	a.n++
	// orb's listeners are process-wide and keyed by session: two tests
	// of this package sharing ids would close each other's portals.
	a.id = fmt.Sprintf("ptl%s%04d", a.rt.token[:6], a.n)
	b, _ := json.Marshal(history.Entry{Seq: 1, At: time.Now(), Kind: "meta",
		Data: map[string]any{"cwd": a.home, "mode": "project", "project": a.proj.Slug}})
	if err := os.WriteFile(filepath.Join(a.hist, a.id+".jsonl"), append(b, '\n'), 0o644); err != nil {
		return nil, err
	}
	a.rt.mu.Lock()
	a.rt.name, a.rt.addr = container.OrbName(a.id), a.rt.addrs[0]
	a.rt.mu.Unlock()
	ctx, cancel := actionCtx()
	defer cancel()
	o, err := orb.Open(ctx, a.rt, a.home, a.id, a.proj, "")
	if err != nil {
		return nil, err
	}
	a.o, a.owner, a.pane = o, "live", "empty"
	a.hosts, a.foreign = map[int]bool{}, map[int]net.Listener{}
	a.lnDial, a.dialOK, a.stale, a.last, a.errHost = "", false, false, 0, 0
	a.trace, a.action = nil, "Init"
	return a.GetState()
}

// endWalk leaves nothing of the walk running: the open and the restart
// it held are let go, its listeners closed, its orb stopped.
func (a *portalsAdapter) endWalk() {
	if a.o == nil {
		return
	}
	a.dropPending()
	if a.restart != nil { // only a live owner's: a kill abandons it
		a.rt.release(a.rt.addrs[0], nil)
		<-a.restart
		a.restart = nil
	}
	if a.owner == "live" {
		orb.CloseSessionPortals(a.home, a.id)
		ctx, cancel := actionCtx()
		a.o.Stop(ctx)
		cancel()
	}
	for _, ln := range a.foreign {
		ln.Close()
	}
	a.foreign = nil
	if len(a.trace) > 0 {
		a.walks = append(a.walks, a.trace)
	}
	a.trace = nil
	// The session list is read on every request; keep it to this walk.
	os.Remove(filepath.Join(a.hist, a.id+".jsonl"))
	a.o = nil
}

// dropPending ends an open in flight the way its process's end does:
// the call dies before it writes. The pipe is closed empty, recordPortal
// reads no state and writes nothing.
func (a *portalsAdapter) dropPending() {
	if a.pend == nil {
		return
	}
	a.pend.w.Close()
	<-a.pend.done
	a.pend = nil
}

// --- observing ---

// fetch dials a loopback port and reads what answers ("" for nothing).
func fetch(port int, wait time.Duration) (accepted bool, got string) {
	c, err := net.DialTimeout("tcp", "127.0.0.1:"+strconv.Itoa(port), time.Second)
	if err != nil {
		return false, ""
	}
	defer c.Close()
	c.SetDeadline(time.Now().Add(wait))
	b, _ := io.ReadAll(c)
	return true, string(b)
}

// guestAnswer reports whether s came from this test's container.
func (r *portalsRuntime) guestAnswer(s string) bool {
	f := strings.Fields(s)
	return len(f) == 3 && (f[0] == "up" || f[0] == "down") && f[1] == r.token
}

func (a *portalsAdapter) observe() (portalsFields, error) {
	f := portalsFields{owner: a.owner, pane: a.pane}
	st, err := orb.ReadState(a.home, a.id)
	if err != nil {
		return f, err
	}
	f.file = string(st.Status)
	cs, err := a.rt.Inspect(context.Background(), container.OrbName(a.id))
	if err != nil {
		return f, err
	}
	f.ctr = string(cs)
	var rec *orb.PortalState
	for i, p := range st.Portals {
		if p.Guest == a.rt.guest {
			rec = &st.Portals[i]
		}
	}
	f.rec = rec != nil
	if rec != nil {
		a.last = rec.Host
	}
	var live []int
	var answer string
	for _, h := range slices.Sorted(maps.Keys(a.hosts)) {
		if _, ok := a.foreign[h]; ok {
			delete(a.hosts, h)
			continue
		}
		ok, got := fetch(h, 2*time.Second)
		if !ok {
			delete(a.hosts, h)
			continue
		}
		if len(live) == 0 {
			answer = got
		}
		live = append(live, h)
	}
	f.lns, f.ln = len(live), len(live) > 0
	if f.ln {
		a.dialOK = a.rt.guestAnswer(answer)
		if rec != nil && !slices.Contains(live, rec.Host) {
			return f, fmt.Errorf("state.json records portal port %d, the owner listens on %v", rec.Host, live)
		}
	}
	f.dialOK = a.dialOK
	f.pending = a.pend != nil
	f.moved = f.pending && !f.dialOK
	switch {
	case !f.rec:
		a.stale = false
	case f.ln:
		a.stale = !f.dialOK
	}
	f.stale = a.stale
	_, f.other = a.foreign[a.last]
	f.errLn = a.errHost != 0 && slices.Contains(live, a.errHost)
	return f, nil
}

// apiView is what the pane would show after a poll now: the orb's
// portals as serve reports them, and for a live one, what answers at
// its URL.
func (a *portalsAdapter) apiView() (string, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	var detail struct {
		Orb *struct {
			Portals []serve.PortalView `json:"portals"`
		} `json:"orb"`
	}
	if err := a.get(ctx, "/api/sessions/"+a.id+"/orb", &detail); err != nil {
		return "", err
	}
	if detail.Orb == nil {
		return "", fmt.Errorf("session %s has no orb", a.id)
	}
	var row struct {
		Session serve.Row `json:"session"`
	}
	if err := a.get(ctx, "/api/sessions/"+a.id, &row); err != nil {
		return "", err
	}
	view := "empty"
	for _, p := range detail.Orb.Portals {
		if p.Guest != a.rt.guest {
			continue
		}
		view = "dead"
		if !p.Live {
			break
		}
		_, got := fetch(p.Host, 2*time.Second)
		switch {
		case strings.HasPrefix(got, "up ") && a.rt.guestAnswer(got):
			view = "live"
		case strings.HasPrefix(got, "foreign "+a.rt.token):
			view = "foreign"
		default:
			view = "stalled"
		}
	}
	rowLive := row.Session.Orb != nil && slices.Contains(row.Session.Orb.Portals, a.rt.guest)
	if detailLive := view == "live" || view == "stalled" || view == "foreign"; rowLive != detailLive {
		return "", fmt.Errorf("the session row says portal live %v, the orb detail %s", rowLive, view)
	}
	return view, nil
}

func (a *portalsAdapter) get(ctx context.Context, path string, out any) error {
	return a.call(ctx, http.MethodGet, path, out)
}

func (a *portalsAdapter) call(ctx context.Context, method, path string, out any) error {
	req, err := http.NewRequestWithContext(ctx, method, a.srv.URL+path, nil)
	if err != nil {
		return err
	}
	resp, err := a.srv.Client().Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		b, _ := io.ReadAll(resp.Body)
		return fmt.Errorf("%s %s = %d: %s", method, path, resp.StatusCode, b)
	}
	if out == nil {
		return nil
	}
	return json.NewDecoder(resp.Body).Decode(out)
}

// GetState is the role's state after checking that serve's view of the
// portal is the spec's view of it, and records the step just taken.
func (a *portalsAdapter) GetState() (map[string]any, error) {
	f, err := a.observe()
	if err != nil {
		return nil, err
	}
	a.record(f)
	v, err := a.apiView()
	if err != nil {
		return nil, err
	}
	if want := f.view(); v != want {
		return nil, fmt.Errorf("serve shows the portal %s, the model's view of %v is %s", v, f.state(), want)
	}
	return f.state(), nil
}

func (a *portalsAdapter) record(f portalsFields) {
	if a.action == "" {
		return
	}
	name := a.action
	if name != "Init" {
		name = portalsRole + name
	}
	a.action = ""
	step := tracecheck.Step{Action: name, State: portalsQualify(f.state())}
	// fizz has no link for an action that changes nothing (a refused or
	// cached open): such a step is checked apart, like orb_lifecycle's.
	if n := len(a.trace); n > 0 && name != "Init" && reflect.DeepEqual(a.trace[n-1].State, step.State) {
		a.stutters = append(a.stutters, append(slices.Clone(a.trace), step))
		return
	}
	a.trace = append(a.trace, step)
}

func portalsQualify(st map[string]any) map[string]any {
	out := map[string]any{}
	for k, v := range st {
		out[portalsRole+k] = v
	}
	return out
}

// --- actions ---

type portalsAction struct {
	name    string
	require func(portalsFields) bool
	do      func(*portalsAdapter, portalsFields) error
}

var portalsActions = []portalsAction{
	{"AgentOpen", func(f portalsFields) bool { return f.owner == "live" && !f.pending }, (*portalsAdapter).agentOpen},
	{"OpenRecord", func(f portalsFields) bool { return f.pending }, (*portalsAdapter).openRecord},
	{"OpenRecordFails", func(f portalsFields) bool { return f.pending }, (*portalsAdapter).openRecordFails},
	{"AgentClose", func(f portalsFields) bool { return f.owner == "live" && !f.pending && (f.ln || f.rec) }, (*portalsAdapter).agentClose},
	{"SessionExit", func(f portalsFields) bool { return f.owner == "live" }, (*portalsAdapter).sessionExit},
	{"OwnerKilled", func(f portalsFields) bool { return f.owner == "live" }, (*portalsAdapter).ownerKilled},
	{"Restart", func(f portalsFields) bool {
		return f.owner == "live" && f.ctr == "stopped" && (f.file == "running" || f.file == "stopped")
	}, (*portalsAdapter).restartOrb},
	{"Resumed", func(f portalsFields) bool { return f.owner == "live" && f.file == "starting" }, (*portalsAdapter).resumed},
	{"StopOrb", func(f portalsFields) bool { return f.ctr == "running" && f.file == "running" }, (*portalsAdapter).stopOrb},
	{"ExternalStop", func(f portalsFields) bool { return f.ctr == "running" }, (*portalsAdapter).externalStop},
	{"OtherBindsPort", func(f portalsFields) bool { return f.rec && !f.ln && !f.other }, (*portalsAdapter).otherBindsPort},
	{"PanePoll", func(f portalsFields) bool { return f.pane != f.view() }, (*portalsAdapter).panePoll},
}

func portalsActionNamed(name string) (portalsAction, bool) {
	for _, act := range portalsActions {
		if act.name == name {
			return act, true
		}
	}
	return portalsAction{}, false
}

// act takes one action if its require holds of the observed state, and
// returns the state after it.
func (a *portalsAdapter) act(name string) (map[string]any, error) {
	if name == "end" {
		// fizz links a state with nothing enabled to itself as "end".
		f, err := a.observe()
		if err != nil {
			return nil, err
		}
		for _, act := range portalsActions {
			if act.require(f) {
				return nil, fmt.Errorf("the model ends in %v, where %s is enabled", f.state(), act.name)
			}
		}
		return a.GetState()
	}
	act, ok := portalsActionNamed(name)
	if !ok {
		return nil, fmt.Errorf("no action %s", name)
	}
	f, err := a.observe()
	if err != nil {
		return nil, err
	}
	if !act.require(f) {
		return nil, fmt.Errorf("%s is not enabled in %v", name, f.state())
	}
	if err := act.do(a, f); err != nil {
		return nil, fmt.Errorf("%s: %w", name, err)
	}
	a.action = name
	return a.GetState()
}

// agentOpen is tools.portal.open. An open that makes a listener is held
// between its listen and its recordPortal (see the file comment).
func (a *portalsAdapter) agentOpen(f portalsFields) error {
	if f.file != "running" || f.ln {
		ps, err := orb.OpenPortal(a.home, a.id, a.rt.guest, portalsProto)
		switch {
		case f.file != "running" && err == nil:
			return fmt.Errorf("opened a portal on a %s orb", f.file)
		case f.file == "running" && err != nil:
			return fmt.Errorf("a reopen failed: %w", err)
		case f.file == "running" && !a.hosts[ps.Host]:
			a.hosts[ps.Host] = true // a second listener: lns says so
		}
		return nil
	}
	path := a.statePath()
	content, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	var st orb.State
	if err := json.Unmarshal(content, &st); err != nil {
		return err
	}
	before := loopbackPorts()
	holds := [2]string{path + ".hold1", path + ".hold2"}
	ok := false
	defer func() {
		if !ok { // never leave a pipe where serve reads state.json
			for _, h := range holds {
				unblock(h)
				os.Remove(h)
			}
			writeFileAtomic(path, content)
		}
	}()
	if err := fifoAt(path, holds[0]); err != nil {
		return err
	}
	done := make(chan portalsOpenResult, 1)
	go func() {
		ps, err := orb.OpenPortal(a.home, a.id, a.rt.guest, portalsProto)
		done <- portalsOpenResult{ps, err}
	}()
	w1, err := openWriter(holds[0], done)
	if err != nil {
		return err
	}
	// The second read, recordPortal's, finds the second pipe.
	if err := fifoAt(path, holds[1]); err != nil {
		w1.Close()
		return err
	}
	w1.Write(content)
	w1.Close()
	w2, err := openWriter(holds[1], done)
	if err != nil {
		return err
	}
	if err := writeFileAtomic(path, content); err != nil {
		w2.Close()
		return err
	}
	for _, h := range holds {
		os.Remove(h)
	}
	ok = true
	a.pend = &portalsPending{w: w2, done: done}
	after := loopbackPorts()
	for p := range after {
		if before[p] {
			continue
		}
		if _, got := fetch(p, 300*time.Millisecond); a.rt.guestAnswer(got) {
			a.pend.host = p
			break
		}
	}
	if a.pend.host == 0 {
		return errors.New("the open is recording, and no new loopback listener reaches the container")
	}
	a.hosts[a.pend.host] = true
	a.lnDial, a.errHost = st.IP, 0
	return nil
}

// openRecord lets recordPortal read state.json as it is now.
func (a *portalsAdapter) openRecord(portalsFields) error {
	content, err := os.ReadFile(a.statePath())
	if err != nil {
		return err
	}
	p := a.pend
	a.pend = nil
	p.w.Write(content)
	p.w.Close()
	r := <-p.done
	if r.err != nil {
		return fmt.Errorf("OpenPortal: %w", r.err)
	}
	if r.ps.Host != p.host {
		return fmt.Errorf("OpenPortal returned port %d, its listener is %d", r.ps.Host, p.host)
	}
	return nil
}

// openRecordFails makes recordPortal's write fail: the orb dir is a
// file for the moment it writes.
func (a *portalsAdapter) openRecordFails(portalsFields) error {
	dir := orb.Dir(a.home, a.id)
	content, err := os.ReadFile(a.statePath())
	if err != nil {
		return err
	}
	if err := os.Rename(dir, dir+".away"); err != nil {
		return err
	}
	defer func() {
		os.Remove(dir)
		os.Rename(dir+".away", dir)
	}()
	if err := os.WriteFile(dir, nil, 0o644); err != nil {
		return err
	}
	p := a.pend
	a.pend = nil
	p.w.Write(content)
	p.w.Close()
	r := <-p.done
	if r.err == nil {
		return errors.New("OpenPortal succeeded with state.json unwritable")
	}
	if r.ps.Host != p.host {
		return fmt.Errorf("OpenPortal returned port %d, its listener is %d", r.ps.Host, p.host)
	}
	a.errHost = p.host
	return nil
}

func (a *portalsAdapter) agentClose(portalsFields) error {
	a.lnDial, a.errHost = "", 0
	return orb.ClosePortal(a.home, a.id, a.rt.guest)
}

// sessionExit is the row unmounting: the portals closed, Orb.Stop. A
// restart in flight holds the orb's lock, so Stop waits for it.
func (a *portalsAdapter) sessionExit(portalsFields) error {
	a.dropPending()
	if a.restart != nil {
		a.rt.release(a.rt.other(a.rt.addr), nil)
		<-a.restart
		a.restart = nil
	}
	orb.CloseSessionPortals(a.home, a.id)
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.o.Stop(ctx); err != nil {
		return err
	}
	a.owner, a.dialOK, a.lnDial, a.errHost = "exited", false, "", 0
	return nil
}

// ownerKilled: the listeners go with the process, state.json stays as
// it was, and a restart in flight never finishes.
func (a *portalsAdapter) ownerKilled(portalsFields) error {
	content, err := os.ReadFile(a.statePath())
	if err != nil {
		return err
	}
	a.dropPending()
	orb.CloseSessionPortals(a.home, a.id)
	if !a.killForgets {
		if err := writeFileAtomic(a.statePath(), content); err != nil {
			return err
		}
	}
	if a.restart != nil {
		// Its Start stays parked until the test ends; the next walk
		// parks its own.
		a.rt.mu.Lock()
		a.abandoned = append(a.abandoned, portalsAbandoned{start: a.rt.parked, done: a.restart})
		a.rt.parked = nil
		a.rt.mu.Unlock()
		a.restart = nil
	}
	a.owner, a.dialOK, a.lnDial, a.errHost = "killed", false, "", 0
	return nil
}

// restartOrb is the child's next exec finding the container down:
// Orb.Command's ensureRunningLocked, held at the runtime's Start.
func (a *portalsAdapter) restartOrb(portalsFields) error {
	a.rt.mu.Lock()
	a.rt.hold = true
	a.rt.mu.Unlock()
	done := make(chan struct{})
	o := a.o
	go func() {
		o.Command(context.Background(), "true")
		close(done)
	}()
	a.restart = done
	return waitUntil("the restart's Start", func() bool {
		select {
		case <-done:
			return true
		default:
		}
		return a.rt.isParked()
	})
}

// resumed lets the start go at a new address: one the listener, if
// there is one, does not dial.
func (a *portalsAdapter) resumed(portalsFields) error {
	next := a.rt.other(a.rt.addr)
	if a.lnDial != "" {
		next = a.rt.other(a.lnDial)
	}
	a.rt.release(next, nil)
	<-a.restart
	a.restart = nil
	return nil
}

func (a *portalsAdapter) stopOrb(portalsFields) error {
	ctx, cancel := actionCtx()
	defer cancel()
	return a.call(ctx, http.MethodPost, "/api/sessions/"+a.id+"/orb/stop", nil)
}

func (a *portalsAdapter) externalStop(portalsFields) error {
	return a.rt.Fake.Stop(context.Background(), container.OrbName(a.id))
}

// otherBindsPort: another server takes the recorded host port.
func (a *portalsAdapter) otherBindsPort(portalsFields) error {
	ln, err := net.Listen("tcp", "127.0.0.1:"+strconv.Itoa(a.last))
	if err != nil {
		return err
	}
	a.foreign[a.last] = ln
	go func() {
		for {
			c, err := ln.Accept()
			if err != nil {
				return
			}
			fmt.Fprintf(c, "foreign %s\n", a.rt.token)
			c.Close()
		}
	}()
	return nil
}

func (a *portalsAdapter) panePoll(portalsFields) error {
	v, err := a.apiView()
	if err != nil {
		return err
	}
	a.pane = v
	return nil
}

// --- pipes and sockets ---

// fifoAt puts a new named pipe at path, also named hold so the adapter
// can open it after path is replaced.
func fifoAt(path, hold string) error {
	if err := syscall.Mkfifo(hold, 0o644); err != nil {
		return err
	}
	tmp := hold + ".link"
	if err := os.Link(hold, tmp); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}

// openWriter waits for a reader to block on the pipe and opens its
// write end, which the reader then waits on for the content.
func openWriter(hold string, done chan portalsOpenResult) (*os.File, error) {
	deadline := time.Now().Add(actionTimeout)
	for {
		fd, err := syscall.Open(hold, syscall.O_WRONLY|syscall.O_NONBLOCK|syscall.O_CLOEXEC, 0)
		if err == nil {
			if err := syscall.SetNonblock(fd, false); err != nil {
				syscall.Close(fd)
				return nil, err
			}
			return os.NewFile(uintptr(fd), hold), nil
		}
		if !errors.Is(err, syscall.ENXIO) {
			return nil, err
		}
		select {
		case r := <-done:
			done <- r
			return nil, fmt.Errorf("OpenPortal returned before reading state.json: %+v, %v", r.ps, r.err)
		default:
		}
		if time.Now().After(deadline) {
			return nil, errors.New("OpenPortal never read state.json")
		}
		time.Sleep(time.Millisecond)
	}
}

// unblock lets a reader stuck on the pipe read an empty file.
func unblock(hold string) {
	if fd, err := syscall.Open(hold, syscall.O_RDWR|syscall.O_NONBLOCK, 0); err == nil {
		syscall.Close(fd)
	}
}

// loopbackPorts is the 127.0.0.1 ports of this process's sockets.
func loopbackPorts() map[int]bool {
	out := map[int]bool{}
	ents, _ := os.ReadDir("/dev/fd")
	for _, e := range ents {
		fd, err := strconv.Atoi(e.Name())
		if err != nil {
			continue
		}
		sa, err := syscall.Getsockname(fd)
		if err != nil {
			continue
		}
		if in4, ok := sa.(*syscall.SockaddrInet4); ok && in4.Addr == [4]byte{127, 0, 0, 1} {
			out[in4.Port] = true
		}
	}
	return out
}

// --- walks ---

// walkPortalsPaths drives the adapter down every generated walk and
// returns the first step whose state is not the spec's.
func walkPortalsPaths(t *testing.T, a *portalsAdapter, cover tracecheck.Cover) error {
	t.Helper()
	b, err := pathsJSONCover(portalsSpec, cover)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []tracecheck.Walk `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		t.Fatal(err)
	}
	if len(doc.Paths) == 0 {
		t.Fatal("no walks")
	}
	for pi, p := range doc.Paths {
		// The walk so far, for a failure's message.
		so := func(si int) string {
			var names []string
			for _, s := range p.Trace[1 : si+1] {
				names = append(names, strings.TrimPrefix(s.Action, portalsRole))
			}
			return strings.Join(names, ", ")
		}
		for si, s := range p.Trace {
			var got map[string]any
			var err error
			if si == 0 {
				got, err = a.Init()
			} else {
				got, err = a.act(strings.TrimPrefix(s.Action, portalsRole))
			}
			if err != nil {
				return fmt.Errorf("walk %d step %d (%s): %w\nwalk: %s", pi, si, s.Action, err, so(si))
			}
			if diff := portalsDiff(s.State, portalsQualify(got)); diff != "" {
				return fmt.Errorf("walk %d step %d (%s): %s\nwalk: %s", pi, si, s.Action, diff, so(si))
			}
		}
	}
	a.endWalk()
	t.Logf("%d generated walks (%s) matched step for step", len(doc.Paths), cover)
	return nil
}

func portalsDiff(want, got map[string]any) string {
	var diffs []string
	for _, k := range slices.Sorted(maps.Keys(want)) {
		if !strings.HasPrefix(k, portalsRole) {
			continue
		}
		wj, _ := json.Marshal(want[k])
		gj, _ := json.Marshal(got[k])
		if string(wj) != string(gj) {
			diffs = append(diffs, fmt.Sprintf("%s: spec %s, server %s", k, wj, gj))
		}
	}
	return strings.Join(diffs, "; ")
}

// portalsGuided walks the adapter at random among the actions its own
// view enables — the cached and refused opens included, which no
// generated walk takes because fizz has no link for a step that changes
// nothing.
func portalsGuided(a *portalsAdapter, walks, steps int, seed uint64) error {
	r := mrand.New(mrand.NewPCG(seed, seed))
	for w := range walks {
		if _, err := a.Init(); err != nil {
			return err
		}
		for s := range steps {
			f, err := a.observe()
			if err != nil {
				return err
			}
			var on []string
			for _, act := range portalsActions {
				if act.require(f) {
					on = append(on, act.name)
				}
			}
			if len(on) == 0 {
				break
			}
			name := on[r.IntN(len(on))]
			if _, err := a.act(name); err != nil {
				return fmt.Errorf("guided walk %d step %d: %w", w, s, err)
			}
		}
	}
	a.endWalk()
	return nil
}

// checkPortalsWalks replays every walk the adapter lived on the graph:
// the trace check for a flow with no session transcript.
func checkPortalsWalks(a *portalsAdapter) error {
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath(portalsSpec)), "..", "testdata", portalsSpec))
	if err != nil {
		return err
	}
	for i, tr := range a.walks {
		if v := g.Check(tr); v != nil {
			b, _ := json.Marshal(tr)
			return fmt.Errorf("walk %d is not a path in the model: %v\ntrace: %s", i, v, b)
		}
	}
	for _, tr := range a.stutters {
		v := g.Check(tr)
		if v != nil && (v.Index != len(tr)-1 || !strings.Contains(v.Reason, "is not enabled")) {
			b, _ := json.Marshal(tr)
			return fmt.Errorf("step %s left the state unchanged where the model changes it: %v\ntrace: %s", tr[len(tr)-1].Action, v, b)
		}
	}
	return nil
}

func TestOrbPortalsConsistencyPaths(t *testing.T) {
	t.Parallel()
	a := newPortalsAdapter(t)
	if err := walkPortalsPaths(t, a, envCover()); err != nil {
		t.Fatal(err)
	}
	seed := uint64(time.Now().UnixNano())
	if s := os.Getenv("PORTALS_WALK_SEED"); s != "" {
		fmt.Sscan(s, &seed)
	}
	if err := portalsGuided(a, 30, 25, seed); err != nil {
		t.Fatalf("PORTALS_WALK_SEED=%d: %v", seed, err)
	}
	if err := checkPortalsWalks(a); err != nil {
		t.Fatalf("PORTALS_WALK_SEED=%d: %v", seed, err)
	}
	t.Logf("%d walks replayed on the graph, %d no-op steps", len(a.walks), len(a.stutters))
}

// A killed owner whose record vanished is the tidy system F1 says this
// is not: a run that plays the kill that way must fail, or the walks
// prove nothing.
func TestOrbPortalsConsistencyPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newPortalsAdapter(t)
	a.killForgets = true
	err := walkPortalsPaths(t, a, tracecheck.CoverStates)
	if err == nil {
		t.Fatal("walks whose killed owner drops the record passed; the walk is not checking state")
	}
	t.Logf("caught: %v", err)
}
