//go:build !windows

package mbt

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"
	_ "unsafe" // go:linkname, for the orb package's host seams below

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/orb_relay_proxy_auth.fizz against internal/orb as a session child
// runs it: orb.Open, Orb.Command, Orb.Stop, the real proxy listener and
// relay handler, the real guest shim (bash over /dev/tcp) and the real
// token file. Nothing of this flow goes through serve or a model turn:
// the proxy lives in the session's child process, so the adapter plays
// that process in-process, over container.Fake, the way the orb
// package's own tests do.
//
// What is faked, and why:
//   - The container (relayRuntime): container.Fake runs a guest exec on
//     the host. The wrapper gives that exec the container's create env
//     under the exec's own env, as `container exec -e` does, so a guest
//     whose exec env carries no token still has the one it was created
//     with. It answers the guest's resolv.conf with the walk's gateway.
//   - The gateway: the product refuses a loopback gateway, so the fake
//     gateway is 0.0.0.0 and the listener binds every interface. The
//     guest's network is the wrapper's: a call to the listener started
//     on the current gateway goes to it through a loopback forwarder,
//     one to a listener on a gateway the guest no longer has goes to a
//     closed port.
//   - A container stop does not kill the guest's processes here, so a
//     call in flight lives through one, as the spec has it. A real stop
//     kills the shim; the spec's res then describes the host side only.
//   - The host bough: the relay runs os.Executable(), which here is this
//     test binary; init below makes it a stand-in that records what it
//     was asked (argv, AGENT_BROWSER_SESSION) and exits when the adapter
//     says. A relayTimeout kill is played by the stand-in SIGKILLing
//     itself, which is what exec.CommandContext does at the deadline.
//   - A child crash drops the *orb.Orb without Stop. Its listener stays
//     open in this process (a real crash takes it along) and is closed
//     at the walk's end; the model has no way back to it either.
//
// Every field is read off the product: tokens off the token file and
// the create env (numbered by the order they appear), o.token and the
// listener's address off the env Orb.Command hands the runtime, the
// token the listener checks by asking it (a relay call it refuses with
// 401 or answers past the gate), and a call's outcome off the shim's
// exit code and stderr and the stand-in's record.

// The host seams the orb package's own TestMain replaces: the
// developer's ~/.zshrc (an interactive zsh per process) and host CLIs
// (git config --global, gh auth token). Reached by linkname because
// they are unexported and this suite may not read the developer's
// shell or git config either; see init.
//
//go:linkname orbHostShellEnv github.com/andreylukin/bough/internal/orb.hostShellEnv
var orbHostShellEnv func() map[string]string

//go:linkname orbHostCommand github.com/andreylukin/bough/internal/orb.hostCommand
var orbHostCommand func(string, ...string) string

// relayCtl marks the stand-in host bough's second argument: the call's
// control dir. Every relayed argv the adapter makes carries it, so a
// relay that ran a command it should refuse runs the stand-in, which
// records it, and never this test binary's tests.
const relayCtl = "relayctl:"

func init() {
	if len(os.Args) >= 3 && strings.HasPrefix(os.Args[2], relayCtl) {
		// syscall.Exit, not os.Exit: a -race binary sleeps a second at
		// exit (GORACE atexit_sleep_ms), and every relayed call waited
		// for it.
		syscall.Exit(fakeHostBough(os.Args[1:]))
	}
	orbHostShellEnv = func() map[string]string { return nil }
	orbHostCommand = func(string, ...string) string { return "" }
}

// relayHostOut is the stand-in's stdout: past the 2 KiB Go writes before
// it chunks a response it cannot size, which is what the relay's
// Content-Length is for (ShimExitIsHosts' sized).
var relayHostOut = strings.Repeat("host stdout, sized past 2KiB\n", 160)

// hostCall is what the stand-in host bough was asked.
type hostCall struct {
	Args    []string `json:"args"`    // argv without the control dir
	Browser string   `json:"browser"` // AGENT_BROWSER_SESSION
}

// fakeHostBough is the host bough the relay runs: it records its call,
// then waits for the adapter's release: an exit code, or "kill".
func fakeHostBough(args []string) int {
	dir := strings.TrimPrefix(args[1], relayCtl)
	rec, _ := json.Marshal(hostCall{Args: append([]string{args[0]}, args[2:]...), Browser: os.Getenv("AGENT_BROWSER_SESSION")})
	if err := writeAtomic(filepath.Join(dir, "started"), rec); err != nil {
		fmt.Fprintln(os.Stderr, "stand-in host bough:", err)
		return 3
	}
	for deadline := time.Now().Add(2 * time.Minute); time.Now().Before(deadline); time.Sleep(2 * time.Millisecond) {
		b, err := os.ReadFile(filepath.Join(dir, "release"))
		if err != nil {
			continue
		}
		if string(b) == "kill" {
			// relayTimeout: CommandContext SIGKILLs the command, which
			// has written nothing.
			syscall.Kill(os.Getpid(), syscall.SIGKILL)
			select {}
		}
		code, _ := strconv.Atoi(string(b))
		os.Stdout.WriteString(relayHostOut)
		fmt.Fprintf(os.Stderr, "host: exit %d\n", code)
		return code
	}
	fmt.Fprintln(os.Stderr, "stand-in host bough: never released")
	return 3
}

func writeAtomic(path string, b []byte) error {
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, b, 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}

// guestCallMarker is $0 of a guest call's sh, which is how the runtime
// tells a call (whose BOUGH_HOST goes through the guest's network) from
// the adapter's own reads.
const guestCallMarker = "guest-call"

// guestCallScript is a program in the guest calling `bough`: it may set
// or drop the token and name another session, as any program there can,
// records the token the shim will send, then runs the shim off PATH.
const guestCallScript = `out=$1 mode=$2 tok=$3 sess=$4
shift 4
case $mode in
set) export BOUGH_ORB_TOKEN="$tok" ;;
unset) unset BOUGH_ORB_TOKEN ;;
esac
if [ -n "$sess" ]; then export BOUGH_SESSION="$sess"; fi
printf '%s' "${BOUGH_ORB_TOKEN:-}" > "$out"
exec bough "$@"
`

