//go:build !windows

package mbt

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"reflect"
	"slices"
	"sort"
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

// specs/background_agent_launch_failure.fizz against a real serve: a
// parent session (a history file with no process, as in
// background_agents) starts children a and b over the API behind a
// running cap of 1, and the adapter breaks the machine they start on.
//
// The faults are real, not mocked:
//   - launch: serve runs from its own hard link of the bough binary, and
//     BreakLaunch renames that link away, so exec of a child fails the
//     way a deleted or replaced binary makes it fail.
//   - hang: llm-control's start hold (control.HoldStart) parks every
//     child that mounts the llm row, which is before history: a live
//     process with no history file. Repair lifts the hold.
//   - KillA/KillB: SIGKILL to the parked child's pid (start-<pid>.held).
//   - ServeRestart: SIGTERM and a start of the same binary on the same
//     HOME.

const (
	blfMaxRunning    = 1
	blfMaxPerSession = 1 << 20
)

// blfModel is the spec's state as the adapter drove it: it gates the
// actions and says what a step waits for. GetState never reports it.
type blfModel struct {
	fault, a, b   string
	aSlot, bSlot  bool
	aTask, bTask  bool
	aN, bN, total int
	answer        string
}

type blfAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	cwd  string
	exe  string // serve's own link of the binary
	gate gate
	m    blfModel

	parent, a, b string
	answer       string // what the last spawn answered
	// Held model turns by child, "" when none; names are unique across
	// walks.
	aTurn, bTurn string
	turn         int
	queued       []string // every turn queued, for Cleanup
	as           []string // every a serve accepted, for the trace check
	ran          map[string]int

	// killAsStop is the wrong adapter: KillA and KillB press Stop instead
	// of killing the child.
	killAsStop bool
}

func newBLFAdapter(t *testing.T) *blfAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	// Serve children exec serve's own executable. The shared test binary
	// must never go away under other suites, so serve runs from a link
	// of its own that BreakLaunch can rename.
	s.Shutdown()
	exe := filepath.Join(s.Root, "bough-blf")
	if err := os.Link(s.Bin(), exe); err != nil {
		t.Fatal(err)
	}
	if err := s.Resume(exe); err != nil {
		t.Fatal(err)
	}
	return &blfAdapter{t: t, s: s, dir: control.Dir(s.Home), exe: exe, cwd: s.Dir(t, "work"), ran: map[string]int{}}
}

// Init writes a fresh parent: a history file with its meta line and no
// process, so reports land in the file without waking a turn.
func (a *blfAdapter) Init() error {
	id := history.NewID()
	path := filepath.Join(a.s.Home, ".bough", "history", id+".jsonl")
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(path, nil, 0o644); err != nil {
		return err
	}
	if _, err := history.AppendFile(path, "meta", map[string]any{"cwd": a.cwd}); err != nil {
		return err
	}
	a.parent, a.a, a.b, a.aTurn, a.bTurn, a.answer = id, "", "", "", "", ""
	a.m = blfModel{fault: "none", a: "none", b: "none"}
	a.gate.reset()
	return nil
}

// Cleanup puts the machine right and lets everything the walk left run
// out: untaken turns are withdrawn and held ones released, so whatever
// still boots or runs answers at once; then every child process ends.
func (a *blfAdapter) Cleanup() error {
	var errs []error
	if _, err := os.Stat(a.exe); err != nil {
		if err := os.Rename(a.exe+".gone", a.exe); err != nil {
			errs = append(errs, err)
		}
	}
	for _, name := range a.queued {
		if os.Remove(filepath.Join(a.dir, name+".json")) == nil {
			continue
		}
		if _, err := os.Stat(filepath.Join(a.dir, name+".taken")); err == nil {
			control.Release(a.t, a.dir, name)
		}
	}
	a.queued, a.aTurn, a.bTurn = nil, "", ""
	control.ReleaseStart(a.t, a.dir)
	if err := a.waitAgents("the walk's children to settle", func(c serve.AgentCount) bool { return c.Running == 0 && c.Queued == 0 }); err != nil {
		errs = append(errs, err)
	}
	for _, id := range []string{a.a, a.b} {
		if id == "" {
			continue
		}
		if err := a.end(id); err != nil {
			errs = append(errs, err)
		}
	}
	a.heldPIDs() // drops the markers of killed children
	return errors.Join(errs...)
}

