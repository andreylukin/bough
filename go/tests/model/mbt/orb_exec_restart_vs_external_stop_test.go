//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"maps"
	"net"
	"net/http"
	"net/http/httptest"
	"net/rpc"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"
	_ "unsafe" // go:linkname, for the child's host helpers

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/orb_exec_restart_vs_external_stop.fizz against the real code on
// both sides of state.json: the child's exec seam (orb.Orb.Command ->
// ensureRunningLocked) and the stops made outside it (serve's Stop orb,
// the reaper, Kill, and orb.StopContainer as `bough project stop` calls
// it), with what serve shows of the result.
//
// serve runs in process (Supervisor + API behind httptest), as in
// orb_lifecycle, on a runtime that is container.Fake with holds: a stop's
// rt.Stop and the child's rt.Start wait there for the walk to say how
// they end, which is how the races the spec names become states.
//
// The child is a real process serve spawns: this test binary, re-run in
// child mode (oxsChildMain). It opens the session's orb with orb.Open
// and runs orb.Command when told to, so state.json is written by the
// product's own code with the child's own pid, and serve's owner check
// is the real one. Its runtime is the parent's, over net/rpc: one
// container state, two processes, as with a real engine. resume.sh is a
// host script that waits for the walk to say how it exits.
//
// `bough project stop` is orb.StopContainer after its confirm; the CLI
// process itself would open the host's runtime, so the walk calls
// StopContainer on the held one instead.

const (
	oxsSpec     = "orb_exec_restart_vs_external_stop"
	oxsChildEnv = "MODEL_OXS_CHILD" // the parent's rpc address: this process is the child
	oxsSlug     = "oxs"
)

// oxsFields is the Orb role's state.
type oxsFields struct {
	owner                                    bool
	file, phase, writer, ctr, step, stop, by string
	pfile, pphase                            string
	pmark, mark, ctrok                       bool
}

func (o oxsFields) state() map[string]any {
	return map[string]any{
		"owner": o.owner, "file": o.file, "phase": o.phase, "writer": o.writer, "ctr": o.ctr,
		"step": o.step, "stop": o.stop, "by": o.by, "pfile": o.pfile, "pphase": o.pphase,
		"pmark": o.pmark, "mark": o.mark, "ctrok": o.ctrok,
	}
}

// status and up are the spec's derived views, line for line.
func (o oxsFields) status() string {
	if o.file == "failed" || o.file == "stopped" {
		return o.file
	}
	if !o.owner || o.mark {
		return "stopped"
	}
	return o.file
}

func (o oxsFields) up() bool {
	r := o.ctr == "running"
	switch o.file {
	case "failed":
		return r
	case "running", "starting":
	default:
		return false
	}
	u := o.file == "running" || r
	if !o.owner {
		return u && r
	}
	return u && !o.mark
}

// oxsPhase maps state.json's phase onto the spec's: the steps of a
// restart, and nothing once ready.
func oxsPhase(p string) string {
	switch p {
	case orb.PhaseContainer:
		return "container"
	case orb.PhaseResume:
		return "resume"
	}
	return ""
}

// --- the runtime ---

// oxsHold is one runtime call waiting for the walk's decision.
type oxsHold struct {
	stage string      // stop: "marked", then "done" for serve's; start: "starting"
	two   bool        // serve's stop: held again once rt.Stop's work is done
	ch    chan string // the decision
}

// oxsRuntime is container.Fake with holds on rt.Stop (armed per stop)
// and rt.Start (every start of the walk's container after its first).
type oxsRuntime struct {
	*container.Fake
	mu        sync.Mutex
	armed     map[string]*oxsHold // the next Stop of the name holds
	stops     map[string]*oxsHold
	holdStart map[string]bool
	starts    map[string]*oxsHold
	created   map[string]string // name -> the token it was created with
	// inspectHold: the next Inspect of the name is StopContainer's, after
	// its held stop found the container missing.
	inspectHold map[string]*oxsHold
}

func newOxsRuntime() *oxsRuntime {
	return &oxsRuntime{Fake: container.NewFake(), armed: map[string]*oxsHold{}, stops: map[string]*oxsHold{},
		holdStart: map[string]bool{}, starts: map[string]*oxsHold{}, created: map[string]string{}, inspectHold: map[string]*oxsHold{}}
}

var errOxsStopRefused = errors.New("model: the runtime refused the stop")

func (r *oxsRuntime) Stop(ctx context.Context, name string) error {
	r.mu.Lock()
	h := r.armed[name]
	delete(r.armed, name)
	if h != nil {
		h.stage = "marked"
		r.stops[name] = h
	}
	r.mu.Unlock()
	if h == nil {
		return r.Fake.Stop(ctx, name)
	}
	var err error
	switch <-h.ch {
	case "ok":
		err = r.Fake.Stop(ctx, name)
		if h.two && err != nil {
			// A missing container errs, and StopContainer's Inspect calls
			// it stopped: the spec's RuntimeStopOk is both, so serve's
			// second hold is in that Inspect (inspectHold), not here.
			r.mu.Lock()
			r.inspectHold[name] = h
			r.mu.Unlock()
			return err
		}
		if h.two {
			r.done(h)
		}
	case "down":
		r.Fake.Stop(ctx, name)
		err = fmt.Errorf("model: stop timed out: %w", errOxsStopRefused)
	default:
		err = errOxsStopRefused
	}
	r.mu.Lock()
	delete(r.stops, name)
	r.mu.Unlock()
	return err
}