// relayRuntime is container.Fake as one guest's container and network.
type relayRuntime struct {
	*container.Fake
	mu       sync.Mutex
	gateway  string              // the guest's resolv.conf nameserver; "" for none
	fresh    bool                // resolv.conf was read with a gateway: a listener may have started
	env      map[string][]string // each container's create env
	lastExec []string            // the env Orb.Command passed on the last exec
	reach    func(port string) bool
	dead     string // a closed port: where the guest's unreachable BOUGH_HOST leads
	loop     string // the loopback the listeners are dialed on
	routes   map[string]net.Listener
}

func newRelayRuntime(dead, loop string, reach func(string) bool) *relayRuntime {
	return &relayRuntime{Fake: container.NewFake(), env: map[string][]string{}, dead: dead, loop: loop, reach: reach, routes: map[string]net.Listener{}}
}

// route is the guest's way to the listener on port: a forwarder on a
// port of its own. The listener binds the wildcard on its port, which
// does not stop another process binding 127.0.0.1 on the same one; a
// neighbour's serve answered the guest's calls with 404 that way.
func (r *relayRuntime) route(port string) (string, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if ln, ok := r.routes[port]; ok {
		_, p, _ := net.SplitHostPort(ln.Addr().String())
		return p, nil
	}
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return "", err
	}
	r.routes[port] = ln
	dst := net.JoinHostPort(r.loop, port)
	go func() {
		for {
			c, err := ln.Accept()
			if err != nil {
				return
			}
			go func() {
				defer c.Close()
				d, err := net.DialTimeout("tcp", dst, 5*time.Second)
				if err != nil {
					return
				}
				defer d.Close()
				done := make(chan struct{})
				go func() {
					io.Copy(d, c)
					d.(*net.TCPConn).CloseWrite()
					close(done)
				}()
				io.Copy(c, d)
				c.(*net.TCPConn).CloseWrite()
				<-done
			}()
		}
	}()
	_, p, _ := net.SplitHostPort(ln.Addr().String())
	return p, nil
}

// closeRoutes ends the walk's forwarders.
func (r *relayRuntime) closeRoutes() {
	r.mu.Lock()
	defer r.mu.Unlock()
	for _, ln := range r.routes {
		ln.Close()
	}
}

func (r *relayRuntime) setGateway(gw string) {
	r.mu.Lock()
	r.gateway = gw
	r.mu.Unlock()
}

// takeFresh says whether a listener may have started since the last call.
func (r *relayRuntime) takeFresh() bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	f := r.fresh
	r.fresh = false
	return f
}

func (r *relayRuntime) execEnv() []string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return slices.Clone(r.lastExec)
}

func (r *relayRuntime) Start(ctx context.Context, spec container.RunSpec) error {
	st, _ := r.Fake.Inspect(ctx, spec.Name)
	if err := r.Fake.Start(ctx, spec); err != nil {
		return err
	}
	if st == container.StateMissing {
		r.mu.Lock()
		r.env[spec.Name] = slices.Clone(spec.Env)
		r.mu.Unlock()
	}
	return nil
}

func (r *relayRuntime) Remove(ctx context.Context, name string) error {
	r.mu.Lock()
	delete(r.env, name)
	r.mu.Unlock()
	return r.Fake.Remove(ctx, name)
}

// hostEnvKeys are left out of the host env a guest exec starts from: a
// suite run inside an orb has them set, and a guest has only its own.
var hostEnvKeys = []string{"BOUGH_ORB_TOKEN", "BOUGH_HOST", "BOUGH_SESSION", "AGENT_BROWSER_SESSION"}

func (r *relayRuntime) Command(ctx context.Context, name string, opt container.ExecOptions, argv ...string) *exec.Cmd {
	if len(argv) == 3 && argv[0] == "sh" && strings.Contains(argv[2], "/etc/resolv.conf") {
		r.mu.Lock()
		gw := r.gateway
		r.fresh = r.fresh || gw != ""
		r.mu.Unlock()
		return r.Fake.Command(ctx, name, opt, "printf", "%s\n", gw)
	}
	r.mu.Lock()
	r.lastExec = slices.Clone(opt.Env)
	create := r.env[name]
	r.mu.Unlock()
	cmd := r.Fake.Command(ctx, name, opt, argv...)
	env := slices.DeleteFunc(os.Environ(), func(e string) bool {
		k, _, _ := strings.Cut(e, "=")
		return slices.Contains(hostEnvKeys, k)
	})
	// The container's env, then the exec's -e over it.
	env = append(append(append(env, create...), opt.Env...), opt.Secrets...)
	if len(argv) > 3 && argv[3] == guestCallMarker {
		for i, e := range env {
			v, ok := strings.CutPrefix(e, "BOUGH_HOST=")
			if !ok {
				continue
			}
			port := r.dead
			if u, err := url.Parse(v); err == nil && r.reach(u.Port()) {
				if p, err := r.route(u.Port()); err == nil {
					port = p
				}
			}
			env[i] = "BOUGH_HOST=http://127.0.0.1:" + port
		}
	}
	cmd.Env = env
	return cmd
}

// relayFields is the Orb role's state.
type relayFields struct {
	proc             bool
	ctr              string
	gen, ctok        int
	tokfile          bool
	otok             int
	proxy            string
	ptok             int
	res              string
	sent, at, gate   int
	ran, scope       string
	hostrc, rc       int
	explained, sized bool
}

func (f relayFields) state() map[string]any {
	return map[string]any{
		"proc": f.proc, "ctr": f.ctr, "gen": f.gen, "ctok": f.ctok, "tokfile": f.tokfile,
		"otok": f.otok, "proxy": f.proxy, "ptok": f.ptok, "res": f.res, "sent": f.sent,
		"at": f.at, "gate": f.gate, "ran": f.ran, "scope": f.scope, "hostrc": f.hostrc,
		"rc": f.rc, "explained": f.explained, "sized": f.sized,
	}
}

