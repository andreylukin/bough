//go:build !windows

package mbt

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/second_writer_process.fizz against real processes on one HOME
// and one port: serve A (the walk's first serve, the launchd job), serve
// B (whichever serve starts next), `bough serve stop`, and a terminal
// `bough --headless -r S`. S is a background agent A queues behind a
// running cap of 1 that another agent (H) holds with a blocked turn.
//
// launchd is the adapter: `managed` is its own flag, and Respawn,
// ServeStart and UpdateReinstall launch B (bootout is SIGTERM) the way
// the agent would. `serve stop` is the real verb (BOUGH_NO_LAUNCHD, so
// it never touches launchctl). A "stopping" serve is held inside
// sup.Close by the "close" test hold (serve.Options.HoldDir), port
// closed and children alive, until CloseDone lets it go on.
//
// The parent Q's child died with the constructor's serve, so a report
// S or H sends is appended to Q's file instead of waking a turn that
// would take one of S's queued model turns: every model request in a
// walk is S's.

type swProc struct {
	cmd    *exec.Cmd
	exited chan struct{}
	stdin  io.WriteCloser
}

func (p *swProc) alive() bool {
	if p == nil {
		return false
	}
	select {
	case <-p.exited:
		return false
	default:
		return true
	}
}

func (p *swProc) pid() int { return p.cmd.Process.Pid }

// waitExit waits for the process itself; groupGone for its children.
func (p *swProc) waitExit(d time.Duration) bool {
	select {
	case <-p.exited:
		return true
	case <-time.After(d):
		return false
	}
}

func (p *swProc) groupGone(d time.Duration) bool {
	deadline := time.Now().Add(d)
	for syscall.Kill(-p.pid(), 0) == nil {
		if time.Now().After(deadline) {
			return false
		}
		time.Sleep(20 * time.Millisecond)
	}
	return true
}

// kill ends the process and everything in its group.
func (p *swProc) kill() {
	if p == nil {
		return
	}
	syscall.Kill(-p.pid(), syscall.SIGKILL)
	p.waitExit(10 * time.Second)
	p.groupGone(5 * time.Second)
}

type secondWriterAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	hold string // A's BOUGH_SERVE_TEST_HOLD dir
	gate gate

	q, h, sid string
	scwd      string // S's cwd, where a terminal resumes it
	walk      int
	turn      int
	ids       []string

	a, b, cli, stop *swProc
	aSignalled      bool // A was sent SIGINT (serve stop, update)
	bStarting       bool // B is due to start: BListen launches it
	managed, upd    bool
	bwrote          bool

	pending string // S's next model turn, queued and not yet taken
	held    string // S's turn in flight
	hHeld   string // H's turn, holding the running slot

	taken map[string]int
	dirty bool

	// killUpdate is TestSecondWriterProcessCatchesWrongAdapter's bug:
	// UpdateSignal SIGKILLs A instead of asking it to stop.
	killUpdate bool
}

func newSecondWriterAdapter(t *testing.T) *secondWriterAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig, Env: []string{"BOUGH_NO_LAUNCHD=1"}})
	a := &secondWriterAdapter{t: t, s: s, dir: control.Dir(s.Home), taken: map[string]int{}}
	a.hold = filepath.Join(s.Root, "hold")
	if err := os.MkdirAll(a.hold, 0o755); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := actionCtx()
	defer cancel()
	a.scwd = s.Dir(t, "parent")
	row, err := s.CreateSession(ctx, a.scwd, "the agents' parent")
	if err != nil {
		t.Fatal(err)
	}
	a.q = row.ID
	if _, err := waitRow(s, a.q, "the parent's turn", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		t.Fatal(err)
	}
	// From here on every serve is the adapter's own process.
	s.Shutdown()
	t.Cleanup(a.teardown)
	return a
}

func (a *secondWriterAdapter) launch(env []string, args ...string) (*swProc, error) {
	cmd := a.s.Command(a.s.Bin(), env, args...)
	p := &swProc{cmd: cmd, exited: make(chan struct{})}
	if args[0] != "serve" {
		w, err := cmd.StdinPipe()
		if err != nil {
			return nil, err
		}
		p.stdin = w
	}
	if err := cmd.Start(); err != nil {
		return nil, err
	}
	go func() { cmd.Wait(); close(p.exited) }()
	return p, nil
}

func (a *secondWriterAdapter) launchServe(env ...string) (*swProc, error) {
	return a.launch(env, "serve", "--run", a.s.Addr)
}