func (a *blfAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Parent", Index: 0}: a}, nil
}

// GetState reads the Parent role off the machine and the API:
//   - fault: whether serve's executable is there and the start hold is set.
//   - a, b: the children listing (the Work dialog's rows), with "booting"
//     for a live child whose history has no turn yet; serve's "error" is
//     the spec's "failed".
//   - *_slot: the parent row's running count, credited to the child that
//     is booting or running, else (a leak) to a child that holds none.
//   - *_task: meta.json's Task, what a restart would start again.
//   - *_n: the reports in the parent's history file.
//   - total: the parent row's agent total; answer: the last spawn's.
func (a *blfAdapter) GetState() (map[string]any, error) {
	rows, err := a.children()
	if err != nil {
		return nil, err
	}
	count, err := a.agents()
	if err != nil {
		return nil, err
	}
	meta, err := a.meta()
	if err != nil {
		return nil, err
	}
	fault := "none"
	if _, err := os.Stat(a.exe); err != nil {
		fault = "launch"
	} else if _, err := os.Stat(filepath.Join(a.dir, "start.hold")); err == nil {
		fault = "hang"
	}
	// A child whose spawn answered an error has no id here; any row the
	// adapter did not name is it (a ghost the spawn left behind).
	aID := a.a
	if aID == "" {
		for _, r := range rows {
			if r.ID != a.b {
				aID = r.ID
			}
		}
	}
	st := map[string]any{"fault": fault, "total": count.Total, "answer": a.answer}
	status := map[string]string{"a": a.childStatus(rows, aID), "b": a.childStatus(rows, a.b)}
	left := count.Running
	slot := map[string]bool{}
	for _, k := range []string{"a", "b"} {
		if left > 0 && (status[k] == "booting" || status[k] == "running") {
			slot[k], left = true, left-1
		}
	}
	for _, k := range []string{"a", "b"} {
		if left > 0 && !slot[k] && status[k] != "none" && status[k] != "queued" {
			slot[k], left = true, left-1
		}
	}
	for k, id := range map[string]string{"a": aID, "b": a.b} {
		st[k] = status[k]
		st[k+"_slot"] = slot[k]
		st[k+"_task"] = id != "" && meta[id].Task != nil
		st[k+"_n"] = a.notices(id)
	}
	return st, nil
}

// childStatus is id's status as the spec names it.
func (a *blfAdapter) childStatus(rows []serve.Row, id string) string {
	if id == "" {
		return "none"
	}
	for _, r := range rows {
		if r.ID != id {
			continue
		}
		if r.Status == serve.StatusQueued {
			return "queued"
		}
		if r.Live && !blfHasTurn(a.entries(id)) {
			return "booting"
		}
		if r.Status == serve.StatusError {
			return "failed"
		}
		return string(r.Status)
	}
	return "none"
}

// hasTurn says whether a transcript got as far as a turn.
func blfHasTurn(entries []history.Entry) bool {
	return slices.ContainsFunc(entries, func(e history.Entry) bool { return e.Kind == "input" })
}

// --- the spec's transitions, for the gate and the waits ---

func blfStarted(fault string) string {
	switch fault {
	case "launch":
		return "failed"
	case "hang":
		return "booting"
	}
	return "running"
}

// drain is the spec's drain: a freed slot starts the queue, a first.
func (m *blfModel) drain() {
	busy := m.aSlot || m.bSlot
	for _, c := range []struct {
		st         *string
		slot, task *bool
		n          *int
	}{{&m.a, &m.aSlot, &m.aTask, &m.aN}, {&m.b, &m.bSlot, &m.bTask, &m.bN}} {
		if *c.st != "queued" || busy {
			continue
		}
		*c.st = blfStarted(m.fault)
		switch *c.st {
		case "failed":
			*c.task = false
			*c.n++
		case "running":
			*c.slot, *c.task, busy = true, false, true
		default:
			*c.slot, busy = true, true
		}
	}
}