// relayCall is one run of the guest's shim.
type relayCall struct {
	dir      string
	at, gate int // the container's token and the listener's when it was made
	done     chan struct{}
	rc       int
	stdout   string
	stderr   string
	released string    // what the adapter told the host command: "0", "2", "kill"
	host     *hostCall // the host command it started, nil when refused
}

func (c *relayCall) finished() bool {
	select {
	case <-c.done:
		return true
	default:
		return false
	}
}

// hostStarted reads the stand-in's record once it is there.
func (c *relayCall) hostStarted() *hostCall {
	if c.host == nil {
		if b, err := os.ReadFile(filepath.Join(c.dir, "started")); err == nil {
			var h hostCall
			if json.Unmarshal(b, &h) == nil {
				c.host = &h
			}
		}
	}
	return c.host
}

// running is the call in flight: its host command started, the shim has
// not returned.
func (c *relayCall) running() bool { return c.hostStarted() != nil && !c.finished() }

// shimRefusal classifies a shim that returned without a host command
// behind it, by what it said.
func shimRefusal(stderr string) string {
	switch {
	case strings.Contains(stderr, "BOUGH_HOST unset"):
		return "unset"
	case strings.Contains(stderr, "cannot reach the host relay"):
		return "connect_fail"
	case strings.Contains(stderr, "missing or wrong orb token"):
		return "unauthorized"
	case strings.Contains(stderr, "too large for the host relay"), strings.Contains(stderr, "request body too large"), strings.Contains(stderr, "orb relay: arg:"), strings.Contains(stderr, "orb relay: stdin:"):
		return "bad_request"
	case strings.Contains(stderr, "only `bough mcp"):
		return "forbidden"
	}
	return ""
}

const (
	relayMaxGen       = 2
	relayGateway      = "0.0.0.0"
	relayOtherSession = "someone-elses-session"
	relayOtherProject = "someone-elses-project"
)

type relayAdapter struct {
	t    *testing.T
	root string
	home string
	p    projectdef.Project
	dead string
	loop string
	hist string
	gate gate
	n    int

	id      string
	rt      *relayRuntime
	scratch string
	o       *orb.Orb   // the session child's orb; nil while no child runs
	dropped []*orb.Orb // orbs no Stop closed (a crash, a failed Stop): closed at Cleanup
	stopped string     // the port SessionEnds' Stop closed, checked once

	order     []string // every token this walk made, in order: gen i is order[i-1]
	epoch     int      // the guest's gateway; a restart onto a new one bumps it
	addr      string   // o's listener's port, "" when o has none
	addrEpoch int      // the gateway that listener was started on
	otok      int      // o.token, as the last exec's env carried it
	calls     int
	call      *relayCall // the last call; nil once the model forgets it
	cur       *relayFields
	client    *http.Client

	pending string // the action GetState records next
	trace   []tracecheck.Step
	ids     []string

	// newGatewayKept is the deliberate bug the wrong-adapter test
	// injects: a restart onto a new gateway played as the same one.
	newGatewayKept bool
}