// teardown kills every process a walk left: the next Init starts over.
func (a *secondWriterAdapter) teardown() {
	os.Remove(filepath.Join(a.hold, "close"))
	for _, p := range []*swProc{a.cli, a.stop, a.b, a.a} {
		p.kill()
	}
	a.cli, a.stop, a.b, a.a = nil, nil, nil, nil
}

// Init starts a fresh A with H holding the one running slot and S
// queued behind it.
func (a *secondWriterAdapter) Init() error {
	a.gate.reset()
	if a.sid != "" && !a.dirty {
		return nil
	}
	a.walk++
	a.dirty = false
	a.teardown()
	// A turn queued for a walk's S and never taken would answer the next
	// walk's first request.
	stale, _ := filepath.Glob(filepath.Join(a.dir, "*.json"))
	for _, f := range stale {
		os.Remove(f)
	}
	a.aSignalled, a.bStarting, a.managed, a.upd, a.bwrote = false, false, true, false, false
	a.pending, a.held, a.hHeld = "", "", ""

	if err := os.WriteFile(filepath.Join(a.hold, "close"), nil, 0o644); err != nil {
		return err
	}
	p, err := a.launchServe("BOUGH_SERVE_TEST_HOLD=" + a.hold)
	if err != nil {
		return err
	}
	a.a = p
	if err := a.waitHealthy(p); err != nil {
		return err
	}
	// A task a past walk left queued starts now: let it finish before
	// anything is queued for the model, so it cannot take that turn.
	if err := a.settle(); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	hold := a.nextTurn("h")
	control.Queue(a.t, a.dir, hold, control.Turn{Mode: "block", Text: "held " + hold})
	h, queued, err := a.s.CreateAgent(ctx, a.q, "hold the slot "+hold, 1, 1<<20)
	if err != nil {
		return err
	}
	if queued {
		return fmt.Errorf("the slot holder %s was queued: something else holds the running slot", h.ID)
	}
	a.h, a.hHeld = h.ID, hold
	control.WaitTaken(a.t, a.dir, hold, actionTimeout)
	tk, queued, err := a.s.CreateAgent(ctx, a.q, fmt.Sprintf("the queued task of walk %d", a.walk), 1, 1<<20)
	if err != nil {
		return err
	}
	if !queued {
		return fmt.Errorf("task %s started past a running cap of 1", tk.ID)
	}
	a.sid = tk.ID
	a.ids = append(a.ids, tk.ID)
	return nil
}

func (a *secondWriterAdapter) nextTurn(prefix string) string {
	a.turn++
	return fmt.Sprintf("%s%05d", prefix, a.turn)
}

// waitHealthy waits for p to answer on the port, or to exit.
func (a *secondWriterAdapter) waitHealthy(p *swProc) error {
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		if !p.alive() {
			return fmt.Errorf("serve pid %d exited before it answered:\n%s", p.pid(), outTail(a.s.Output()))
		}
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		_, err := a.s.Build(ctx)
		cancel()
		if err == nil && a.listening(p) {
			return nil
		}
		time.Sleep(50 * time.Millisecond)
	}
	return fmt.Errorf("serve pid %d not answering after %s", p.pid(), actionTimeout)
}

// listening says whether p holds a listening TCP socket.
func (a *secondWriterAdapter) listening(p *swProc) bool {
	out, _ := exec.Command("lsof", "-a", "-p", strconv.Itoa(p.pid()), "-iTCP", "-sTCP:LISTEN", "-t").Output()
	return len(bytes.TrimSpace(out)) > 0
}

// portClosed waits until nothing accepts on the port.
func (a *secondWriterAdapter) portClosed() error {
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		c, err := net.DialTimeout("tcp", a.s.Addr, 200*time.Millisecond)
		if err != nil {
			return nil
		}
		c.Close()
		time.Sleep(20 * time.Millisecond)
	}
	return fmt.Errorf("port %s still accepting after %s", a.s.Addr, actionTimeout)
}

func outTail(s string) string {
	if len(s) > 4000 {
		return s[len(s)-4000:]
	}
	return s
}

// settle waits until no session is running or queued.
func (a *secondWriterAdapter) settle() error {
	ctx, cancel := actionCtx()
	defer cancel()
	var last []serve.Row
	for {
		rows, err := a.s.ListSessions(ctx, true)
		if err == nil {
			busy := false
			for _, r := range rows {
				if r.Status == serve.StatusRunning || r.Status == serve.StatusQueued {
					busy = true
				}
				if r.ID == a.q && r.Agents != nil && r.Agents.Running+r.Agents.Queued > 0 {
					busy = true
				}
			}
			if !busy {
				return nil
			}
			last = rows
		}
		select {
		case <-ctx.Done():
			b, _ := json.Marshal(last)
			return fmt.Errorf("sessions still busy: %s", b)
		case <-time.After(100 * time.Millisecond):
		}
	}
}