// done holds serve's stop after rt.Stop's work: StopContainer has its
// answer and serve has not recorded stoppedAt.
func (r *oxsRuntime) done(h *oxsHold) {
	r.mu.Lock()
	h.stage = "done"
	r.mu.Unlock()
	<-h.ch
}

// Inspect answers StopContainer's question about a container its held
// stop found missing, then holds as done (see Stop).
func (r *oxsRuntime) Inspect(ctx context.Context, name string) (container.State, error) {
	r.mu.Lock()
	h := r.inspectHold[name]
	delete(r.inspectHold, name)
	r.mu.Unlock()
	st, err := r.Fake.Inspect(ctx, name)
	if h != nil {
		r.done(h)
		r.mu.Lock()
		delete(r.stops, name)
		r.mu.Unlock()
	}
	return st, err
}

func (r *oxsRuntime) Start(ctx context.Context, spec container.RunSpec) error {
	r.mu.Lock()
	var h *oxsHold
	if r.holdStart[spec.Name] {
		h = &oxsHold{stage: "starting", ch: make(chan string, 1)}
		r.starts[spec.Name] = h
	}
	r.mu.Unlock()
	if h != nil {
		d := <-h.ch
		r.mu.Lock()
		delete(r.starts, spec.Name)
		r.mu.Unlock()
		if d != "ok" {
			return errors.New("model: the runtime could not start the container")
		}
	}
	before, _ := r.Fake.Inspect(ctx, spec.Name)
	if err := r.Fake.Start(ctx, spec); err != nil {
		return err
	}
	if before == container.StateMissing {
		tok := ""
		for _, e := range spec.Env {
			if v, ok := strings.CutPrefix(e, "BOUGH_ORB_TOKEN="); ok {
				tok = v
			}
		}
		r.mu.Lock()
		r.created[spec.Name] = tok
		r.mu.Unlock()
	}
	return nil
}

func (r *oxsRuntime) stopHold(name string) *oxsHold {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.stops[name]
}

func (r *oxsRuntime) stopStage(name string) string {
	r.mu.Lock()
	defer r.mu.Unlock()
	if h := r.stops[name]; h != nil {
		return h.stage
	}
	return ""
}

func (r *oxsRuntime) startHold(name string) *oxsHold {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.starts[name]
}

// oxsServer is the runtime and the walk's control, served to the child
// as "OXS". net/rpc needs exported methods whose arguments are exported
// or builtin types.
type oxsServer struct{ a *oxsAdapter }

func (s *oxsServer) Inspect(name string, st *container.State) (err error) {
	*st, err = s.a.rt.Inspect(context.Background(), name)
	return err
}

func (s *oxsServer) Start(spec container.RunSpec, _ *bool) error {
	return s.a.rt.Start(context.Background(), spec)
}

func (s *oxsServer) Stop(name string, _ *bool) error { return s.a.rt.Stop(context.Background(), name) }

func (s *oxsServer) Remove(name string, _ *bool) error {
	return s.a.rt.Remove(context.Background(), name)
}

func (s *oxsServer) ImageExists(tag string, ok *bool) (err error) {
	*ok, err = s.a.rt.ImageExists(context.Background(), tag)
	return err
}

func (s *oxsServer) Running(_ int, names *[]string) (err error) {
	*names, err = s.a.rt.Running(context.Background())
	return err
}

func (s *oxsServer) Address(name string, addr *string) (err error) {
	*addr, err = s.a.rt.Address(context.Background(), name)
	return err
}

// Ready is the child's orb.Open returning: "<session>\t<error>".
func (s *oxsServer) Ready(msg string, _ *bool) error {
	s.a.child(msg).ready <- msg[strings.IndexByte(msg, '\t')+1:]
	return nil
}

// Next blocks until the walk has a command for the child's session.
func (s *oxsServer) Next(session string, cmd *string) error {
	c := s.a.child(session)
	select {
	case *cmd = <-c.cmds:
		return nil
	case <-c.gone:
		return errors.New("model: the walk is over")
	}
}

// Done is the child's exec returning: "<session>\t<error>".
func (s *oxsServer) Done(msg string, _ *bool) error {
	s.a.child(msg).done <- msg[strings.IndexByte(msg, '\t')+1:]
	return nil
}

// --- the child process ---

//go:linkname oxsHostShellEnv github.com/andreylukin/bough/internal/orb.hostShellEnv
var oxsHostShellEnv func() map[string]string

//go:linkname oxsHostCommand github.com/andreylukin/bough/internal/orb.hostCommand
var oxsHostCommand func(string, ...string) string

func init() {
	if addr := os.Getenv(oxsChildEnv); addr != "" {
		os.Exit(oxsChildMain(addr))
	}
}