func blfPending(s string) bool { return s == "queued" || s == "booting" || s == "running" }

// --- actions ---

func (a *blfAdapter) BreakLaunch() error {
	if !a.gate.pass(a.m.fault == "none") {
		return nil
	}
	a.m.fault = "launch"
	return os.Rename(a.exe, a.exe+".gone")
}

func (a *blfAdapter) BreakBoot() error {
	if !a.gate.pass(a.m.fault == "none") {
		return nil
	}
	a.m.fault = "hang"
	control.HoldStart(a.t, a.dir)
	return nil
}

// Repair fixes the machine; a parked child goes on and opens its turn,
// which is held so it stays running.
func (a *blfAdapter) Repair() error {
	if !a.gate.pass(a.m.fault != "none") {
		return nil
	}
	if a.m.fault == "launch" {
		a.m.fault = "none"
		if err := os.Rename(a.exe+".gone", a.exe); err != nil {
			return err
		}
		return a.settle()
	}
	a.m.fault = "none"
	if a.m.a == "booting" {
		a.aTurn = a.queue()
		a.m.a, a.m.aTask = "running", false
	}
	if a.m.b == "booting" {
		a.bTurn = a.queue()
		a.m.b, a.m.bTask = "running", false
	}
	control.ReleaseStart(a.t, a.dir)
	return a.settle()
}