func newRelayAdapter(t *testing.T) *relayAdapter {
	// A short root under the system temp dir: the shim's paths and the
	// orb dirs are fine long, but keep it like servetest's.
	root, err := os.MkdirTemp("", "brel-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(root) })
	root, _ = filepath.EvalSymlinks(root)
	a := &relayAdapter{t: t, root: root, home: filepath.Join(root, "home")}
	a.hist = filepath.Join(a.home, ".bough", "history")
	if err := os.MkdirAll(a.hist, 0o755); err != nil {
		t.Fatal(err)
	}
	if a.p, err = projectdef.CreateEmpty(a.home, "relay", "Relay"); err != nil {
		t.Fatal(err)
	}
	// A port nothing listens on: the guest's route to a gateway it no
	// longer has.
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	_, a.dead, _ = net.SplitHostPort(ln.Addr().String())
	ln.Close()
	// The listeners are dialed on IPv6 loopback where there is one:
	// fewer neighbours bind ::1 on a port than 127.0.0.1.
	a.loop = "127.0.0.1"
	if ln, err := net.Listen("tcp", "[::1]:0"); err == nil {
		ln.Close()
		a.loop = "::1"
	}
	tr := &http.Transport{Proxy: nil, DisableKeepAlives: true}
	a.client = &http.Client{Transport: tr, Timeout: 10 * time.Second}
	t.Cleanup(tr.CloseIdleConnections)
	return a
}

// Init starts each walk on a fresh session with no container: a new
// runtime, so no container, create env or token survives the last walk.
func (a *relayAdapter) Init() error {
	a.n++
	a.id = fmt.Sprintf("r%04d", a.n)
	a.ids = append(a.ids, a.id)
	a.rt = newRelayRuntime(a.dead, a.loop, a.reachable)
	a.scratch = filepath.Join(a.root, "scratch", a.id)
	if err := os.MkdirAll(a.scratch, 0o755); err != nil {
		return err
	}
	a.o, a.dropped, a.stopped = nil, nil, ""
	a.order, a.epoch, a.addr, a.addrEpoch, a.otok = nil, 0, "", 0, 0
	a.call, a.cur = nil, nil
	a.gate.reset()
	a.trace, a.pending = nil, "Init"
	return nil
}

// Cleanup lets a call in flight finish and closes every listener the
// walk left, so none outlives it.
func (a *relayAdapter) Cleanup() error {
	var errs []error
	if c := a.call; c != nil && c.running() {
		errs = append(errs, a.release(c, "0"))
	}
	ctx, cancel := actionCtx()
	defer cancel()
	name := container.OrbName(a.id)
	for _, o := range append(a.dropped, a.o) {
		if o == nil {
			continue
		}
		// Stop fails on a missing container before it closes the
		// listener; the walk is over, so give it one to stop.
		if st, _ := a.rt.Inspect(ctx, name); st == container.StateMissing {
			a.rt.AddImage("cleanup")
			a.rt.Fake.Start(ctx, container.RunSpec{Name: name, Image: "cleanup"})
		}
		errs = append(errs, o.Stop(ctx))
	}
	a.o, a.dropped = nil, nil
	a.rt.closeRoutes()
	return errors.Join(errs...)
}

func (a *relayAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Orb", Index: 0}: a}, nil
}

// GetState reads the role off the product, and records the step taken
// with it: the walk's transcript is what the trace check replays.
func (a *relayAdapter) GetState() (map[string]any, error) {
	f, err := a.observe()
	if err != nil {
		return nil, err
	}
	a.cur = &f
	st := f.state()
	if a.pending != "" {
		q := map[string]any{}
		for k, v := range st {
			q["Orb#0."+k] = v
		}
		action := a.pending
		if action != "Init" {
			action = "Orb#0." + action
		}
		a.trace = append(a.trace, tracecheck.Step{Action: action, State: q})
		if err := a.record(a.pending); err != nil {
			return nil, err
		}
		a.pending = ""
	}
	return st, nil
}

// view is the state the next action's require reads.
func (a *relayAdapter) view() (relayFields, error) {
	if a.cur != nil {
		return *a.cur, nil
	}
	f, err := a.observe()
	if err != nil {
		return f, err
	}
	a.cur = &f
	return f, nil
}

// enabled is the gate over the spec's require, read off the product;
// once the gate is shut nothing is read.
func (a *relayAdapter) enabled(req func(relayFields) bool) (bool, error) {
	if a.gate.off {
		return false, nil
	}
	f, err := a.view()
	if err != nil {
		return false, err
	}
	return a.gate.pass(req(f)), nil
}

// acted marks the step taken: GetState records it and reads afresh.
func (a *relayAdapter) acted(name string) {
	a.pending, a.cur = name, nil
}

func (a *relayAdapter) num(tok string) int {
	if tok == "" {
		return 0
	}
	if i := slices.Index(a.order, tok); i >= 0 {
		return i + 1
	}
	a.order = append(a.order, tok)
	return len(a.order)
}

// reachable is the guest's network: only the listener on the gateway it
// has now answers.
func (a *relayAdapter) reachable(port string) bool {
	return port != "" && port == a.addr && a.addrEpoch == a.epoch
}

func envValue(env []string, key string) string {
	v := ""
	for _, e := range env {
		if s, ok := strings.CutPrefix(e, key+"="); ok {
			v = s
		}
	}
	return v
}

// observe reads the role's fields off the product.
func (a *relayAdapter) observe() (relayFields, error) {
	f := relayFields{res: "idle", sized: true}
	ctx, cancel := actionCtx()
	defer cancel()
	name := container.OrbName(a.id)
	cs, err := a.rt.Inspect(ctx, name)
	if err != nil {
		return f, err
	}
	f.ctr = string(cs)

	// Tokens are numbered as they appear: the create env of the last
	// container made, and the token file.
	f.ctok = a.num(envValue(a.rt.LastRun.Env, "BOUGH_ORB_TOKEN"))
	file := ""
	if b, err := os.ReadFile(filepath.Join(orb.Dir(a.home, a.id), "token")); err == nil {
		file = strings.TrimSpace(string(b))
	} else if !errors.Is(err, os.ErrNotExist) {
		return f, err
	}
	f.tokfile = file != "" && f.ctok != 0 && a.num(file) == f.ctok

	f.proc = a.o != nil
	f.proxy = "none"
	if a.o != nil {
		if cs == container.StateRunning {
			// What the child hands the runtime on an exec: o.token and
			// the listener's URL. Built, not run.
			a.o.Command(ctx, "true")
			env := a.rt.execEnv()
			tok := envValue(env, "BOUGH_ORB_TOKEN")
			if tok != "" && !slices.Contains(a.order, tok) {
				return f, fmt.Errorf("the exec env carries a token no container or file has: %q", tok)
			}
			a.otok = a.num(tok)
			addr := ""
			if h := envValue(env, "BOUGH_HOST"); h != "" {
				u, err := url.Parse(h)
				if err != nil || u.Port() == "" {
					return f, fmt.Errorf("BOUGH_HOST %q: %v", h, err)
				}
				addr = u.Port()
			}
			if a.rt.takeFresh() && addr != "" {
				a.addrEpoch = a.epoch
			} else if addr != a.addr && addr != "" {
				return f, fmt.Errorf("a listener on port %s appeared without a gateway lookup", addr)
			}
			a.addr = addr
		}
		f.otok = a.otok
		if auth := a.o.State().ProxyAuth; (auth == orb.ProxyAuthLegacy) != (f.otok == 0) {
			return f, fmt.Errorf("state.json says proxy auth %q while o.token is gen %d", auth, f.otok)
		}
		if a.addr != "" {
			f.proxy = "stale"
			if a.addrEpoch == a.epoch {
				f.proxy = "up"
			}
			if f.ptok, err = a.listenerToken(a.addr); err != nil {
				return f, err
			}
		}
	} else if a.stopped != "" {
		// Stop closed the listener: soon nothing may answer there. The
		// socket itself closes once the Accept blocked on it returns,
		// a moment after Stop does.
		f.proxy = "up"
		for deadline := time.Now().Add(2 * time.Second); time.Now().Before(deadline); time.Sleep(5 * time.Millisecond) {
			c, err := net.DialTimeout("tcp", net.JoinHostPort(a.loop, a.stopped), time.Second)
			if err != nil {
				f.proxy = "none"
				break
			}
			c.Close()
		}
		a.stopped = ""
	}
	f.gen = len(a.order)

	c := a.call
	if c == nil {
		return f, nil
	}
	h := c.hostStarted()
	if h == nil && !c.finished() {
		return f, errors.New("a call neither returned nor started its host command")
	}
	sent := 0
	if b, err := os.ReadFile(filepath.Join(c.dir, "guest")); err == nil && len(b) > 0 {
		sent = -1
		if i := slices.Index(a.order, string(b)); i >= 0 {
			sent = i + 1
		}
	}
	past := func() {
		f.sent, f.at, f.gate = sent, c.at, c.gate
	}
	switch {
	case h != nil && !c.finished():
		f.res, f.ran = "running", h.Args[0]
		f.scope = "own"
		if h.Browser != a.id || (h.Args[0] == "project" && slices.Contains(h.Args, relayOtherProject)) {
			f.scope = "other"
		}
		past()
	case h != nil:
		f.rc, f.explained = c.rc, c.stderr != ""
		switch c.released {
		case "kill":
			f.res, f.hostrc = "timed_out", -1
		case "0", "2":
			f.res, f.hostrc = "ok", map[string]int{"0": 0, "2": 2}[c.released]
			f.sized = c.stdout == relayHostOut
		default:
			return f, fmt.Errorf("the shim returned while its host command ran: exit %d, stderr %q", c.rc, c.stderr)
		}
	default:
		f.res, f.rc, f.explained = shimRefusal(c.stderr), c.rc, c.stderr != ""
		if f.res == "" {
			return f, fmt.Errorf("the shim failed in a way no refusal explains: exit %d, stderr %q", c.rc, c.stderr)
		}
		if f.res == "bad_request" || f.res == "forbidden" {
			past()
		}
	}
	return f, nil
}

// listenerToken asks the listener on port which token it checks: a call
// it lets past the token answers 403 (the command is refused), one it
// does not answers 401. 0 is a listener that checks none.
func (a *relayAdapter) listenerToken(port string) (int, error) {
	// A refused subcommand, carrying relayCtl so a relay that ran it
	// would run the stand-in (which fails on the missing dir), never
	// this binary's tests.
	body := base64.StdEncoding.EncodeToString([]byte("serve")) + "\n" +
		base64.StdEncoding.EncodeToString([]byte(relayCtl+filepath.Join(a.root, "no-such-dir"))) + "\n\n"
	try := func(tok string) (int, error) {
		req, err := http.NewRequest(http.MethodPost, "http://"+net.JoinHostPort(a.loop, port)+"/bough/exec", strings.NewReader(body))
		if err != nil {
			return 0, err
		}
		if tok != "" {
			req.Header.Set("Authorization", "Bearer "+tok)
		}
		resp, err := a.client.Do(req)
		if err != nil {
			return 0, fmt.Errorf("the listener on %s: %w", port, err)
		}
		resp.Body.Close()
		if resp.StatusCode != http.StatusUnauthorized && resp.StatusCode != http.StatusForbidden {
			return 0, fmt.Errorf("the listener on %s answered a refused command %d", port, resp.StatusCode)
		}
		return resp.StatusCode, nil
	}
	code, err := try("")
	if err != nil || code != http.StatusUnauthorized {
		return 0, err
	}
	for i, tok := range a.order {
		code, err := try(tok)
		if err != nil {
			return 0, err
		}
		if code != http.StatusUnauthorized {
			return i + 1, nil
		}
	}
	return 0, fmt.Errorf("the listener on %s lets in no token this walk made", port)
}

// settle is the model's: a lifecycle step forgets a call that is over.
func (a *relayAdapter) settle() {
	if a.call != nil && !a.call.running() {
		a.call = nil
	}
}

// --- the orb's life ---

func (a *relayAdapter) PreTokenContainer() error {
	if ok, err := a.enabled(func(f relayFields) bool { return f.gen == 0 && f.ctr == "missing" && !f.proc }); !ok {
		return err
	}
	a.acted("PreTokenContainer")
	a.settle()
	ctx, cancel := actionCtx()
	defer cancel()
	tag, err := orb.EnsureImage(ctx, a.rt, a.home, a.p, nil)
	if err != nil {
		return err
	}
	name := container.OrbName(a.id)
	if err := a.rt.Start(ctx, container.RunSpec{Name: name, Image: tag}); err != nil {
		return err
	}
	if err := a.rt.Stop(ctx, name); err != nil {
		return err
	}
	// The state the older bough left: it names this image, so Open
	// reuses the container rather than replacing an unproven one.
	b, _ := json.Marshal(orb.State{Session: a.id, Project: a.p.Slug, Container: name, Image: tag, Status: orb.StatusStopped})
	if err := os.MkdirAll(orb.Dir(a.home, a.id), 0o755); err != nil {
		return err
	}
	return os.WriteFile(filepath.Join(orb.Dir(a.home, a.id), "state.json"), b, 0o644)
}

func (a *relayAdapter) Open() error          { return a.open("Open", relayGateway) }
func (a *relayAdapter) OpenNoGateway() error { return a.open("OpenNoGateway", "") }

func (a *relayAdapter) open(name, gw string) error {
	if ok, err := a.enabled(func(f relayFields) bool { return !f.proc && (f.ctr != "missing" || f.gen < relayMaxGen) }); !ok {
		return err
	}
	a.acted(name)
	a.settle()
	a.rt.setGateway(gw)
	ctx, cancel := actionCtx()
	defer cancel()
	o, err := orb.Open(ctx, a.rt, a.home, a.id, a.p, a.scratch)
	if err != nil {
		return err
	}
	a.o, a.otok, a.addr, a.stopped = o, 0, "", ""
	return nil
}

func (a *relayAdapter) SessionEnds() error {
	if ok, err := a.enabled(func(f relayFields) bool { return f.proc }); !ok {
		return err
	}
	a.acted("SessionEnds")
	a.settle()
	ctx, cancel := actionCtx()
	defer cancel()
	o := a.o
	a.o, a.otok = nil, 0
	if err := o.Stop(ctx); err != nil {
		// The runtime has no container to stop: the child exits all
		// the same, and its listener with it. In this process it stays
		// open until Cleanup.
		a.dropped = append(a.dropped, o)
	} else {
		a.stopped = a.addr
	}
	a.addr = ""
	return nil
}

func (a *relayAdapter) ChildCrash() error {
	if ok, err := a.enabled(func(f relayFields) bool { return f.proc }); !ok {
		return err
	}
	a.acted("ChildCrash")
	a.settle()
	a.dropped = append(a.dropped, a.o)
	a.o, a.otok, a.addr = nil, 0, ""
	return nil
}

func (a *relayAdapter) ExternalStop() error {
	if ok, err := a.enabled(func(f relayFields) bool { return f.ctr == "running" }); !ok {
		return err
	}
	a.acted("ExternalStop")
	a.settle()
	ctx, cancel := actionCtx()
	defer cancel()
	return a.rt.Stop(ctx, container.OrbName(a.id))
}

func (a *relayAdapter) ContainerRemoved() error {
	if ok, err := a.enabled(func(f relayFields) bool { return f.ctr != "missing" }); !ok {
		return err
	}
	a.acted("ContainerRemoved")
	a.settle()
	ctx, cancel := actionCtx()
	defer cancel()
	return a.rt.Remove(ctx, container.OrbName(a.id))
}

func (a *relayAdapter) TokenFileLost() error {
	if ok, err := a.enabled(func(f relayFields) bool { return f.tokfile }); !ok {
		return err
	}
	a.acted("TokenFileLost")
	a.settle()
	return os.Remove(filepath.Join(orb.Dir(a.home, a.id), "token"))
}

func (a *relayAdapter) RestartSameGateway() error { return a.restart("RestartSameGateway", "same") }
func (a *relayAdapter) RestartNewGateway() error  { return a.restart("RestartNewGateway", "new") }
func (a *relayAdapter) RestartNoGateway() error   { return a.restart("RestartNoGateway", "none") }

// restart is the child's next exec finding the container down: Orb.Command
// starts it again (ensureRunningLocked) before it runs anything.
func (a *relayAdapter) restart(name, gw string) error {
	if ok, err := a.enabled(func(f relayFields) bool {
		return f.proc && f.ctr != "running" && (f.ctr != "missing" || f.gen < relayMaxGen)
	}); !ok {
		return err
	}
	a.acted(name)
	a.settle()
	switch gw {
	case "new":
		if !a.newGatewayKept {
			a.epoch++
		}
		a.rt.setGateway(relayGateway)
	case "same":
		a.rt.setGateway(relayGateway)
	default:
		a.rt.setGateway("")
	}
	ctx, cancel := actionCtx()
	defer cancel()
	cmd := a.o.Command(ctx, "true")
	if cmd.Err != nil {
		return cmd.Err
	}
	return cmd.Run()
}

// --- the guest's shim ---

func (a *relayAdapter) callable(f relayFields) bool {
	return f.proc && f.ctr == "running" && f.res != "running"
}

func (a *relayAdapter) CallMcp() error {
	return a.guestCall("CallMcp", nil, "own", "", "", nil, "mcp", "list")
}

func (a *relayAdapter) CallOther() error {
	return a.guestCall("CallOther", nil, "own", "", "", nil, "serve")
}

func (a *relayAdapter) CallProjectOther() error {
	return a.guestCall("CallProjectOther", nil, "own", "", "", nil, "project", "rm", relayOtherProject, "--yes")
}

func (a *relayAdapter) CallBrowserOtherSession() error {
	return a.guestCall("CallBrowserOtherSession", nil, "own", "", relayOtherSession, nil, "browser", "snapshot")
}

func (a *relayAdapter) CallWrongToken() error {
	return a.guestCall("CallWrongToken", nil, "set", "0123456789abcdef-not-this-orbs", "", nil, "mcp", "list")
}

func (a *relayAdapter) CallNoToken() error {
	return a.guestCall("CallNoToken", nil, "unset", "", "", nil, "mcp", "list")
}

func (a *relayAdapter) CallOldToken() error {
	if a.gate.off {
		return nil
	}
	f, err := a.view()
	if err != nil {
		return err
	}
	old := ""
	if f.gen >= 2 {
		old = a.order[f.gen-2]
	}
	return a.guestCall("CallOldToken", func(f relayFields) bool { return f.gen >= 2 }, "set", old, "", nil, "mcp", "list")
}

// CallTooLarge pipes more than the relay's 8 MiB, base64'd, into the shim.
func (a *relayAdapter) CallTooLarge() error {
	return a.guestCall("CallTooLarge", nil, "own", "", "", bytes.Repeat([]byte("x"), 6<<20+1024), "mcp", "call")
}

// guestCall runs the shim in the guest and returns once it has either
// returned or started its host command.
func (a *relayAdapter) guestCall(name string, also func(relayFields) bool, mode, tok, session string, stdin []byte, args ...string) error {
	if ok, err := a.enabled(func(f relayFields) bool { return a.callable(f) && (also == nil || also(f)) }); !ok {
		return err
	}
	f := *a.cur
	a.acted(name)
	a.calls++
	c := &relayCall{dir: filepath.Join(a.root, "calls", fmt.Sprintf("c%06d", a.calls)), at: f.ctok, gate: f.ptok, done: make(chan struct{})}
	a.call = c
	if err := os.MkdirAll(c.dir, 0o755); err != nil {
		return err
	}
	argv := append([]string{"sh", "-c", guestCallScript, guestCallMarker, filepath.Join(c.dir, "guest"), mode, tok, session, args[0], relayCtl + c.dir}, args[1:]...)
	cmd := a.o.Command(context.Background(), argv...)
	if cmd.Err != nil {
		return cmd.Err
	}
	var out, errb bytes.Buffer
	cmd.Stdout, cmd.Stderr = &out, &errb
	if stdin != nil {
		cmd.Stdin = bytes.NewReader(stdin)
	}
	if err := cmd.Start(); err != nil {
		return err
	}
	go func() {
		cmd.Wait()
		c.rc, c.stdout, c.stderr = cmd.ProcessState.ExitCode(), out.String(), errb.String()
		close(c.done)
	}()
	return waitUntil("the call to return or start its host command", func() bool {
		return c.finished() || c.hostStarted() != nil
	})
}

// --- the host command ---

func (a *relayAdapter) HostExitsOk() error    { return a.hostExits("HostExitsOk", "0") }
func (a *relayAdapter) HostExitsError() error { return a.hostExits("HostExitsError", "2") }
func (a *relayAdapter) HostTimeout() error    { return a.hostExits("HostTimeout", "kill") }

func (a *relayAdapter) hostExits(name, how string) error {
	if ok, err := a.enabled(func(f relayFields) bool { return f.res == "running" }); !ok {
		return err
	}
	a.acted(name)
	return a.release(a.call, how)
}

// release ends the host command as how says and waits for the shim.
func (a *relayAdapter) release(c *relayCall, how string) error {
	c.released = how
	if err := writeAtomic(filepath.Join(c.dir, "release"), []byte(how)); err != nil {
		return err
	}
	select {
	case <-c.done:
		return nil
	case <-time.After(actionTimeout):
		return errors.New("the shim did not return after its host command ended")
	}
}

var relayActions = map[string]map[string]fmbt.ActionFunc{"Orb": {
	"PreTokenContainer":       action((*relayAdapter).PreTokenContainer),
	"Open":                    action((*relayAdapter).Open),
	"OpenNoGateway":           action((*relayAdapter).OpenNoGateway),
	"SessionEnds":             action((*relayAdapter).SessionEnds),
	"ChildCrash":              action((*relayAdapter).ChildCrash),
	"ExternalStop":            action((*relayAdapter).ExternalStop),
	"ContainerRemoved":        action((*relayAdapter).ContainerRemoved),
	"TokenFileLost":           action((*relayAdapter).TokenFileLost),
	"RestartSameGateway":      action((*relayAdapter).RestartSameGateway),
	"RestartNewGateway":       action((*relayAdapter).RestartNewGateway),
	"RestartNoGateway":        action((*relayAdapter).RestartNoGateway),
	"CallMcp":                 action((*relayAdapter).CallMcp),
	"CallOther":               action((*relayAdapter).CallOther),
	"CallProjectOther":        action((*relayAdapter).CallProjectOther),
	"CallBrowserOtherSession": action((*relayAdapter).CallBrowserOtherSession),
	"CallWrongToken":          action((*relayAdapter).CallWrongToken),
	"CallNoToken":             action((*relayAdapter).CallNoToken),
	"CallOldToken":            action((*relayAdapter).CallOldToken),
	"CallTooLarge":            action((*relayAdapter).CallTooLarge),
	"HostExitsOk":             action((*relayAdapter).HostExitsOk),
	"HostExitsError":          action((*relayAdapter).HostExitsError),
	"HostTimeout":             action((*relayAdapter).HostTimeout),
}, "": {
	// fizz links a state with nothing enabled to itself as "end": no
	// child, no container and both tokens used (MAX_GEN). Nothing happens.
	"end": func(any, []fmbt.Arg) (any, error) { return nil, nil },
}}

func relayOptions() map[string]any {
	return map[string]any{"max-seq-runs": 200, "max-actions": 12, "max-parallel-runs": 0}
}

// record appends the step to the walk's transcript: what the guest saw
// of it (a call's exit code, stderr and stdout), in history's format.
func (a *relayAdapter) record(action string) error {
	data := map[string]any{"action": action}
	if c := a.call; c != nil && (strings.HasPrefix(action, "Call") || strings.HasPrefix(action, "Host")) {
		data["host"] = c.hostStarted() != nil
		data["returned"] = c.finished()
		if c.finished() {
			data["exit"], data["stderr"], data["stdout"] = c.rc, c.stderr, len(c.stdout)
		}
	}
	path := filepath.Join(a.hist, a.id+".jsonl")
	entries, _ := history.Read(path)
	b, _ := json.Marshal(history.Entry{Seq: int64(len(entries) + 1), At: time.Now(), Kind: "relay", Data: data})
	fh, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return err
	}
	defer fh.Close()
	_, err = fh.Write(append(b, '\n'))
	return err
}