// oxsChildMain is the session's child: it opens the orb and runs one
// exec per command, reporting each back.
func oxsChildMain(addr string) int {
	// orb reads the user's ~/.zshrc and host CLIs by the account's real
	// home, not $HOME: never here (the orb package's tests stub the same).
	oxsHostShellEnv = func() map[string]string { return nil }
	oxsHostCommand = func(string, ...string) string { return "" }
	id := os.Getenv("BOUGH_SESSION_ID")
	for i, a := range os.Args {
		if a == "-r" && i+1 < len(os.Args) {
			id = os.Args[i+1]
		}
	}
	c, err := rpc.Dial("tcp", addr)
	if err != nil {
		fmt.Fprintln(os.Stderr, "oxs child:", err)
		return 1
	}
	// serve closes stdin to end a child.
	go func() { io.Copy(io.Discard, os.Stdin); os.Exit(0) }()
	home := os.Getenv("HOME")
	ctx := context.Background()
	var o *orb.Orb
	p, err := projectdef.Load(home, oxsSlug)
	if err == nil {
		o, err = orb.Open(ctx, &oxsRemote{c: c}, home, id, p, "")
	}
	if err := c.Call("OXS.Ready", id+"\t"+errText(err), new(bool)); err != nil || o == nil {
		return 1
	}
	for {
		var cmd string
		if err := c.Call("OXS.Next", id, &cmd); err != nil {
			return 0
		}
		err := o.Command(ctx, "true").Run()
		if err := c.Call("OXS.Done", id+"\t"+errText(err), new(bool)); err != nil {
			return 0
		}
	}
}

func errText(err error) string {
	if err == nil {
		return ""
	}
	return err.Error()
}

// oxsRemote is the child's view of the parent's runtime.
type oxsRemote struct{ c *rpc.Client }

func (r *oxsRemote) Name() string                    { return "fake" }
func (r *oxsRemote) Available(context.Context) error { return nil }
func (r *oxsRemote) Build(context.Context, container.BuildSpec, io.Writer) error {
	return errors.New("model: the parent builds the image")
}
func (r *oxsRemote) Commit(context.Context, container.CommitSpec, io.Writer) error {
	return errors.New("model: the parent builds the image")
}
func (r *oxsRemote) ImageExists(_ context.Context, tag string) (ok bool, err error) {
	err = r.c.Call("OXS.ImageExists", tag, &ok)
	return ok, err
}
func (r *oxsRemote) Start(_ context.Context, spec container.RunSpec) error {
	return r.c.Call("OXS.Start", spec, new(bool))
}
func (r *oxsRemote) Stop(_ context.Context, name string) error {
	return r.c.Call("OXS.Stop", name, new(bool))
}
func (r *oxsRemote) Remove(_ context.Context, name string) error {
	return r.c.Call("OXS.Remove", name, new(bool))
}
func (r *oxsRemote) Inspect(_ context.Context, name string) (st container.State, err error) {
	err = r.c.Call("OXS.Inspect", name, &st)
	return st, err
}
func (r *oxsRemote) Running(context.Context) (names []string, err error) {
	err = r.c.Call("OXS.Running", 0, &names)
	return names, err
}
func (r *oxsRemote) Address(_ context.Context, name string) (addr string, err error) {
	err = r.c.Call("OXS.Address", name, &addr)
	return addr, err
}
func (r *oxsRemote) CreateVolume(context.Context, string) error        { return nil }
func (r *oxsRemote) Images(context.Context) ([]string, error)          { return nil, nil }
func (r *oxsRemote) ContainerImages(context.Context) ([]string, error) { return nil, nil }
func (r *oxsRemote) RemoveImage(context.Context, string) error         { return nil }

// Command runs only resume.sh, on the host as Fake does. Everything else
// (the exec seam's own `true`, the proxy's read of the guest's
// resolv.conf, which on the host is the host's) is `true`.
func (r *oxsRemote) Command(ctx context.Context, name string, opt container.ExecOptions, argv ...string) *exec.Cmd {
	if len(argv) == 0 || !strings.HasSuffix(argv[len(argv)-1], projectdef.FileResume) {
		argv = []string{"true"}
	}
	cmd := exec.CommandContext(ctx, argv[0], argv[1:]...)
	cmd.Dir = opt.Workdir
	cmd.Env = append(append(os.Environ(), opt.Env...), opt.Secrets...)
	return cmd
}

// oxsResume is the project's resume.sh: it waits for the walk to write
// the exit code, and gives up after a minute so an orphan (its child
// was killed) cannot outlive the test.
const oxsResume = `#!/bin/sh
d="$MODEL_OXS_CTL/$BOUGH_SESSION"
: > "$d.waiting"
i=0
while [ ! -f "$d.resume" ]; do
  i=$((i+1))
  if [ $i -gt 6000 ]; then rm -f "$d.waiting"; exit 3; fi
  sleep 0.01
done
code=$(cat "$d.resume")
rm -f "$d.resume" "$d.waiting"
exit "$code"
`

// --- the adapter ---