func (a *secondWriterAdapter) Cleanup() error { return nil }

func (a *secondWriterAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Host", Index: 0}: a}, nil
}

func (a *secondWriterAdapter) historyPath() string {
	return filepath.Join(a.s.Home, ".bough", "history", a.sid+".jsonl")
}

// turnOpen reads S's history: an input with no done/cancelled after it.
func (a *secondWriterAdapter) turnOpen() bool {
	entries, err := history.Read(a.historyPath())
	if err != nil {
		return false
	}
	st, _ := serve.StatusOf(entries, true)
	return st == serve.StatusRunning || st == serve.StatusNeedsYou
}

// taskQueued is S's persisted task in meta.json, which every serve
// reads its queue from.
func (a *secondWriterAdapter) taskQueued() (bool, error) {
	b, err := os.ReadFile(filepath.Join(a.s.Home, ".bough", "serve", "meta.json"))
	if err != nil {
		return false, err
	}
	var f struct {
		Sessions map[string]serve.SessionMeta `json:"sessions"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		return false, err
	}
	return f.Sessions[a.sid].Task != nil, nil
}

func (a *secondWriterAdapter) metaBytes() []byte {
	b, _ := os.ReadFile(filepath.Join(a.s.Home, ".bough", "serve", "meta.json"))
	return b
}

// pidfile names the serve ~/.bough/serve.pid records: "A", "B", "none",
// or the pid when it is neither.
func (a *secondWriterAdapter) pidfile() string {
	b, err := os.ReadFile(filepath.Join(a.s.Home, ".bough", "serve.pid"))
	if err != nil {
		return "none"
	}
	f := strings.Fields(string(b))
	if len(f) == 0 {
		return "empty"
	}
	pid, _ := strconv.Atoi(f[0])
	switch {
	case a.a != nil && pid == a.a.pid():
		return "A"
	case a.b != nil && pid == a.b.pid():
		return "B"
	}
	return "pid " + f[0]
}

// kids says which serve has a child with S's history file open: the
// session's writer, found among each serve's own children.
func (a *secondWriterAdapter) kids() (akid, bkid bool) {
	if _, err := os.Stat(a.historyPath()); err != nil {
		return false, false
	}
	parent := map[string]string{}
	var pids []string
	for name, p := range map[string]*swProc{"A": a.a, "B": a.b} {
		if !p.alive() {
			continue
		}
		out, _ := exec.Command("pgrep", "-P", strconv.Itoa(p.pid())).Output()
		for _, c := range strings.Fields(string(out)) {
			parent[c] = name
			pids = append(pids, c)
		}
	}
	if len(pids) == 0 {
		return false, false
	}
	out, _ := exec.Command("lsof", "-a", "-p", strings.Join(pids, ","), "-t", "--", a.historyPath()).Output()
	for _, c := range strings.Fields(string(out)) {
		switch parent[c] {
		case "A":
			akid = true
		case "B":
			bkid = true
		}
	}
	return akid, bkid
}

// shown is S's status on the page, from whichever serve answers.
func (a *secondWriterAdapter) shown() string {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	rows, err := a.s.ListSessions(ctx, true)
	if err != nil {
		if errors.Is(err, syscall.ECONNREFUSED) || strings.Contains(err.Error(), "connection refused") {
			return "offline"
		}
		return "error: " + err.Error()
	}
	for _, r := range rows {
		if r.ID != a.sid {
			continue
		}
		switch r.Status {
		// The spec's done is a closed turn: a resume closes a dangling
		// one as cancelled, which the page shows as stopped.
		case serve.StatusDone, serve.StatusStopped:
			return "done"
		}
		return string(r.Status)
	}
	return "missing"
}

func (a *secondWriterAdapter) GetState() (map[string]any, error) {
	st := map[string]any{
		"managed": a.managed,
		"waiting": a.stop != nil,
		"upd":     a.upd,
		"bwrote":  a.bwrote,
		"pf":      a.pidfile(),
	}
	switch {
	case !a.a.alive():
		st["a"] = "gone"
	case a.aSignalled:
		st["a"] = "stopping"
	default:
		st["a"] = "serving"
	}
	switch {
	case a.bStarting:
		st["b"] = "starting"
	case a.b == nil:
		st["b"] = "none"
	case a.b.alive():
		st["b"] = "serving"
	default:
		st["b"] = "failed"
	}
	queued, err := a.taskQueued()
	if err != nil {
		return nil, err
	}
	st["task"] = "started"
	if queued {
		st["task"] = "queued"
	}
	st["akid"], st["bkid"] = a.kids()
	open := a.turnOpen()
	st["turn"] = "closed"
	if open {
		st["turn"] = "open"
	}
	switch {
	case !a.cli.alive():
		st["cli"] = "none"
	case open:
		st["cli"] = "turn"
	default:
		st["cli"] = "idle"
	}
	st["shown"] = a.shown()
	return st, nil
}

// ensurePending has one model turn queued for S's next request. A turn
// a child took and never answered (a start that was killed) is gone.
func (a *secondWriterAdapter) ensurePending() {
	if a.pending != "" && !a.wasTaken(a.pending) {
		return
	}
	a.pending = a.nextTurn("s")
	control.Queue(a.t, a.dir, a.pending, control.Turn{Mode: "block", Text: "finished " + a.pending})
}

func (a *secondWriterAdapter) wasTaken(name string) bool {
	_, err := os.Stat(filepath.Join(a.dir, name+".taken"))
	return err == nil
}

// takePending waits for S's request to take the pending turn, which is
// then S's turn in flight.
func (a *secondWriterAdapter) takePending() error {
	deadline := time.Now().Add(actionTimeout)
	for !a.wasTaken(a.pending) {
		if time.Now().After(deadline) {
			return fmt.Errorf("S's turn %s was never taken", a.pending)
		}
		time.Sleep(20 * time.Millisecond)
	}
	a.held, a.pending = a.pending, ""
	return nil
}

func (a *secondWriterAdapter) waitTurn(open bool) error {
	deadline := time.Now().Add(actionTimeout)
	for a.turnOpen() != open {
		if time.Now().After(deadline) {
			return fmt.Errorf("S's turn open=%v never showed in its history", open)
		}
		time.Sleep(20 * time.Millisecond)
	}
	return nil
}

func (a *secondWriterAdapter) serving() (aServing, bServing bool) {
	return a.a.alive() && !a.aSignalled, !a.bStarting && a.b.alive()
}

// ---- serve A ----

// DrainA frees the running slot: H's turn ends and A's queue starts S.
func (a *secondWriterAdapter) DrainA() error {
	queued, err := a.taskQueued()
	if err != nil {
		return err
	}
	as, _ := a.serving()
	_, bkid := a.kids()
	if !a.gate.pass(as && queued && !a.cli.alive() && !bkid) {
		return nil
	}
	a.ensurePending()
	control.Release(a.t, a.dir, a.hHeld)
	a.hHeld = ""
	if err := a.takePending(); err != nil {
		return err
	}
	return a.waitTurn(true)
}

// ServeStop is the real `bough serve stop`, left running: it returns
// in StopReturn.
func (a *secondWriterAdapter) ServeStop() error {
	as, _ := a.serving()
	if !a.gate.pass(as && a.stop == nil && !a.upd) {
		return nil
	}
	p, err := a.launch([]string{"BOUGH_NO_LAUNCHD=1"}, "serve", "stop")
	if err != nil {
		return err
	}
	a.stop, a.managed, a.aSignalled = p, false, true
	return a.portClosed()
}

func (a *secondWriterAdapter) StopReturn() error {
	if !a.gate.pass(a.stop != nil) {
		return nil
	}
	if !a.stop.waitExit(actionTimeout) {
		return errors.New("serve stop did not return")
	}
	a.stop = nil
	return nil
}

// CloseDone lets the stopping A go on: it kills its children, runs its
// exit cleanup and exits.
func (a *secondWriterAdapter) CloseDone() error {
	if !a.gate.pass(a.a.alive() && a.aSignalled) {
		return nil
	}
	os.Remove(filepath.Join(a.hold, "close"))
	if !a.a.waitExit(actionTimeout) || !a.a.groupGone(10*time.Second) {
		return errors.New("A did not exit after its close was let go")
	}
	return nil
}

// Crash is SIGKILL to A and its children.
func (a *secondWriterAdapter) Crash() error {
	as, _ := a.serving()
	if !a.gate.pass(as) {
		return nil
	}
	syscall.Kill(-a.a.pid(), syscall.SIGKILL)
	if !a.a.waitExit(actionTimeout) || !a.a.groupGone(10*time.Second) {
		return errors.New("A outlived SIGKILL")
	}
	return nil
}

// UpdateSignal is restartServe's SIGINT, the launchd agent still loaded.
func (a *secondWriterAdapter) UpdateSignal() error {
	as, _ := a.serving()
	if !a.gate.pass(as && a.stop == nil && !a.upd) {
		return nil
	}
	a.upd, a.aSignalled = true, true
	if a.killUpdate {
		a.a.cmd.Process.Kill()
		a.a.waitExit(actionTimeout)
		return nil
	}
	a.a.cmd.Process.Signal(syscall.SIGINT)
	return a.portClosed()
}

// UpdateReinstall is installServeAgent: bootout (SIGTERM to whatever
// launchd respawned) and bootstrap of the new binary, which starts at
// once. restartServe's own pidfile removal is covered in cmd/bough.
func (a *secondWriterAdapter) UpdateReinstall() error {
	if !a.gate.pass(a.upd && !a.a.alive()) {
		return nil
	}
	if a.b.alive() && !a.bStarting {
		a.b.cmd.Process.Signal(syscall.SIGTERM)
		if !a.b.waitExit(actionTimeout) || !a.b.groupGone(10*time.Second) {
			return errors.New("B outlived its bootout")
		}
	}
	a.upd, a.managed = false, true
	a.startB()
	return nil
}

// ---- serve B ----

// startB is a start that has not loaded anything yet: BListen runs it.
func (a *secondWriterAdapter) startB() {
	a.bStarting, a.bwrote = true, false
}

func (a *secondWriterAdapter) bIdle() bool { return !a.bStarting && !a.b.alive() }

func (a *secondWriterAdapter) ServeStart() error {
	as, _ := a.serving()
	if !a.gate.pass(!a.managed && a.stop == nil && !as && a.bIdle()) {
		return nil
	}
	a.managed = true
	a.startB()
	return nil
}

func (a *secondWriterAdapter) ServeRunDirect() error {
	if !a.gate.pass(a.bIdle()) {
		return nil
	}
	a.startB()
	return nil
}

func (a *secondWriterAdapter) Respawn() error {
	if !a.gate.pass(a.managed && !a.a.alive() && a.bIdle()) {
		return nil
	}
	a.startB()
	return nil
}

// BListen runs B's start: it either serves or exits. bwrote is what
// the start did to meta.json.
func (a *secondWriterAdapter) BListen() error {
	if !a.gate.pass(a.bStarting) {
		return nil
	}
	queued, err := a.taskQueued()
	if err != nil {
		return err
	}
	if queued {
		// If B drains S, S's request takes this.
		a.ensurePending()
	}
	before := a.metaBytes()
	p, err := a.launchServe()
	if err != nil {
		return err
	}
	a.b, a.bStarting = p, false
	deadline := time.Now().Add(actionTimeout)
	for p.alive() && !a.listening(p) {
		if time.Now().After(deadline) {
			return errors.New("B neither served nor exited")
		}
		time.Sleep(20 * time.Millisecond)
	}
	if p.alive() {
		if err := a.waitHealthy(p); err != nil {
			return err
		}
		// A queued task is drained at once; give it the time to show.
		if queued {
			for i := 0; i < 250 && !a.wasTaken(a.pending); i++ {
				time.Sleep(20 * time.Millisecond)
			}
			if a.wasTaken(a.pending) {
				a.takePending()
				if err := a.waitTurn(true); err != nil {
					return err
				}
			}
		}
	} else if a.wasTaken(a.pending) {
		// A child the failed start drained took it and was killed.
		a.pending = ""
	}
	a.bwrote = !bytes.Equal(before, a.metaBytes())
	return nil
}

// ---- session S ----

func (a *secondWriterAdapter) ChildFinish() error {
	akid, bkid := a.kids()
	cliTurn := a.cli.alive() && a.turnOpen()
	if !a.gate.pass(a.turnOpen() && (akid || bkid) && !cliTurn && a.held != "") {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	return a.waitTurn(false)
}

func (a *secondWriterAdapter) WebPrompt() error {
	sh := a.shown()
	if !a.gate.pass((sh == "done" || sh == "interrupted") && !a.cli.alive()) {
		return nil
	}
	a.ensurePending()
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.sid, "web "+a.pending); err != nil {
		return err
	}
	if err := a.takePending(); err != nil {
		return err
	}
	return a.waitTurn(true)
}

// CliResume is `bough -r S` in a terminal, in S's directory.
func (a *secondWriterAdapter) CliResume() error {
	queued, err := a.taskQueued()
	if err != nil {
		return err
	}
	akid, bkid := a.kids()
	if !a.gate.pass(!a.cli.alive() && !akid && !bkid && !queued) {
		return nil
	}
	cmd := a.s.Command(a.s.Bin(), []string{"BOUGH_NO_LAUNCHD=1"}, "--headless", "-r", a.sid)
	cmd.Dir = a.scwd
	w, err := cmd.StdinPipe()
	if err != nil {
		return err
	}
	if err := cmd.Start(); err != nil {
		return err
	}
	p := &swProc{cmd: cmd, exited: make(chan struct{}), stdin: w}
	go func() { cmd.Wait(); close(p.exited) }()
	a.cli = p
	// Resumed once it has S's file open (and closed a dangling turn).
	deadline := time.Now().Add(actionTimeout)
	for {
		out, _ := exec.Command("lsof", "-a", "-p", strconv.Itoa(p.pid()), "-t", "--", a.historyPath()).Output()
		if len(bytes.TrimSpace(out)) > 0 {
			break
		}
		if !p.alive() {
			return fmt.Errorf("the terminal exited:\n%s", outTail(a.s.Output()))
		}
		if time.Now().After(deadline) {
			return errors.New("the terminal never opened S's history")
		}
		time.Sleep(20 * time.Millisecond)
	}
	return a.waitTurn(false)
}

func (a *secondWriterAdapter) CliPrompt() error {
	if !a.gate.pass(a.cli.alive() && !a.turnOpen()) {
		return nil
	}
	a.ensurePending()
	if _, err := fmt.Fprintf(a.cli.stdin, "terminal %s\n", a.pending); err != nil {
		return err
	}
	if err := a.takePending(); err != nil {
		return err
	}
	return a.waitTurn(true)
}

func (a *secondWriterAdapter) CliFinish() error {
	if !a.gate.pass(a.cli.alive() && a.turnOpen() && a.held != "") {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	return a.waitTurn(false)
}

func (a *secondWriterAdapter) CliExit() error {
	if !a.gate.pass(a.cli.alive() && !a.turnOpen()) {
		return nil
	}
	a.cli.stdin.Close()
	if !a.cli.waitExit(actionTimeout) {
		return errors.New("the terminal did not exit on EOF")
	}
	return nil
}

func swpAction(name string, f func(*secondWriterAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*secondWriterAdapter)
		open := !a.gate.off
		err := f(a)
		if open && !a.gate.off {
			a.taken[name]++
			a.dirty = true
		}
		return nil, err
	}
}

var secondWriterActions = map[string]map[string]fmbt.ActionFunc{"Host": {
	"DrainA":          swpAction("DrainA", (*secondWriterAdapter).DrainA),
	"ServeStop":       swpAction("ServeStop", (*secondWriterAdapter).ServeStop),
	"StopReturn":      swpAction("StopReturn", (*secondWriterAdapter).StopReturn),
	"CloseDone":       swpAction("CloseDone", (*secondWriterAdapter).CloseDone),
	"Crash":           swpAction("Crash", (*secondWriterAdapter).Crash),
	"UpdateSignal":    swpAction("UpdateSignal", (*secondWriterAdapter).UpdateSignal),
	"UpdateReinstall": swpAction("UpdateReinstall", (*secondWriterAdapter).UpdateReinstall),
	"ServeStart":      swpAction("ServeStart", (*secondWriterAdapter).ServeStart),
	"ServeRunDirect":  swpAction("ServeRunDirect", (*secondWriterAdapter).ServeRunDirect),
	"Respawn":         swpAction("Respawn", (*secondWriterAdapter).Respawn),
	"BListen":         swpAction("BListen", (*secondWriterAdapter).BListen),
	"ChildFinish":     swpAction("ChildFinish", (*secondWriterAdapter).ChildFinish),
	"WebPrompt":       swpAction("WebPrompt", (*secondWriterAdapter).WebPrompt),
	"CliResume":       swpAction("CliResume", (*secondWriterAdapter).CliResume),
	"CliPrompt":       swpAction("CliPrompt", (*secondWriterAdapter).CliPrompt),
	"CliFinish":       swpAction("CliFinish", (*secondWriterAdapter).CliFinish),
	"CliExit":         swpAction("CliExit", (*secondWriterAdapter).CliExit),
}}

func secondWriterOptions() map[string]any {
	return map[string]any{"max-seq-runs": 150, "max-actions": 8, "max-parallel-runs": 0}
}

// secondWriterHistory reads S's transcript as one path the spec allows,
// all through the web: its first input is A's drain, a done is the
// child's finish, a later input a web prompt. A "cancelled" closing an
// open turn is the writer dying and the next one closing its turn: the
// first death is the update's stop of A (then launchd's respawn serves
// B), the second the update's bootout of B. The next input, if any, is
// the web prompt that resumed it; a cancel with none after it was a
// terminal's resume. Two deaths are all one walk has (A once, B at the
// reinstall), and a third is not a path.
func secondWriterHistory(entries []history.Entry) []tracecheck.Step {
	turn := func(s string) map[string]any { return map[string]any{"Host#0.turn": s} }
	step := func(action, t string) tracecheck.Step {
		return tracecheck.Step{Action: "Host#0." + action, State: turn(t)}
	}
	var kinds []string
	lastClose := ""
	for _, e := range entries {
		switch e.Kind {
		case "input":
			kinds = append(kinds, "input")
			lastClose = ""
		case "cancelled":
			kinds = append(kinds, "cancelled")
			lastClose = "cancelled"
		case "done":
			// The loop writes a done after every cancel: bookkeeping.
			if lastClose == "cancelled" {
				continue
			}
			kinds = append(kinds, "done")
			lastClose = "done"
		}
	}
	steps := []tracecheck.Step{{Action: "Init", State: turn("closed")}}
	started, deaths := false, 0
	for i := 0; i < len(kinds); i++ {
		switch kinds[i] {
		case "input":
			if !started {
				steps = append(steps, step("DrainA", "open"))
				started = true
			} else {
				steps = append(steps, step("WebPrompt", "open"))
			}
		case "done":
			steps = append(steps, step("ChildFinish", "closed"))
		case "cancelled":
			deaths++
			resumed := i+1 < len(kinds) && kinds[i+1] == "input"
			switch deaths {
			case 1:
				steps = append(steps, step("UpdateSignal", "open"), step("CloseDone", "open"))
				if resumed {
					steps = append(steps, step("Respawn", "open"), step("BListen", "open"))
				}
			case 2:
				steps = append(steps, step("UpdateReinstall", "open"))
				if resumed {
					steps = append(steps, step("BListen", "open"))
				}
			default:
				steps = append(steps, step("ThirdWriterDeath", "open"))
			}
			if resumed {
				steps = append(steps, step("WebPrompt", "open"))
				i++
			} else {
				steps = append(steps, step("CliResume", "closed"))
			}
		}
	}
	return steps
}

func init() { historyProjections["second_writer_process"] = secondWriterHistory }

// walkSecondWriter walks every path of walks against a, comparing the
// host with the spec's state at each step.
func walkSecondWriter(a *secondWriterAdapter, b []byte) error {
	var f struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		return err
	}
	for i, p := range f.Paths {
		a.dirty = true // every path starts from a fresh host
		var err error
		for j, step := range p.Trace {
			if j == 0 {
				err = a.Init()
			} else {
				name := strings.TrimPrefix(step.Action, "Host#0.")
				_, err = secondWriterActions["Host"][name](a, nil)
				if err == nil && a.gate.off {
					err = errors.New("the adapter found it disabled")
				}
			}
			if err != nil {
				return fmt.Errorf("path %d step %d (%s): %v", i, j, step.Action, err)
			}
			got, err := a.GetState()
			if err != nil {
				return fmt.Errorf("path %d step %d (%s): state: %v", i, j, step.Action, err)
			}
			if d := hostDiff(step.State, got); d != "" {
				return fmt.Errorf("path %d step %d (%s): %s", i, j, step.Action, d)
			}
		}
	}
	a.t.Logf("%d paths, actions taken: %v", len(f.Paths), a.taken)
	return nil
}

func hostDiff(want, got map[string]any) string {
	var d []string
	for k, v := range want {
		field, ok := strings.CutPrefix(k, "Host#0.")
		if !ok {
			continue
		}
		if fmt.Sprint(got[field]) != fmt.Sprint(v) {
			d = append(d, fmt.Sprintf("%s: spec %v, host %v", field, v, got[field]))
		}
	}
	sort.Strings(d)
	return strings.Join(d, "; ")
}

func TestSecondWriterProcess(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSecondWriterAdapter(t)
	if err := runMBT(t, "second_writer_process", a, secondWriterActions, secondWriterOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("actions taken: %v", a.taken)
	checkSecondWriterHistories(t, a)
}

func checkSecondWriterHistories(t *testing.T, a *secondWriterAdapter) {
	t.Helper()
	g, err := tracecheck.Load(fizzCheck(t, "second_writer_process"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		p := filepath.Join(a.s.Home, ".bough", "history", id+".jsonl")
		if _, err := os.Stat(p); err != nil {
			continue // never drained: no file, nothing to replay
		}
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), secondWriterHistory)
	}
}

// Every generated path of testdata/second_writer_process against real
// processes, then every S transcript the walks wrote on the graph.
func TestSecondWriterProcessPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	b, err := pathsJSON("second_writer_process")
	if err != nil {
		t.Fatal(err)
	}
	a := newSecondWriterAdapter(t)
	if err := walkSecondWriter(a, b); err != nil {
		t.Fatal(err)
	}
	checkSecondWriterHistories(t, a)
}

// The projection reads two writer deaths as the update's stop of A and
// bootout of B, and a transcript where a second input opened a turn
// that was still open (two writers) is not a path in the spec.
func TestSecondWriterProcessHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(fizzCheck(t, "second_writer_process"))
	if err != nil {
		t.Fatal(err)
	}
	e := func(kind string) history.Entry { return history.Entry{Kind: kind} }
	two := []history.Entry{e("meta"), e("input"), e("done"), e("input"), e("cancelled"), e("done"),
		e("input"), e("cancelled"), e("done"), e("input"), e("done"), e("input")}
	if v := g.Check(secondWriterHistory(two)); v != nil {
		t.Fatalf("two deaths, each resumed from the web: %v", v)
	}
	cli := []history.Entry{e("meta"), e("input"), e("cancelled"), e("done")}
	if v := g.Check(secondWriterHistory(cli)); v != nil {
		t.Fatalf("a terminal's resume after A died: %v", v)
	}
	forked := []history.Entry{e("meta"), e("input"), e("input"), e("done")}
	if v := g.Check(secondWriterHistory(forked)); v == nil {
		t.Fatal("a second input into an open turn passed the trace check")
	}
}

// A run where the update SIGKILLs A instead of stopping it must fail on
// that transition, or the walk is not checking the host's state.
func TestSecondWriterProcessCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	b, err := pathsJSONCover("second_writer_process", tracecheck.CoverTransitions)
	if err != nil {
		t.Fatal(err)
	}
	// Only the walks that take the transition: the rest cannot tell.
	var f struct {
		Paths []json.RawMessage `json:"paths"`
	}
	json.Unmarshal(b, &f)
	var keep []json.RawMessage
	for _, p := range f.Paths {
		if bytes.Contains(p, []byte(`"Host#0.UpdateSignal"`)) {
			keep = append(keep, p)
			break
		}
	}
	if len(keep) == 0 {
		t.Fatal("no walk takes UpdateSignal")
	}
	b, _ = json.Marshal(map[string]any{"paths": keep})
	a := newSecondWriterAdapter(t)
	a.killUpdate = true
	err = walkSecondWriter(a, b)
	if err == nil || !strings.Contains(err.Error(), "UpdateSignal") {
		t.Fatalf("a walk whose update SIGKILLs A did not fail on UpdateSignal: %v", err)
	}
	t.Logf("caught: %v", err)
}

// A start that cannot bind loads nothing: with the port held by a live
// serve whose pidfile is gone (a stop by an older binary removed it),
// the pidfile check passes, and a start that drained its queue before
// Listen started S's task and saved meta.json under the live serve,
// then killed the child on its way out: the task was lost.
func TestSecondWriterProcessBindsBeforeItLoads(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSecondWriterAdapter(t)
	if err := a.Init(); err != nil {
		t.Fatal(err)
	}
	if err := os.Remove(filepath.Join(a.s.Home, ".bough", "serve.pid")); err != nil {
		t.Fatal(err)
	}
	before := a.metaBytes()
	p, err := a.launchServe()
	if err != nil {
		t.Fatal(err)
	}
	if !p.waitExit(actionTimeout) {
		t.Fatal("a serve that could not bind kept running")
	}
	if !bytes.Equal(before, a.metaBytes()) {
		t.Errorf("the failed start rewrote meta.json:\nbefore %s\nafter  %s", before, a.metaBytes())
	}
	if queued, err := a.taskQueued(); err != nil || !queued {
		t.Errorf("S's task after the failed start: queued=%v, %v", queued, err)
	}
	if _, err := os.Stat(a.historyPath()); err == nil {
		t.Error("the failed start ran S's task")
	}
}