// orbRelayProxyAuthHistory reads a walk's transcript: every step, and
// of each call only what the guest saw, so the check is on the shim's
// half of the state (res, rc, explained, sized), read off its own words.
func orbRelayProxyAuthHistory(entries []history.Entry) []tracecheck.Step {
	st := func(res string, rc int, explained, sized bool) map[string]any {
		return map[string]any{"Orb#0.res": res, "Orb#0.rc": rc, "Orb#0.explained": explained, "Orb#0.sized": sized}
	}
	idle := st("idle", 0, false, true)
	steps := []tracecheck.Step{{Action: "Init", State: idle}}
	inflight := false
	last := idle
	for _, e := range entries {
		if e.Kind != "relay" {
			continue
		}
		action, _ := e.Data["action"].(string)
		if action == "" || action == "Init" {
			continue
		}
		host, _ := e.Data["host"].(bool)
		returned, _ := e.Data["returned"].(bool)
		exit := 0
		if n, ok := e.Data["exit"].(float64); ok {
			exit = int(n)
		}
		stderr, _ := e.Data["stderr"].(string)
		stdout := 0
		if n, ok := e.Data["stdout"].(float64); ok {
			stdout = int(n)
		}
		var s map[string]any
		switch {
		case strings.HasPrefix(action, "Call") && host && !returned:
			inflight = true
			s = st("running", 0, false, true)
		case strings.HasPrefix(action, "Call"):
			s = st(shimRefusal(stderr), exit, stderr != "", true)
		case action == "HostTimeout":
			inflight = false
			s = st("timed_out", exit, stderr != "", true)
		case strings.HasPrefix(action, "Host"):
			inflight = false
			s = st("ok", exit, stderr != "", stdout == len(relayHostOut))
		case inflight:
			s = last
		default:
			s = idle
		}
		last = s
		steps = append(steps, tracecheck.Step{Action: "Orb#0." + action, State: s})
	}
	return steps
}