// oxsChild is the parent's end of one walk's child.
type oxsChild struct {
	ready, done chan string
	cmds        chan string
	gone        chan struct{}
}

// oxsStopper is an external stop in flight: its caller and the outcome it
// saw (ok: the stop succeeded as its caller reports success).
type oxsStopper struct {
	by     string
	result chan oxsStopResult
}

type oxsStopResult struct {
	ok  bool
	err error // the harness's own failure, not the stop's
}

type oxsAdapter struct {
	t    *testing.T
	home string
	hist string
	ctl  string
	rt   *oxsRuntime
	sup  *serve.Supervisor
	api  *serve.API
	srv  *httptest.Server
	rpcL net.Listener

	mu       sync.Mutex
	children map[string]*oxsChild

	n       int
	id      string
	busy    bool // the child's exec is in flight
	stopper *oxsStopper
	prev    oxsFields // what the stop in flight saw before MarkStopped

	gate   gate
	action string
	trace  []tracecheck.Step
	traces [][]tracecheck.Step

	// stopErrAsOk is the wrong-adapter test's bug: RuntimeStopErr lets
	// the stop succeed, so no Restore runs.
	stopErrAsOk bool
}

func newOxsAdapter(t *testing.T) *oxsAdapter {
	root, err := os.MkdirTemp("", "boxs-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(root) })
	root, _ = filepath.EvalSymlinks(root)
	a := &oxsAdapter{t: t, home: filepath.Join(root, "home"), ctl: filepath.Join(root, "ctl"), rt: newOxsRuntime(), children: map[string]*oxsChild{}}
	a.hist = filepath.Join(a.home, ".bough", "history")
	for _, d := range []string{a.hist, a.ctl} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	// The project: no repos, no identity, the waiting resume.sh; its image
	// is built here, so the child finds it.
	if _, err := projectdef.CreateEmpty(a.home, oxsSlug, ""); err != nil {
		t.Fatal(err)
	}
	if err := projectdef.WriteFile(a.home, oxsSlug, projectdef.FileYAML, "repos: []\n"); err != nil {
		t.Fatal(err)
	}
	if err := projectdef.WriteFile(a.home, oxsSlug, projectdef.FileResume, oxsResume); err != nil {
		t.Fatal(err)
	}
	p, err := projectdef.Load(a.home, oxsSlug)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := orb.EnsureImage(context.Background(), a.rt, a.home, p, nil); err != nil {
		t.Fatal(err)
	}

	srv := rpc.NewServer()
	if err := srv.RegisterName("OXS", &oxsServer{a: a}); err != nil {
		t.Fatal(err)
	}
	a.rpcL, err = net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	go srv.Accept(a.rpcL)

	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	a.sup, err = serve.NewSupervisor(serve.Options{
		Exe:      exe,
		HistDir:  a.hist,
		MetaPath: filepath.Join(a.home, ".bough", "serve", "meta.json"),
		Env: []string{"HOME=" + a.home, "PATH=" + os.Getenv("PATH"),
			oxsChildEnv + "=" + a.rpcL.Addr().String(), "MODEL_OXS_CTL=" + a.ctl},
		Runtime: a.rt,
	})
	if err != nil {
		t.Fatal(err)
	}
	a.api = serve.NewAPI(a.sup)
	a.srv = httptest.NewServer(a.api)
	t.Cleanup(func() {
		a.srv.Close()
		a.sup.Close()
		a.rpcL.Close()
	})
	return a
}

func (a *oxsAdapter) child(msg string) *oxsChild {
	id, _, _ := strings.Cut(msg, "\t")
	a.mu.Lock()
	defer a.mu.Unlock()
	c := a.children[id]
	if c == nil {
		c = &oxsChild{ready: make(chan string, 1), done: make(chan string, 1), cmds: make(chan string, 1), gone: make(chan struct{})}
		a.children[id] = c
	}
	return c
}

func (a *oxsAdapter) name() string { return container.OrbName(a.id) }

// resume answers the waiting resume.sh with an exit code.
func (a *oxsAdapter) resume(code int) error {
	return writeFileAtomic(filepath.Join(a.ctl, a.id+".resume"), []byte(fmt.Sprint(code)))
}

// Init starts each walk on a fresh project session whose child opened
// its orb: running, the container up, nothing stopped.
func (a *oxsAdapter) Init() error {
	a.n++
	a.id = fmt.Sprintf("oxs%03d", a.n)
	a.busy, a.stopper, a.prev = false, nil, oxsFields{}
	a.gate.reset()
	if err := a.appendHistory("meta", map[string]any{"cwd": a.home, "mode": "project", "project": oxsSlug}); err != nil {
		return err
	}
	if err := a.resume(0); err != nil { // Open's resume.sh
		return err
	}
	c := a.child(a.id)
	if err := a.sup.Adopt(a.id); err != nil {
		return err
	}
	select {
	case msg := <-c.ready:
		if msg != "" {
			return fmt.Errorf("the child's orb.Open: %s", msg)
		}
	case <-time.After(actionTimeout):
		return errors.New("waiting for the child's orb.Open: timed out")
	}
	a.rt.mu.Lock()
	a.rt.holdStart[a.name()] = true
	a.rt.mu.Unlock()
	a.api.ForgetRunning()
	a.trace = nil
	a.action = "Init"
	return nil
}

// Cleanup lets every held call finish, ends the child and removes the
// orb, so the next walk's reaper finds nothing of this one.
func (a *oxsAdapter) Cleanup() error {
	c := a.child(a.id)
	close(c.gone)
	a.rt.mu.Lock()
	delete(a.rt.holdStart, a.name())
	delete(a.rt.armed, a.name())
	a.rt.mu.Unlock()
	if h := a.rt.startHold(a.name()); h != nil {
		h.ch <- "abandon"
	}
	if a.stopper != nil {
		if h := a.rt.stopHold(a.name()); h != nil {
			h.ch <- "ok"
			h.ch <- "ok"
		}
		<-a.stopper.result
		a.stopper = nil
	}
	a.resume(1)
	if err := a.sup.Kill(a.id); err != nil {
		return err
	}
	if err := orb.Remove(context.Background(), a.rt, a.home, a.id); err != nil {
		return err
	}
	os.Remove(filepath.Join(a.ctl, a.id+".resume"))
	os.Remove(filepath.Join(a.ctl, a.id+".waiting"))
	if len(a.trace) > 0 {
		a.traces = append(a.traces, a.trace)
		a.trace = nil
	}
	return nil
}

func (a *oxsAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Orb", Index: 0}: a}, nil
}

// observe reads the role's fields: state.json, the runtime, serve's
// child table and stoppedAt, the calls the runtime holds, and (for the
// stop in flight) what its caller was and what MarkStopped read.
func (a *oxsAdapter) observe() (oxsFields, error) {
	var o oxsFields
	st, err := orb.ReadState(a.home, a.id)
	if err != nil {
		return o, err
	}
	o.file, o.phase = string(st.Status), oxsPhase(st.Phase)
	// The child never writes "stopped" in this flow (its own Orb.Stop is
	// kept out), and serve writes nothing else: a Restore puts back what
	// it replaced, and a replaced "stopped" was serve's.
	o.writer = "child"
	if st.Status == orb.StatusStopped {
		o.writer = "serve"
	}
	cs, err := a.rt.Fake.Inspect(context.Background(), a.name()) // not a held stop's
	if err != nil {
		return o, err
	}
	o.ctr = string(cs)
	o.owner = a.sup.Live(a.id)
	o.step = "idle"
	if a.rt.startHold(a.name()) != nil {
		o.step = "starting"
	} else if a.busy {
		o.step = "resuming"
	}
	o.stop = "none"
	switch a.rt.stopStage(a.name()) {
	case "marked":
		o.stop = "marked"
	case "done":
		o.stop = "done"
	}
	if o.stop != "none" && a.stopper != nil {
		o.by, o.pfile, o.pphase, o.pmark = a.stopper.by, a.prev.file, a.prev.phase, a.prev.mark
	}
	if at, ok := a.api.StoppedAt(a.id); ok {
		o.mark = !st.UpdatedAt.After(at)
	}
	a.rt.mu.Lock()
	created := a.rt.created[a.name()]
	a.rt.mu.Unlock()
	tok, _ := os.ReadFile(filepath.Join(orb.Dir(a.home, a.id), "token"))
	o.ctrok = cs != container.StateMissing && created == strings.TrimSpace(string(tok))
	return o, nil
}

// GetState is the role's state, after checking that serve shows it as
// the spec's status and up.
func (a *oxsAdapter) GetState() (map[string]any, error) {
	o, err := a.observe()
	if err != nil {
		return nil, err
	}
	if a.action != "" {
		name := a.action
		if name != "Init" {
			name = "Orb#0." + name
		}
		a.trace = append(a.trace, tracecheck.Step{Action: name, State: oxsQualify(o.state())})
		a.action = ""
	}
	if err := a.checkViews(o); err != nil {
		return nil, err
	}
	return o.state(), nil
}

func oxsQualify(st map[string]any) map[string]any {
	out := map[string]any{}
	for k, v := range st {
		out["Orb#0."+k] = v
	}
	return out
}

// checkViews compares the row's and the orb detail's status and up with
// the spec's. The running snapshot is taken fresh (its TTL lag is
// orb_lifecycle's).
func (a *oxsAdapter) checkViews(o oxsFields) error {
	a.api.ForgetRunning()
	ctx, cancel := actionCtx()
	defer cancel()
	var row struct {
		Session serve.Row `json:"session"`
	}
	if err := a.get(ctx, "/api/sessions/"+a.id, &row); err != nil {
		return err
	}
	ro := row.Session.Orb
	if ro == nil {
		return fmt.Errorf("session %s row has no orb: %+v", a.id, row.Session)
	}
	var detail struct {
		Orb *serve.OrbState `json:"orb"`
	}
	if err := a.get(ctx, "/api/sessions/"+a.id+"/orb", &detail); err != nil {
		return err
	}
	if detail.Orb == nil {
		return fmt.Errorf("session %s has no orb detail", a.id)
	}
	var bad []string
	want := func(what string, got, exp any) {
		if got != exp {
			bad = append(bad, fmt.Sprintf("%s = %v, the model says %v", what, got, exp))
		}
	}
	want("row status", string(ro.Status), o.status())
	want("row up", ro.Up, o.up())
	// The detail is the row's orbState, except that a "running" one asks
	// the runtime (sessionOrb): a Restore's "running" over a stopped
	// container, which the row shows up (UpOnlyWhileContainerRuns, refuted
	// in the spec), the detail shows stopped.
	dStatus, dUp := o.status(), o.up()
	if dStatus == "running" && o.ctr != "running" {
		dStatus, dUp = "stopped", false
	}
	want("orb detail status", string(detail.Orb.Status), dStatus)
	want("orb detail up", detail.Orb.Up, dUp)
	if len(bad) > 0 {
		return fmt.Errorf("serve's view of %s disagrees with the model in state %v: %s", a.id, o.state(), strings.Join(bad, "; "))
	}
	return nil
}

func (a *oxsAdapter) get(ctx context.Context, path string, out any) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, a.srv.URL+path, nil)
	if err != nil {
		return err
	}
	resp, err := a.srv.Client().Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("GET %s = %d", path, resp.StatusCode)
	}
	return json.NewDecoder(resp.Body).Decode(out)
}