// SpawnA is tools.spawn({background}) with the cap free.
func (a *blfAdapter) SpawnA() error {
	if !a.gate.pass(a.m.a == "none" && a.m.b == "none") {
		return nil
	}
	st := blfStarted(a.m.fault)
	name := ""
	if st == "running" {
		name = a.queue()
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, queued, err := a.s.CreateAgent(ctx, a.parent, "task a", blfMaxRunning, blfMaxPerSession)
	var apiErr *servetest.APIError
	switch {
	case errors.As(err, &apiErr) && apiErr.Status/100 == 5:
		a.answer = "error"
	case err != nil:
		return err
	case queued:
		return fmt.Errorf("a was queued with the cap free")
	default:
		a.answer = "ok"
		a.a, a.aTurn = row.ID, name
		a.as = append(a.as, row.ID)
	}
	if st == "failed" {
		a.m.answer = "error"
	} else {
		a.m.answer = "ok"
		a.m.total++
		a.m.a, a.m.aSlot, a.m.aTask = st, true, st == "booting"
	}
	return a.settle()
}

// SpawnB is a second spawn while a holds the slot: it queues.
func (a *blfAdapter) SpawnB() error {
	if !a.gate.pass(a.m.b == "none" && (a.m.a == "booting" || a.m.a == "running")) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, queued, err := a.s.CreateAgent(ctx, a.parent, "task b", blfMaxRunning, blfMaxPerSession)
	if err != nil {
		return err
	}
	if !queued {
		return fmt.Errorf("b started past a running cap of 1")
	}
	a.b, a.answer = row.ID, "ok"
	a.m.answer, a.m.b, a.m.bTask = "ok", "queued", true
	a.m.total++
	return a.settle()
}

func (a *blfAdapter) FinishA() error {
	if !a.gate.pass(a.m.a == "running") {
		return nil
	}
	a.m.a, a.m.aSlot = "done", false
	a.m.aN++
	a.drainNext()
	control.Release(a.t, a.dir, a.aTurn)
	a.aTurn = ""
	return a.settle()
}

func (a *blfAdapter) FinishB() error {
	if !a.gate.pass(a.m.b == "running") {
		return nil
	}
	a.m.b, a.m.bSlot = "done", false
	a.m.bN++
	a.drainNext()
	control.Release(a.t, a.dir, a.bTurn)
	a.bTurn = ""
	return a.settle()
}

// KillA is the booting child dying before any history: SIGKILL.
func (a *blfAdapter) KillA() error {
	if !a.gate.pass(a.m.a == "booting") {
		return nil
	}
	a.m.a, a.m.aSlot, a.m.aTask = "failed", false, false
	a.m.aN++
	return a.kill(a.a)
}

func (a *blfAdapter) KillB() error {
	if !a.gate.pass(a.m.b == "booting") {
		return nil
	}
	a.m.b, a.m.bSlot, a.m.bTask = "failed", false, false
	a.m.bN++
	return a.kill(a.b)
}

func (a *blfAdapter) kill(id string) error {
	a.drainNext()
	if a.killAsStop {
		ctx, cancel := actionCtx()
		defer cancel()
		if _, err := a.s.Stop(ctx, id); err != nil {
			return err
		}
		return a.settle()
	}
	pids, err := a.waitHeld()
	if err != nil {
		return err
	}
	for _, pid := range pids {
		if err := syscall.Kill(pid, syscall.SIGKILL); err != nil {
			return fmt.Errorf("kill booting child %d: %w", pid, err)
		}
	}
	return a.settle()
}

// StopA is the Work dialog's Stop agent.
func (a *blfAdapter) StopA() error {
	if !a.gate.pass(blfPending(a.m.a)) {
		return nil
	}
	if a.m.a == "queued" {
		a.m.a, a.m.aTask = "none", false
		a.m.total--
		return a.stop(a.a, "queued")
	}
	a.m.a, a.m.aSlot, a.m.aTask = "stopped", false, false
	a.m.aN++
	a.aTurn = "" // the interrupt cancels a held model call
	return a.stop(a.a, "running")
}

func (a *blfAdapter) StopB() error {
	if !a.gate.pass(blfPending(a.m.b)) {
		return nil
	}
	if a.m.b == "queued" {
		a.m.b, a.m.bTask = "none", false
		a.m.total--
		return a.stop(a.b, "queued")
	}
	a.m.b, a.m.bSlot, a.m.bTask = "stopped", false, false
	a.m.bN++
	a.bTurn = ""
	return a.stop(a.b, "running")
}

func (a *blfAdapter) stop(id, want string) error {
	if want != "queued" {
		a.drainNext()
	}
	ctx, cancel := actionCtx()
	defer cancel()
	was, err := a.s.Stop(ctx, id)
	if err != nil {
		return err
	}
	if was != want {
		return fmt.Errorf("stop of %s answered %q, want %q", id, was, want)
	}
	return a.settle()
}

// ServeRestart is serve going down and coming back with no turn open.
// The start runs the same link, so a launch fault is lifted only for
// the exec of serve itself.
func (a *blfAdapter) ServeRestart() error {
	if !a.gate.pass(a.m.a != "running" && a.m.b != "running") {
		return nil
	}
	if a.m.a == "booting" {
		a.m.a, a.m.aSlot = "queued", false
	}
	if a.m.b == "booting" {
		a.m.b, a.m.bSlot = "queued", false
	}
	a.drainNext()
	a.s.Shutdown()
	a.heldPIDs()
	if a.m.fault == "launch" {
		if err := os.Rename(a.exe+".gone", a.exe); err != nil {
			return err
		}
	}
	err := a.s.Resume(a.exe)
	if a.m.fault == "launch" {
		if rerr := os.Rename(a.exe, a.exe+".gone"); rerr != nil && err == nil {
			err = rerr
		}
	}
	if err != nil {
		return err
	}
	return a.settle()
}

// drainNext applies the spec's drain to the model and queues the held
// turn of a child it starts on a sound machine, before the step that
// frees the slot: the drain starts its model call at once.
func (a *blfAdapter) drainNext() {
	before := a.m
	a.m.drain()
	if before.a == "queued" && a.m.a == "running" {
		a.aTurn = a.queue()
	}
	if before.b == "queued" && a.m.b == "running" {
		a.bTurn = a.queue()
	}
}

// --- plumbing ---

// settle waits for the machine to reach the state the model says the
// step leads to. It never fails the step itself: the state check after
// it compares with the graph, which is the judge.
func (a *blfAdapter) settle() error {
	want := map[string]any{
		"fault": a.m.fault, "a": a.m.a, "b": a.m.b, "a_slot": a.m.aSlot, "b_slot": a.m.bSlot,
		"a_task": a.m.aTask, "b_task": a.m.bTask, "a_n": a.m.aN, "b_n": a.m.bN, "total": a.m.total, "answer": a.m.answer,
	}
	deadline := time.Now().Add(10 * time.Second)
	for {
		got, err := a.GetState()
		if err == nil && reflect.DeepEqual(got, want) {
			break
		}
		if time.Now().After(deadline) {
			return nil
		}
		time.Sleep(50 * time.Millisecond)
	}
	// A late second report or a leaked slot lands within a poll or two.
	time.Sleep(300 * time.Millisecond)
	return nil
}

func (a *blfAdapter) queue() string {
	a.turn++
	name := fmt.Sprintf("l%05d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	a.queued = append(a.queued, name)
	return name
}

// heldPIDs are the live children parked at the start hold; markers of
// dead ones (a SIGKILL skips the child's own cleanup) are removed.
func (a *blfAdapter) heldPIDs() []int {
	files, _ := filepath.Glob(filepath.Join(a.dir, "start-*.held"))
	var pids []int
	for _, f := range files {
		var pid int
		if _, err := fmt.Sscanf(filepath.Base(f), "start-%d.held", &pid); err != nil {
			continue
		}
		if syscall.Kill(pid, 0) != nil {
			os.Remove(f)
			continue
		}
		pids = append(pids, pid)
	}
	sort.Ints(pids)
	return pids
}

func (a *blfAdapter) waitHeld() ([]int, error) {
	deadline := time.Now().Add(actionTimeout)
	for {
		if pids := a.heldPIDs(); len(pids) > 0 {
			return pids, nil
		}
		if time.Now().After(deadline) {
			return nil, errors.New("no child parked at the start hold")
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// end SIGINTs a child's idle process and waits for it to go.
func (a *blfAdapter) end(id string) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		rows, err := a.children()
		if err != nil {
			return err
		}
		live := false
		for _, r := range rows {
			live = live || (r.ID == id && r.Live)
		}
		if !live {
			return nil
		}
		ctx, cancel := actionCtx()
		err = a.s.Interrupt(ctx, id)
		cancel()
		var apiErr *servetest.APIError
		if err != nil && !(errors.As(err, &apiErr) && apiErr.Status == http.StatusNotFound) {
			return err
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("child %s still live after %s", id, actionTimeout)
		}
		time.Sleep(200 * time.Millisecond)
	}
}

func (a *blfAdapter) children() ([]serve.Row, error) {
	var r struct {
		Children []serve.Row `json:"children"`
	}
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, a.s.URL+"/api/sessions/"+a.parent+"/children", nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("children of %s answered %d", a.parent, resp.StatusCode)
	}
	err = json.NewDecoder(resp.Body).Decode(&r)
	return r.Children, err
}

func (a *blfAdapter) agents() (serve.AgentCount, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.parent)
	if err != nil || row.Agents == nil {
		return serve.AgentCount{}, err
	}
	return *row.Agents, nil
}

func (a *blfAdapter) waitAgents(what string, ok func(serve.AgentCount) bool) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		c, err := a.agents()
		if err == nil && ok(c) {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for %s: agents %+v, last error %v", what, c, err)
		}
		time.Sleep(50 * time.Millisecond)
	}
}