func init() { historyProjections["orb_relay_proxy_auth"] = orbRelayProxyAuthHistory }

// checkRelayHistories replays every walk's transcript on the graph.
func checkRelayHistories(t *testing.T, adapters ...*relayAdapter) {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("orb_relay_proxy_auth")), "..", "testdata", "orb_relay_proxy_auth"))
	if err != nil {
		t.Fatal(err)
	}
	walks, calls := 0, 0
	for _, a := range adapters {
		for _, id := range a.ids {
			entries := sessionHistory(t, a.home, id)
			for _, e := range entries {
				if s, _ := e.Data["action"].(string); strings.HasPrefix(s, "Call") {
					calls++
				}
			}
			checkHistory(t, g, entries, orbRelayProxyAuthHistory)
			walks++
		}
	}
	t.Logf("%d walks, %d relayed calls replayed", walks, calls)
}

func TestOrbRelayProxyAuth(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newRelayAdapter(t)
	if err := runMBT(t, "orb_relay_proxy_auth", a, relayActions, relayOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkRelayHistories(t, a)
}

// The runner's uniform picks over 22 actions rarely get a call through
// a restarted orb, so the adapter also walks the checked-in graph: every
// state (MODEL_COVER=transitions: every link), each action enabled in
// the adapter's view where the graph enables it, the state after it the
// graph's.
func TestOrbRelayProxyAuthPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	adapters, err := walkRelayPaths(t, envCover(), nil)
	if err != nil {
		t.Fatal(err)
	}
	checkRelayHistories(t, adapters...)
}