// act runs one action: the spec's require, read off the observation,
// gates it; do performs it.
func (a *oxsAdapter) act(name string, require func(oxsFields) bool, do func(oxsFields) error) error {
	o, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(require(o)) {
		return nil
	}
	a.action = name
	return do(o)
}

// --- the external stops ---

// startStop arms the runtime to hold this stop's rt.Stop, runs call in
// the background and returns once MarkStopped is written and rt.Stop is
// waiting.
func (a *oxsAdapter) startStop(o oxsFields, by string, call func() (bool, error)) error {
	a.prev = o
	s := &oxsStopper{by: by, result: make(chan oxsStopResult, 1)}
	a.rt.mu.Lock()
	a.rt.armed[a.name()] = &oxsHold{two: by == "serve", ch: make(chan string, 2)}
	a.rt.mu.Unlock()
	a.stopper = s
	go func() {
		ok, err := call()
		s.result <- oxsStopResult{ok, err}
	}()
	return waitUntil("the stop's rt.Stop", func() bool { return a.rt.stopStage(a.name()) == "marked" })
}

func (a *oxsAdapter) ServeStopOrb() error {
	return a.act("ServeStopOrb", func(o oxsFields) bool { return o.stop == "none" && o.up() }, func(o oxsFields) error {
		return a.startStop(o, "serve", func() (bool, error) {
			req, _ := http.NewRequest(http.MethodPost, a.srv.URL+"/api/sessions/"+a.id+"/orb/stop", nil)
			resp, err := a.srv.Client().Do(req)
			if err != nil {
				return false, err
			}
			resp.Body.Close()
			switch resp.StatusCode {
			case http.StatusOK:
				return true, nil
			case http.StatusInternalServerError:
				return false, nil
			}
			return false, fmt.Errorf("POST orb/stop = %d", resp.StatusCode)
		})
	})
}