func (a *blfAdapter) meta() (map[string]serve.SessionMeta, error) {
	b, err := os.ReadFile(filepath.Join(a.s.Home, ".bough", "serve", "meta.json"))
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	var f struct {
		Sessions map[string]serve.SessionMeta `json:"sessions"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		return nil, err
	}
	return f.Sessions, nil
}

// notices counts the reports from id in the parent's file.
func (a *blfAdapter) notices(id string) int {
	if id == "" {
		return 0
	}
	n := 0
	for _, e := range a.entries(a.parent) {
		if e.Kind == "notice" && e.Data["from"] == id {
			n++
		}
	}
	return n
}

func (a *blfAdapter) entries(id string) []history.Entry {
	entries, _ := history.Read(filepath.Join(a.s.Home, ".bough", "history", id+".jsonl"))
	return entries
}

func blfAction(name string, f func(*blfAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*blfAdapter)
		err := f(a)
		if !a.gate.off {
			a.ran[name]++
		}
		return nil, err
	}
}

var backgroundAgentLaunchFailureActions = map[string]map[string]fmbt.ActionFunc{"Parent": {
	"BreakLaunch":  blfAction("BreakLaunch", (*blfAdapter).BreakLaunch),
	"BreakBoot":    blfAction("BreakBoot", (*blfAdapter).BreakBoot),
	"Repair":       blfAction("Repair", (*blfAdapter).Repair),
	"SpawnA":       blfAction("SpawnA", (*blfAdapter).SpawnA),
	"SpawnB":       blfAction("SpawnB", (*blfAdapter).SpawnB),
	"FinishA":      blfAction("FinishA", (*blfAdapter).FinishA),
	"FinishB":      blfAction("FinishB", (*blfAdapter).FinishB),
	"KillA":        blfAction("KillA", (*blfAdapter).KillA),
	"KillB":        blfAction("KillB", (*blfAdapter).KillB),
	"StopA":        blfAction("StopA", (*blfAdapter).StopA),
	"StopB":        blfAction("StopB", (*blfAdapter).StopB),
	"ServeRestart": blfAction("ServeRestart", (*blfAdapter).ServeRestart),
}}

func backgroundAgentLaunchFailureOptions() map[string]any {
	return map[string]any{"max-seq-runs": 150, "max-actions": 8, "max-parallel-runs": 0}
}

// backgroundAgentLaunchFailureHistory reads child a's own transcript:
// its first input is the spawn that started it (whatever boot it went
// through first, the path from Init to a running a is SpawnA), its turn
// closes as FinishA or, cancelled, StopA. a's history is only ever
// written by a child that got past its start, so failed starts leave
// nothing to replay; the parent's side is the path walk's.
func backgroundAgentLaunchFailureHistory(entries []history.Entry) []tracecheck.Step {
	state := func(s string) map[string]any { return map[string]any{"Parent#0.a": s} }
	steps := []tracecheck.Step{{Action: "Init", State: state("none")}}
	open, lastClose := false, ""
	for _, e := range entries {
		switch e.Kind {
		case "input":
			// A second input would be a message; this flow sends none,
			// and the check must say so.
			steps = append(steps, tracecheck.Step{Action: "Parent#0.SpawnA", State: state("running")})
			open, lastClose = true, ""
		case "done", "cancelled":
			if !open || (e.Kind == "done" && lastClose == "cancelled") {
				continue
			}
			open, lastClose = false, e.Kind
			if e.Kind == "cancelled" {
				steps = append(steps, tracecheck.Step{Action: "Parent#0.StopA", State: state("stopped")})
			} else {
				steps = append(steps, tracecheck.Step{Action: "Parent#0.FinishA", State: state("done")})
			}
		}
	}
	return steps
}

func init() {
	historyProjections["background_agent_launch_failure"] = backgroundAgentLaunchFailureHistory
}

func TestBackgroundAgentLaunchFailure(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newBLFAdapter(t)
	if err := runMBT(t, "background_agent_launch_failure", a, backgroundAgentLaunchFailureActions, backgroundAgentLaunchFailureOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("steps run: %v", a.ran)
	checkBLFHistories(t, a)
}

// Every generated walk against the real serve: each action must be
// enabled in the adapter's view where the graph enables it, and the
// state after it must be the graph's.
func TestBackgroundAgentLaunchFailurePaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newBLFAdapter(t)
	if err := walkBLFPaths(t, a, envCover()); err != nil {
		t.Fatal(err)
	}
	t.Logf("steps run: %v", a.ran)
	checkBLFHistories(t, a)
}

// A booting child killed by SIGKILL could not start; one the person
// stopped was stopped. An adapter that presses Stop for KillA must fail
// the walk, which it can only do on the Kill transitions: every link.
func TestBackgroundAgentLaunchFailurePathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newBLFAdapter(t)
	a.killAsStop = true
	err := walkBLFPaths(t, a, tracecheck.CoverTransitions)
	if err == nil {
		t.Fatal("an adapter whose Kill presses Stop walked every transition; the walk is not checking state")
	}
	t.Logf("caught: %v", err)
}

// The projection reads a finished a as a path, and a transcript with a
// second turn (a message this flow never sends) is not one.
func TestBackgroundAgentLaunchFailureHistoryProjection(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	g, err := tracecheck.Load(fizzCheck(t, "background_agent_launch_failure"))
	if err != nil {
		t.Fatal(err)
	}
	e := func(kind string) history.Entry { return history.Entry{Kind: kind} }
	ok := [][]history.Entry{
		{e("meta"), e("input"), e("done")},
		{e("meta"), e("input"), e("cancelled"), e("done")},
		{e("meta"), e("input")},
	}
	for _, entries := range ok {
		if v := g.Check(backgroundAgentLaunchFailureHistory(entries)); v != nil {
			t.Errorf("%v: %v", entries, v)
		}
	}
	twice := []history.Entry{e("meta"), e("input"), e("done"), e("input"), e("done")}
	if v := g.Check(backgroundAgentLaunchFailureHistory(twice)); v == nil {
		t.Fatal("a child with a second turn passed the trace check")
	}
}

func checkBLFHistories(t *testing.T, a *blfAdapter) {
	t.Helper()
	g, err := tracecheck.Load(fizzCheck(t, "background_agent_launch_failure"))
	if err != nil {
		t.Fatal(err)
	}
	n := 0
	for _, id := range a.as {
		entries := a.entries(id)
		if !blfHasTurn(entries) {
			continue // never started a turn: nothing of it to replay
		}
		checkHistory(t, g, entries, backgroundAgentLaunchFailureHistory)
		n++
	}
	if n == 0 {
		t.Fatal("no child a ran a turn; the trace check checked nothing")
	}
	t.Logf("trace-checked %d transcripts of a", n)
}

func walkBLFPaths(t *testing.T, a *blfAdapter, cover tracecheck.Cover) error {
	t.Helper()
	b, err := pathsJSONCover("background_agent_launch_failure", cover)
	if err != nil {
		return err
	}
	var file struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &file); err != nil {
		return err
	}
	steps := 0
	for _, p := range file.Paths {
		steps += len(p.Trace)
	}
	t.Logf("%s: %d walks, %d steps", cover, len(file.Paths), steps)
	for pi, p := range file.Paths {
		for si, step := range p.Trace {
			if si == 0 {
				if err := a.Init(); err != nil {
					return fmt.Errorf("path %d init: %w", pi, err)
				}
			} else {
				name := strings.TrimPrefix(step.Action, "Parent#0.")
				f, ok := backgroundAgentLaunchFailureActions["Parent"][name]
				if !ok {
					return fmt.Errorf("path %d step %d: no adapter action for %q", pi, si, step.Action)
				}
				if _, err := f(a, nil); err != nil {
					return fmt.Errorf("path %d step %d (%s): %w", pi, si, name, err)
				}
				if a.gate.off {
					return fmt.Errorf("path %d step %d: %s is enabled in the model but not in the adapter's view", pi, si, name)
				}
			}
			got, err := a.GetState()
			if err != nil {
				return fmt.Errorf("path %d step %d: %w", pi, si, err)
			}
			if d := blfDiff(step.State, got); d != "" {
				var walked []string
				for _, s := range p.Trace[1 : si+1] {
					walked = append(walked, strings.TrimPrefix(s.Action, "Parent#0."))
				}
				return fmt.Errorf("path %d step %d (%s): %s\nwalked: %s", pi, si, step.Action, d, strings.Join(walked, " "))
			}
		}
		if err := a.Cleanup(); err != nil {
			return fmt.Errorf("path %d cleanup: %w", pi, err)
		}
	}
	return nil
}

// blfDiff compares the spec's Parent#0 fields with the adapter's state,
// through JSON so ints match the graph's numbers.
func blfDiff(want, got map[string]any) string {
	gb, _ := json.Marshal(got)
	var have map[string]any
	json.Unmarshal(gb, &have)
	var d []string
	for k, v := range want {
		field, ok := strings.CutPrefix(k, "Parent#0.")
		if !ok {
			continue
		}
		if !reflect.DeepEqual(have[field], v) {
			d = append(d, fmt.Sprintf("%s: spec %v, serve %v", field, v, have[field]))
		}
	}
	sort.Strings(d)
	return strings.Join(d, "; ")
}