// relayWorkers is how many adapters walk at once. A walk is mostly
// waiting (a Stop with a call in flight waits out the listener's 1 s
// shutdown), so a few in parallel cost little CPU.
const relayWorkers = 4

// walkRelayPaths drives the generated walks through relayWorkers
// adapters (each its own root, runtime and sessions; setup, when set,
// changes each) and returns the first step whose action an adapter did
// not enable or whose state is not the walk's.
func walkRelayPaths(t *testing.T, cover tracecheck.Cover, setup func(*relayAdapter)) ([]*relayAdapter, error) {
	t.Helper()
	b, err := pathsJSONCover("orb_relay_proxy_auth", cover)
	if err != nil {
		return nil, err
	}
	var file struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &file); err != nil {
		return nil, err
	}
	var (
		mu       sync.Mutex
		taken    = map[string]int{}
		firstErr error
		wg       sync.WaitGroup
	)
	adapters := make([]*relayAdapter, relayWorkers)
	for w := range adapters {
		a := newRelayAdapter(t)
		if setup != nil {
			setup(a)
		}
		adapters[w] = a
		wg.Add(1)
		go func() {
			defer wg.Done()
			for pi := w; pi < len(file.Paths); pi += relayWorkers {
				mu.Lock()
				stop := firstErr != nil
				mu.Unlock()
				if stop {
					return
				}
				counts, err := a.walk(pi, file.Paths[pi].Trace)
				mu.Lock()
				for k, v := range counts {
					taken[k] += v
				}
				if err != nil && firstErr == nil {
					firstErr = err
				}
				mu.Unlock()
				if err != nil {
					return
				}
			}
		}()
	}
	wg.Wait()
	if firstErr != nil {
		return adapters, firstErr
	}
	t.Logf("%d walks, actions taken: %v", len(file.Paths), taken)
	for name := range relayActions["Orb"] {
		if taken[name] == 0 {
			return adapters, fmt.Errorf("no walk took %s", name)
		}
	}
	return adapters, nil
}