// ReaperStop is a reaper tick long after the orb's last activity.
func (a *oxsAdapter) ReaperStop() error {
	return a.act("ReaperStop", func(o oxsFields) bool {
		return o.stop == "none" && (o.file == "running" || o.file == "failed")
	}, func(o oxsFields) error {
		return a.startStop(o, "serve", func() (bool, error) {
			stopped := a.api.ReapIdleOrbs(context.Background(), time.Hour, time.Now().Add(48*time.Hour))
			return slices.Contains(stopped, a.id), nil
		})
	})
}

func (a *oxsAdapter) CliProjectStop() error {
	return a.act("CliProjectStop", func(o oxsFields) bool { return o.stop == "none" }, func(o oxsFields) error {
		return a.startStop(o, "cli", func() (bool, error) {
			ctx, cancel := context.WithTimeout(context.Background(), actionTimeout)
			defer cancel()
			return orb.StopContainer(ctx, a.rt, a.home, a.id) == nil, nil
		})
	})
}

// finishStop waits for the stop's caller to return and checks what it
// reported.
func (a *oxsAdapter) finishStop(ok bool) error {
	var r oxsStopResult
	select {
	case r = <-a.stopper.result:
	case <-time.After(actionTimeout):
		return errors.New("waiting for the stop to return: timed out")
	}
	by := a.stopper.by
	a.stopper = nil
	if r.err != nil {
		return r.err
	}
	if r.ok != ok {
		return fmt.Errorf("%s's stop reported ok=%v, the model says %v", by, r.ok, ok)
	}
	return nil
}

func (a *oxsAdapter) RuntimeStopOk() error {
	return a.act("RuntimeStopOk", func(o oxsFields) bool { return o.stop == "marked" }, func(o oxsFields) error {
		a.rt.stopHold(a.name()).ch <- "ok"
		if o.by == "serve" {
			return waitUntil("rt.Stop to be done", func() bool { return a.rt.stopStage(a.name()) == "done" })
		}
		return a.finishStop(true)
	})
}

func (a *oxsAdapter) RuntimeStopErr() error {
	return a.act("RuntimeStopErr", func(o oxsFields) bool { return o.stop == "marked" && o.ctr != "missing" }, func(o oxsFields) error {
		h := a.rt.stopHold(a.name())
		if a.stopErrAsOk {
			h.ch <- "ok"
			h.ch <- "ok"
			return a.finishStop(true)
		}
		h.ch <- "err"
		return a.finishStop(false)
	})
}

func (a *oxsAdapter) RuntimeStopErrButDown() error {
	return a.act("RuntimeStopErrButDown", func(o oxsFields) bool { return o.stop == "marked" && o.ctr == "running" }, func(o oxsFields) error {
		a.rt.stopHold(a.name()).ch <- "down"
		return a.finishStop(false)
	})
}

func (a *oxsAdapter) RecordStoppedAt() error {
	return a.act("RecordStoppedAt", func(o oxsFields) bool { return o.stop == "done" }, func(o oxsFields) error {
		a.rt.stopHold(a.name()).ch <- "ok"
		return a.finishStop(true)
	})
}

// KillChild is serve's Kill: SIGKILL, reap, stopKilledOrb. A start or a
// resume.sh the child was waiting on is then let go: nobody waits for it.
func (a *oxsAdapter) KillChild() error {
	return a.act("KillChild", func(o oxsFields) bool { return o.owner && o.stop == "none" }, func(o oxsFields) error {
		if err := a.sup.Kill(a.id); err != nil {
			return err
		}
		if h := a.rt.startHold(a.name()); h != nil {
			h.ch <- "abandon"
			if err := waitUntil("the killed child's start to let go", func() bool { return a.rt.startHold(a.name()) == nil }); err != nil {
				return err
			}
		}
		if a.busy {
			a.resume(1)
			a.busy = false
		}
		return nil
	})
}

// ContainerRemovedBehindBack is `container rm` by someone else.
func (a *oxsAdapter) ContainerRemovedBehindBack() error {
	return a.act("ContainerRemovedBehindBack", func(o oxsFields) bool { return o.ctr != "missing" }, func(o oxsFields) error {
		return a.rt.Fake.Remove(context.Background(), a.name())
	})
}

// --- the child's exec seam ---

// waitExec waits until the child's exec is held at until, or returned.
func (a *oxsAdapter) waitExec(what string, until func() bool) error {
	c := a.child(a.id)
	deadline := time.After(actionTimeout)
	for {
		select {
		case <-c.done:
			a.busy = false
			return nil
		case <-deadline:
			return fmt.Errorf("waiting for %s: timed out", what)
		default:
		}
		if until != nil && until() {
			return nil
		}
		time.Sleep(5 * time.Millisecond)
	}
}

func (a *oxsAdapter) waiting() bool {
	_, err := os.Stat(filepath.Join(a.ctl, a.id+".waiting"))
	return err == nil
}

func (a *oxsAdapter) ChildExec() error {
	return a.act("ChildExec", func(o oxsFields) bool { return o.owner && o.step == "idle" && o.ctr != "running" }, func(o oxsFields) error {
		a.busy = true
		a.child(a.id).cmds <- "exec"
		return a.waitExec("the exec's rt.Start", func() bool { return a.rt.startHold(a.name()) != nil })
	})
}

func (a *oxsAdapter) RuntimeStartOk() error {
	return a.act("RuntimeStartOk", func(o oxsFields) bool { return o.owner && o.step == "starting" }, func(o oxsFields) error {
		a.rt.startHold(a.name()).ch <- "ok"
		return a.waitExec("resume.sh", a.waiting)
	})
}

func (a *oxsAdapter) RuntimeStartErr() error {
	return a.act("RuntimeStartErr", func(o oxsFields) bool { return o.owner && o.step == "starting" }, func(o oxsFields) error {
		a.rt.startHold(a.name()).ch <- "err"
		return a.waitExec("the failed exec", nil)
	})
}

func (a *oxsAdapter) endResume(name string, ctr bool, code int) error {
	return a.act(name, func(o oxsFields) bool {
		return o.owner && o.step == "resuming" && (o.ctr == "running") == ctr
	}, func(o oxsFields) error {
		if err := a.resume(code); err != nil {
			return err
		}
		return a.waitExec("the exec", nil)
	})
}

func (a *oxsAdapter) ResumeOk() error   { return a.endResume("ResumeOk", true, 0) }
func (a *oxsAdapter) ResumeFail() error { return a.endResume("ResumeFail", true, 1) }

// ResumeKilled: the container went down under resume.sh, which the host
// script stands in for by failing.
func (a *oxsAdapter) ResumeKilled() error { return a.endResume("ResumeKilled", false, 137) }

func (a *oxsAdapter) appendHistory(kind string, data map[string]any) error {
	path := filepath.Join(a.hist, a.id+".jsonl")
	b, _ := json.Marshal(history.Entry{Seq: 1, At: time.Now(), Kind: kind, Data: data})
	return os.WriteFile(path, append(b, '\n'), 0o644)
}