// walk drives one generated walk and checks every step's state.
func (a *relayAdapter) walk(pi int, trace []tracecheck.Step) (map[string]int, error) {
	taken := map[string]int{}
	defer a.Cleanup()
	for si, step := range trace {
		if si == 0 {
			if err := a.Init(); err != nil {
				return taken, fmt.Errorf("path %d init: %w", pi, err)
			}
		} else {
			name, role := strings.TrimPrefix(step.Action, "Orb#0."), "Orb"
			if name == step.Action {
				role = ""
			}
			f, ok := relayActions[role][name]
			if !ok {
				return taken, fmt.Errorf("path %d step %d: no adapter action for %q", pi, si, step.Action)
			}
			if _, err := f(a, nil); err != nil {
				return taken, fmt.Errorf("path %d step %d (%s): %w", pi, si, name, err)
			}
			if a.gate.off {
				return taken, fmt.Errorf("path %d step %d: %s is enabled in the model but not in the adapter's view", pi, si, name)
			}
			taken[name]++
		}
		got, err := a.GetState()
		if err != nil {
			return taken, fmt.Errorf("path %d step %d (%s): %w", pi, si, step.Action, err)
		}
		for k, v := range step.State {
			field, ok := strings.CutPrefix(k, "Orb#0.")
			if !ok {
				continue
			}
			want, _ := json.Marshal(v)
			have, _ := json.Marshal(got[field])
			if !bytes.Equal(want, have) {
				return taken, fmt.Errorf("path %d step %d (%s): %s is %s, the model says %s\nstate: %v", pi, si, step.Action, field, have, want, got)
			}
		}
	}
	return taken, nil
}

// A green walk proves nothing unless an adapter that breaks the model
// fails it. This one plays a restart onto a new gateway as the same
// gateway, so a listener the guest can no longer reach reads as up: it
// shows only on the RestartNewGateway links out of a state with a
// listener, hence every link.
func TestOrbRelayProxyAuthPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	_, err := walkRelayPaths(t, tracecheck.CoverTransitions, func(a *relayAdapter) { a.newGatewayKept = true })
	if err == nil {
		t.Fatal("an adapter that keeps the gateway on RestartNewGateway walked every link; the walk is not checking state")
	}
	if !strings.Contains(err.Error(), "RestartNewGateway") {
		t.Fatalf("caught, but not where the bug is: %v", err)
	}
	t.Logf("caught: %v", err)
}