var oxsActions = map[string]map[string]fmbt.ActionFunc{"Orb": {
	"ServeStopOrb":               action((*oxsAdapter).ServeStopOrb),
	"ReaperStop":                 action((*oxsAdapter).ReaperStop),
	"CliProjectStop":             action((*oxsAdapter).CliProjectStop),
	"RuntimeStopOk":              action((*oxsAdapter).RuntimeStopOk),
	"RuntimeStopErr":             action((*oxsAdapter).RuntimeStopErr),
	"RuntimeStopErrButDown":      action((*oxsAdapter).RuntimeStopErrButDown),
	"RecordStoppedAt":            action((*oxsAdapter).RecordStoppedAt),
	"KillChild":                  action((*oxsAdapter).KillChild),
	"ContainerRemovedBehindBack": action((*oxsAdapter).ContainerRemovedBehindBack),
	"ChildExec":                  action((*oxsAdapter).ChildExec),
	"RuntimeStartOk":             action((*oxsAdapter).RuntimeStartOk),
	"RuntimeStartErr":            action((*oxsAdapter).RuntimeStartErr),
	"ResumeOk":                   action((*oxsAdapter).ResumeOk),
	"ResumeFail":                 action((*oxsAdapter).ResumeFail),
	"ResumeKilled":               action((*oxsAdapter).ResumeKilled),
}}

// walkOxsPaths drives the adapter down the walks over the checked-in
// graph and returns the first step whose state is not the spec's.
func walkOxsPaths(t *testing.T, a *oxsAdapter, cover tracecheck.Cover) error {
	t.Helper()
	b, err := pathsJSONCover(oxsSpec, cover)
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
		err := func() error {
			for si, s := range p.Trace {
				if si > 0 {
					name := strings.TrimPrefix(s.Action, "Orb#0.")
					f, ok := oxsActions["Orb"][name]
					if !ok {
						return fmt.Errorf("step %d: no action %s", si, s.Action)
					}
					if _, err := f(a, nil); err != nil {
						return fmt.Errorf("step %d (%s): %w", si, s.Action, err)
					}
					if a.gate.off {
						return fmt.Errorf("step %d (%s): the adapter's view says it is not enabled", si, s.Action)
					}
				}
				got, err := a.GetState()
				if err != nil {
					return fmt.Errorf("step %d (%s): %w", si, s.Action, err)
				}
				if diff := oxsStateDiff(s.State, oxsQualify(got)); diff != "" {
					return fmt.Errorf("step %d (%s): %s", si, s.Action, diff)
				}
			}
			return nil
		}()
		if cerr := a.Cleanup(); err == nil && cerr != nil {
			err = fmt.Errorf("Cleanup: %w", cerr)
		}
		if err != nil {
			b, _ := json.Marshal(p.Trace)
			return fmt.Errorf("walk %d: %w\nwalk: %s", pi, err, b)
		}
	}
	return nil
}

func oxsStateDiff(want, got map[string]any) string {
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

// checkOxsTraces replays every walk's own record (the actions taken and
// the states observed after them, not the path it was told to take) on
// the graph: no session transcript comes out of this flow, so this is
// its trace check.
func checkOxsTraces(t *testing.T, a *oxsAdapter) {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath(oxsSpec)), "..", "testdata", oxsSpec))
	if err != nil {
		t.Fatal(err)
	}
	steps := 0
	for i, tr := range a.traces {
		if v := g.Check(tr); v != nil {
			b, _ := json.Marshal(tr)
			t.Errorf("walk %d is not a path in the model: %v\ntrace: %s", i, v, b)
		}
		steps += len(tr) - 1
	}
	if steps == 0 {
		t.Fatal("no walk took a step")
	}
	t.Logf("%d walks, %d steps replayed on the graph", len(a.traces), steps)
}

// Every settled state (every transition with MODEL_COVER=transitions)
// against the real child and serve.
func TestOrbExecRestartVsExternalStopPaths(t *testing.T) {
	t.Parallel()
	a := newOxsAdapter(t)
	if err := walkOxsPaths(t, a, envCover()); err != nil {
		t.Fatal(err)
	}
	checkOxsTraces(t, a)
}

// The runner's random walks, in the exhaustive run only (runMBT).
func TestOrbExecRestartVsExternalStop(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newOxsAdapter(t)
	if err := runMBT(t, oxsSpec, a, oxsActions, map[string]any{"max-seq-runs": 100, "max-actions": 20, "max-parallel-runs": 0}); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkOxsTraces(t, a)
}

// A failed stop that is let through as a success never Restores: the
// walks must catch it, or a green run proves nothing.
func TestOrbExecRestartVsExternalStopPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newOxsAdapter(t)
	a.stopErrAsOk = true
	err := walkOxsPaths(t, a, envCover())
	if err == nil {
		t.Fatal("walks whose RuntimeStopErr succeeds passed; the walk is not checking state")
	}
	t.Logf("caught, as it must be: %.600s", err)
}
