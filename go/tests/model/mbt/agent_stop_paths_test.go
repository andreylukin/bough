//go:build !windows

package mbt

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/agent_stop_paths.fizz against a real serve: a parent with no
// process (as in background_agents) starts A and then Q over the API
// with a running cap of 1. A's turns are llm-control turns; a turn
// that "adopts" answers with a bash call that waits on a flag file, so
// it outlives turn_settle, becomes job 1 and the turn closes with
// done{running:1}. JobEnds writes the flag.
//
// The Work dialog's stop store is the page's, not serve's: the adapter
// keeps it the way useStopStore promises to (an entry lives while its
// row says running or queued), holds the POST itself so "Stopping" is a
// state (ClickStop records it, StopLands sends it, StopFails never
// does), and everything else is read off the server: the rows and jobs
// from the parent's children listing, the running count from the
// parent row, A's turns from its history and the reports from the
// parent's.

// asConfig is llm-control plus an engine whose turns adopt a call that
// is still running 300 ms after the model went quiet.
const asConfig = controlConfig + "- id: loop\n  plugin: engine-unreal\n  config:\n    turn_settle: 300ms\n"

// asWait bounds a wait for the server to catch up with a step.
const asWait = 15 * time.Second

type asAdapter struct {
	t     *testing.T
	s     *servetest.Server
	ba    *baAdapter // the API helpers: children, agents, stop, call
	dir   string     // llm-control's queue
	flags string
	gate  gate

	parent, a, q string
	// aHeld and qHeld are the held turns (llm-control names) of A and Q.
	aHeld, qHeld string
	// flag is the file A's running job waits on, "" with no job.
	flag string
	// resultOwed: A's job ended inside an open turn, so the turn's
	// release makes one more model request to show the model its result.
	resultOwed   bool
	msgs, adopts int
	// The page's stop store and held POSTs.
	aStop, qStop string
	aReq, qReq   bool

	turn   int
	flagN  int
	as     []string // every A, for the trace check
	traces map[string][]history.Entry
	stats  map[string]int

	// finishAsError is TestAgentStopPathsCatchesWrongAdapter's bug:
	// FinishA fails the turn instead of finishing it.
	finishAsError bool
}

func newASAdapter(t *testing.T) *asAdapter {
	s := servetest.Start(t, servetest.Options{Config: asConfig})
	dir := control.Dir(s.Home)
	return &asAdapter{
		t: t, s: s, dir: dir, flags: s.Dir(t, "flags"), stats: map[string]int{}, traces: map[string][]history.Entry{},
		ba: &baAdapter{t: t, s: s, dir: dir, cwd: s.Dir(t, "work"), ran: map[string]int{}},
	}
}

func (a *asAdapter) Init() error {
	if err := a.ba.Init(); err != nil {
		return err
	}
	a.parent = a.ba.parent
	a.a, a.q, a.aHeld, a.qHeld, a.flag = "", "", "", "", ""
	a.resultOwed = false
	a.msgs, a.adopts = 0, 0
	a.aStop, a.qStop, a.aReq, a.qReq = "", "", false, false
	a.gate.reset()
	return nil
}

// Cleanup ends both children and every job, and waits until serve
// counts nothing running, so the next walk starts with the slot free.
func (a *asAdapter) Cleanup() error {
	// Teardown releases jobs and then kills their owner. A wake can be
	// interrupted before its input lands; that race is outside the walk.
	if a.a != "" && a.traces[a.a] == nil {
		a.traces[a.a] = sessionHistory(a.t, a.s.Home, a.a)
	}
	var errs []error
	if a.q != "" {
		if rows, err := a.ba.children(a.parent); err == nil && statusIn(rows, a.q) == string(serve.StatusQueued) {
			if _, err := a.ba.stop(a.q); err != nil {
				errs = append(errs, err)
			}
		}
	}
	// Jobs first: their bash loops outlive a killed child otherwise.
	for i := 1; i <= a.flagN; i++ {
		_ = os.WriteFile(filepath.Join(a.flags, fmt.Sprintf("j%d", i)), nil, 0o644)
	}
	a.unqueueAll()
	for _, id := range []string{a.a, a.q} {
		if id == "" {
			continue
		}
		if err := a.kill(id); err != nil {
			errs = append(errs, err)
		}
	}
	a.unqueueAll()
	a.aHeld, a.qHeld = "", ""
	if err := a.ba.waitParent("no child holding a slot", func(c serve.AgentCount) bool { return c.Running == 0 }); err != nil {
		errs = append(errs, err)
	}
	return errors.Join(errs...)
}

// kill SIGINTs a live child until its process is gone: the first
// cancels an open turn, the next ends the idle process.
func (a *asAdapter) kill(id string) error {
	deadline := time.Now().Add(asWait)
	for {
		row, err := a.ba.waitChild(id, "a row", func(*serve.Row) bool { return true })
		if err != nil || row == nil || !row.Live {
			return err
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("child %s still live after %s of interrupts", id, asWait)
		}
		ctx, cancel := actionCtx()
		err = a.s.Interrupt(ctx, id)
		cancel()
		var apiErr *servetest.APIError
		if err != nil && (!errors.As(err, &apiErr) || apiErr.Status != 404) {
			return err
		}
		time.Sleep(300 * time.Millisecond)
	}
}

// unqueueAll takes back every turn nobody took, so the next walk's
// first request cannot answer with it.
func (a *asAdapter) unqueueAll() {
	files, _ := filepath.Glob(filepath.Join(a.dir, "*.json"))
	for _, f := range files {
		_ = os.Remove(f)
	}
}

func (a *asAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Parent", Index: 0}: a}, nil
}

// asObs is what the server says, before it is folded into the spec's
// fields.
type asObs struct {
	aRow, qRow *serve.Row
	running    int
	entries    []history.Entry // A's
	notices    []history.Entry // the parent's, from A
}

func (a *asAdapter) observe() (asObs, error) {
	var o asObs
	rows, err := a.ba.children(a.parent)
	if err != nil {
		return o, err
	}
	for i := range rows {
		switch rows[i].ID {
		case "":
		case a.a:
			o.aRow = &rows[i]
		case a.q:
			o.qRow = &rows[i]
		}
	}
	c, err := a.ba.agents()
	if err != nil {
		return o, err
	}
	o.running = c.Running
	if a.a != "" {
		o.entries = a.ba.entries(a.a)
		for _, e := range a.ba.entries(a.parent) {
			if e.Kind == "notice" && e.Data["from"] == a.a {
				o.notices = append(o.notices, e)
			}
		}
	}
	return o, nil
}

// asTurns reads A's history the way the spec counts it: an input that
// is not a steer opens a turn (spawn, message, wake); a done or
// cancelled closes it, and the done a cancel is followed by is
// bookkeeping. A done with calls still running is an interim close,
// not one the parent is owed a report for.
type asTurns struct {
	last          string // none | open | done | stopped
	turns, finals int
	openAt        time.Time
}

func readASTurns(entries []history.Entry) asTurns {
	t := asTurns{last: "none"}
	lastClose := ""
	for _, e := range entries {
		switch e.Kind {
		case "input":
			if e.Data["steer"] == true {
				continue
			}
			t.turns++
			t.last, t.openAt, lastClose = "open", e.At, ""
		case "done", "cancelled":
			if e.Kind == "done" && lastClose == "cancelled" && t.last != "open" {
				continue
			}
			if e.Kind == "cancelled" {
				t.last = "stopped"
				t.finals++
			} else {
				t.last = "done"
				if asNum(e.Data["running"]) == 0 {
					t.finals++
				}
			}
			lastClose = e.Kind
		}
	}
	return t
}

var asReportWord = regexp.MustCompile(`^\[agent .* · \S+ (\w+)\]`)

// rowLife is the Work dialog's life for a child row (work.ts
// AGENT_LIFE): what the row says and whether it offers Stop.
func rowLife(r *serve.Row) string {
	if r == nil {
		return "none"
	}
	switch r.Status {
	case serve.StatusRunning, serve.StatusNeedsYou:
		return "running"
	case serve.StatusDone:
		return "finished"
	case serve.StatusStopped, serve.StatusInterrupted:
		return "stopped"
	case serve.StatusIdle:
		if r.Live {
			return "running"
		}
		return "finished"
	case serve.StatusQueued:
		return "queued"
	}
	return string(r.Status)
}

func (a *asAdapter) GetState() (map[string]any, error) {
	o, err := a.observe()
	if err != nil {
		return nil, err
	}
	return a.state(o), nil
}

func (a *asAdapter) state(o asObs) map[string]any {
	t := readASTurns(o.entries)
	aRow := rowLife(o.aRow)
	if a.a == "" {
		aRow = "none"
	}
	q := "none"
	if a.q != "" {
		q = "gone"
		if o.qRow != nil {
			q = rowLife(o.qRow)
			if q == "finished" {
				q = "done"
			}
		}
	}
	// The page's store: an entry ends once its row stops offering Stop.
	if aRow != "running" {
		a.aStop = ""
	}
	if q != "queued" && q != "running" {
		a.qStop = ""
	}
	// The running count is the parent's: Q's share is its own open turn.
	qHolds := 0
	if q == "running" {
		qHolds = 1
	}
	word, shown := "", 0
	if n := len(o.notices); n > 0 {
		last := o.notices[n-1]
		if m := asReportWord.FindStringSubmatch(str(last.Data["text"])); m != nil {
			word = m[1]
		}
		if t.last != "open" && last.At.After(t.openAt) {
			shown = t.turns
		}
	}
	return map[string]any{
		"a": t.last, "a_job": o.aRow != nil && len(o.aRow.Jobs) > 0,
		"a_slot": a.a != "" && o.running-qHolds > 0, "a_row": aRow,
		"a_stop": a.aStop, "a_req": a.aReq,
		"a_turns": t.turns, "a_shown": shown,
		"a_notices": len(o.notices), "a_finals": t.finals, "a_word": word,
		"a_msgs": a.msgs, "a_adopts": a.adopts,
		"q": q, "q_stop": a.qStop, "q_req": a.qReq,
	}
}

// view is the state the gate reads the spec's requires off.
func (a *asAdapter) view() map[string]any {
	st, err := a.GetState()
	if err != nil {
		a.t.Logf("agent_stop_paths: state: %v", err)
		return map[string]any{}
	}
	return st
}

// waitState polls until ok holds for the state read off the server.
func (a *asAdapter) waitState(what string, ok func(map[string]any) bool) error {
	deadline := time.Now().Add(asWait)
	var last map[string]any
	for {
		st, err := a.GetState()
		if err == nil {
			last = st
			if ok(st) {
				return nil
			}
		}
		if time.Now().After(deadline) {
			b, _ := json.Marshal(last)
			return fmt.Errorf("waiting for %s: last state %s, last error %v", what, b, err)
		}
		time.Sleep(50 * time.Millisecond)
	}
}

// queue puts a turn on llm-control's queue. Names sort in the order
// they are queued, and a step queues its turns in the order their
// requests are made, so each request takes the turn meant for it.
func (a *asAdapter) queue(mode string) string {
	a.turn++
	name := fmt.Sprintf("s%05d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: mode, Text: "finished " + name})
	return name
}

func (a *asAdapter) taken(name string) bool {
	_, err := os.Stat(filepath.Join(a.dir, name+".taken"))
	return err == nil
}

func (a *asAdapter) waitTaken(name string) error {
	deadline := time.Now().Add(asWait)
	for !a.taken(name) {
		if time.Now().After(deadline) {
			return fmt.Errorf("llm-control: turn %s not taken after %s", name, asWait)
		}
		time.Sleep(10 * time.Millisecond)
	}
	return nil
}

// drainTurn queues the model turn of Q for a step that may start it
// (a freed slot drains the queue); settleDrain then waits for it to be
// taken if serve drained Q, or takes it back if not.
func (a *asAdapter) drainTurn(v map[string]any) string {
	if v["q"] != "queued" {
		return ""
	}
	return a.queue("block")
}

func (a *asAdapter) settleDrain(name string) error {
	if name == "" {
		return nil
	}
	// Serve drains in the same step the slot frees, and Q's model call
	// follows its boot: give it that long before calling it not drained.
	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		if a.taken(name) {
			a.qHeld = name
			return a.waitState("Q running", func(st map[string]any) bool { return st["q"] == "running" })
		}
		rows, err := a.ba.children(a.parent)
		if err == nil && statusIn(rows, a.q) == string(serve.StatusQueued) {
			if _, running := a.running(); !running {
				// Still queued with the slot held: not drained.
				break
			}
		}
		time.Sleep(50 * time.Millisecond)
	}
	if os.Remove(filepath.Join(a.dir, name+".json")) == nil {
		return nil
	}
	a.qHeld = name
	return a.waitState("Q running", func(st map[string]any) bool { return st["q"] == "running" })
}

// running is the parent's running count and whether it is above zero.
func (a *asAdapter) running() (int, bool) {
	c, err := a.ba.agents()
	if err != nil {
		return 0, true
	}
	return c.Running, c.Running > 0
}

// quiet lets a report or a drain that should not happen have the time
// to (reports poll every 50 ms and give up after 2 s).
func quiet() { time.Sleep(400 * time.Millisecond) }

func (a *asAdapter) SpawnA() error {
	if !a.gate.pass(a.view()["a"] == "none") {
		return nil
	}
	name := a.queue("block")
	status, reply, err := a.ba.spawn(a.parent, "task "+name)
	if err != nil {
		return err
	}
	if status != 201 || reply.Queued {
		return fmt.Errorf("spawn of A answered %d queued=%v", status, reply.Queued)
	}
	a.a, a.aHeld = reply.Session.ID, name
	a.as = append(a.as, a.a)
	if err := a.waitTaken(name); err != nil {
		return err
	}
	return a.waitState("A running", func(st map[string]any) bool { return st["a"] == "open" && st["a_row"] == "running" })
}

func (a *asAdapter) SpawnQ() error {
	v := a.view()
	if !a.gate.pass(v["q"] == "none" && v["a_slot"] == true) {
		return nil
	}
	status, reply, err := a.ba.spawn(a.parent, "task q")
	if err != nil {
		return err
	}
	if status != 201 || !reply.Queued {
		return fmt.Errorf("spawn of Q answered %d queued=%v", status, reply.Queued)
	}
	a.q = reply.Session.ID
	return a.waitState("Q queued", func(st map[string]any) bool { return st["q"] == "queued" })
}

// FinishA releases A's held turn with a reply.
func (a *asAdapter) FinishA() error {
	v := a.view()
	if !a.gate.pass(v["a"] == "open") {
		return nil
	}
	finals := v["a_finals"].(int)
	notices := v["a_notices"].(int)
	if a.resultOwed {
		// The job's result goes out on one more request of this turn.
		a.queue("ok")
		a.resultOwed = false
	}
	qTurn := ""
	if v["a_job"] == false {
		qTurn = a.drainTurn(v)
	}
	if a.finishAsError {
		control.ReleaseWith(a.t, a.dir, a.aHeld, control.Turn{Mode: "error", Error: "model says no"})
	} else {
		control.Release(a.t, a.dir, a.aHeld)
	}
	a.aHeld = ""
	if err := a.waitState("A's turn closed and reported", func(st map[string]any) bool {
		return st["a"] != "open" && st["a_finals"].(int) > finals && st["a_notices"].(int) > notices
	}); err != nil {
		return err
	}
	if err := a.settleDrain(qTurn); err != nil {
		return err
	}
	quiet()
	return nil
}

// AdoptA answers A's held turn with a bash call that waits on a flag:
// it outlives turn_settle and becomes job 1.
func (a *asAdapter) AdoptA() error {
	v := a.view()
	if !a.gate.pass(v["a"] == "open" && v["a_job"] == false && a.adopts < 1) {
		return nil
	}
	a.flagN++
	a.flag = filepath.Join(a.flags, fmt.Sprintf("j%d", a.flagN))
	qTurn := a.drainTurn(v) // only if serve wrongly frees the slot
	control.ReleaseWith(a.t, a.dir, a.aHeld, control.Turn{Bash: "while [ ! -f " + a.flag + " ]; do sleep 0.05; done"})
	a.aHeld = ""
	a.adopts++
	if err := a.waitState("A's turn closed on a job", func(st map[string]any) bool {
		return st["a"] == "done" && st["a_job"] == true
	}); err != nil {
		return err
	}
	quiet()
	return a.settleDrain(qTurn)
}

// JobEnds writes the flag the job waits on. Inside an open turn its
// result waits for the turn's next request; with none open it wakes A
// into a turn of its own, held like any other.
func (a *asAdapter) JobEnds() error {
	v := a.view()
	if !a.gate.pass(v["a_job"] == true) {
		return nil
	}
	wake := ""
	if v["a"] != "open" {
		wake = a.queue("block")
	} else {
		a.resultOwed = true
	}
	if err := os.WriteFile(a.flag, nil, 0o644); err != nil {
		return err
	}
	a.flag = ""
	if wake != "" {
		a.aHeld = wake
		if err := a.waitTaken(wake); err != nil {
			return err
		}
		return a.waitState("A woken", func(st map[string]any) bool { return st["a"] == "open" && st["a_job"] == false })
	}
	return a.waitState("A's job ended", func(st map[string]any) bool { return st["a_job"] == false })
}

// MessageA is the parent (or a person) prompting A again.
func (a *asAdapter) MessageA() error {
	v := a.view()
	if !a.gate.pass((v["a"] == "done" || v["a"] == "stopped") && a.msgs < 1) {
		return nil
	}
	name := a.queue("block")
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.a, "task "+name); err != nil {
		return err
	}
	a.aHeld = name
	a.msgs++
	if err := a.waitTaken(name); err != nil {
		return err
	}
	return a.waitState("A running again", func(st map[string]any) bool { return st["a"] == "open" })
}

func (a *asAdapter) ClickStopA() error {
	if !a.gate.pass(a.view()["a_row"] == "running" && a.aStop == "") {
		return nil
	}
	a.aStop, a.aReq = "stopping", true
	return nil
}

func (a *asAdapter) RetryA() error {
	if !a.gate.pass(a.aStop == "error" && a.view()["a_row"] == "running" && !a.aReq) {
		return nil
	}
	a.aStop, a.aReq = "stopping", true
	return nil
}

// StopLandsA sends the held POST /stop. "running" ends A (its turn is
// cancelled, or its idle wait on a job ends) and frees its slot, which
// drains Q; "idle" changes nothing.
func (a *asAdapter) StopLandsA() error {
	v := a.view()
	if !a.gate.pass(a.aReq) {
		return nil
	}
	a.aReq = false
	qTurn := ""
	if v["a_slot"] == true {
		qTurn = a.drainTurn(v)
	}
	notices := v["a_notices"].(int)
	was, err := a.ba.stop(a.a)
	if err != nil {
		return err
	}
	switch was {
	case "running":
		a.aHeld, a.flag, a.resultOwed = "", "", false
		if err := a.waitState("A stopped and reported", func(st map[string]any) bool {
			return st["a_row"] != "running" && st["a_notices"].(int) > notices
		}); err != nil {
			return err
		}
	case "idle":
	default:
		return fmt.Errorf("stop of A answered %q", was)
	}
	if err := a.settleDrain(qTurn); err != nil {
		return err
	}
	quiet()
	return nil
}

func (a *asAdapter) StopFailsA() error {
	if !a.gate.pass(a.aReq) {
		return nil
	}
	a.aReq = false
	a.view() // the entry may have ended with the row
	if a.aStop != "" {
		a.aStop = "error"
	}
	return nil
}

func (a *asAdapter) TimeoutA() error {
	a.view()
	if !a.gate.pass(a.aStop == "stopping") {
		return nil
	}
	a.aStop = "timeout"
	return nil
}

func (a *asAdapter) ClickStopQ() error {
	q := a.view()["q"]
	if !a.gate.pass((q == "queued" || q == "running") && a.qStop == "") {
		return nil
	}
	a.qStop, a.qReq = "stopping", true
	return nil
}

func (a *asAdapter) RetryQ() error {
	q := a.view()["q"]
	if !a.gate.pass(a.qStop == "error" && (q == "queued" || q == "running") && !a.qReq) {
		return nil
	}
	a.qStop, a.qReq = "stopping", true
	return nil
}

func (a *asAdapter) StopLandsQ() error {
	if !a.gate.pass(a.qReq) {
		return nil
	}
	a.qReq = false
	was, err := a.ba.stop(a.q)
	if err != nil {
		return err
	}
	switch was {
	case "queued":
		return a.waitState("Q gone", func(st map[string]any) bool { return st["q"] == "gone" })
	case "running":
		a.qHeld = ""
		return a.waitState("Q stopped", func(st map[string]any) bool { return st["q"] == "stopped" })
	case "idle":
		return nil
	}
	return fmt.Errorf("stop of Q answered %q", was)
}

func (a *asAdapter) StopFailsQ() error {
	if !a.gate.pass(a.qReq) {
		return nil
	}
	a.qReq = false
	a.view()
	if a.qStop != "" {
		a.qStop = "error"
	}
	return nil
}

func (a *asAdapter) TimeoutQ() error {
	a.view()
	if !a.gate.pass(a.qStop == "stopping") {
		return nil
	}
	a.qStop = "timeout"
	return nil
}

func (a *asAdapter) FinishQ() error {
	if !a.gate.pass(a.view()["q"] == "running") {
		return nil
	}
	control.Release(a.t, a.dir, a.qHeld)
	a.qHeld = ""
	return a.waitState("Q done", func(st map[string]any) bool { return st["q"] == "done" })
}

func asAction(name string, f func(*asAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*asAdapter)
		err := f(a)
		if !a.gate.off {
			a.stats[name]++
		}
		return nil, err
	}
}

var agentStopPathsActions = map[string]map[string]fmbt.ActionFunc{"Parent": {
	"SpawnA":     asAction("SpawnA", (*asAdapter).SpawnA),
	"SpawnQ":     asAction("SpawnQ", (*asAdapter).SpawnQ),
	"FinishA":    asAction("FinishA", (*asAdapter).FinishA),
	"AdoptA":     asAction("AdoptA", (*asAdapter).AdoptA),
	"JobEnds":    asAction("JobEnds", (*asAdapter).JobEnds),
	"MessageA":   asAction("MessageA", (*asAdapter).MessageA),
	"ClickStopA": asAction("ClickStopA", (*asAdapter).ClickStopA),
	"RetryA":     asAction("RetryA", (*asAdapter).RetryA),
	"StopLandsA": asAction("StopLandsA", (*asAdapter).StopLandsA),
	"StopFailsA": asAction("StopFailsA", (*asAdapter).StopFailsA),
	"TimeoutA":   asAction("TimeoutA", (*asAdapter).TimeoutA),
	"ClickStopQ": asAction("ClickStopQ", (*asAdapter).ClickStopQ),
	"RetryQ":     asAction("RetryQ", (*asAdapter).RetryQ),
	"StopLandsQ": asAction("StopLandsQ", (*asAdapter).StopLandsQ),
	"StopFailsQ": asAction("StopFailsQ", (*asAdapter).StopFailsQ),
	"TimeoutQ":   asAction("TimeoutQ", (*asAdapter).TimeoutQ),
	"FinishQ":    asAction("FinishQ", (*asAdapter).FinishQ),
}, "": {
	// deadlock_detection is off, so the runner offers a role-less "end"
	// in every state; declined like any disabled pick (see
	// unseen_trouble_ack_test.go for why it must exist).
	"end": func(m any, _ []fmbt.Arg) (any, error) {
		m.(*asAdapter).gate.pass(false)
		return nil, errDisabled
	},
}}

// agentStopPathsHistory reads A's own transcript as the spec's steps:
// its first input is SpawnA and a later one MessageA; a done with calls
// still running is AdoptA and one without is FinishA; a job's finished
// entry is JobEnds (a wake input after it is that step's turn, not a
// step); a cancelled close is a Stop that landed, after the click that
// sent it. Q, the failed and timed-out POSTs and the parent's side leave
// nothing in this file, and A's own steps are a path without them.
func agentStopPathsHistory(entries []history.Entry) []tracecheck.Step {
	st := func(kv ...any) map[string]any {
		m := map[string]any{}
		for i := 0; i < len(kv); i += 2 {
			m["Parent#0."+kv[i].(string)] = kv[i+1]
		}
		return m
	}
	steps := []tracecheck.Step{{Action: "Init", State: st("a", "none", "a_turns", 0)}}
	add := func(act string, state map[string]any) {
		steps = append(steps, tracecheck.Step{Action: "Parent#0." + act, State: state})
	}
	turns, open, lastClose, job, wakeNext := 0, false, "", false, false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			if e.Data["steer"] == true {
				continue
			}
			// JobEnds already models the wake that this input records.
			// It opens and counts that turn, so it is not MessageA.
			if e.Data["wake"] == true && wakeNext {
				wakeNext = false
				continue
			}
			turns++
			open, lastClose = true, ""
			switch {
			case turns == 1:
				add("SpawnA", st("a", "open", "a_turns", turns))
			default:
				add("MessageA", st("a", "open", "a_turns", turns, "a_job", job))
			}
		case "job":
			switch e.Data["event"] {
			case "started":
				job = true
			case "finished":
				// A stop already ended it: this is the resume recording
				// the job that died with the stopped process.
				if !job {
					continue
				}
				job = false
				if open {
					add("JobEnds", st("a_job", false))
				} else {
					turns++
					open, wakeNext = true, true
					add("JobEnds", st("a", "open", "a_job", false, "a_turns", turns))
				}
			}
		case "done", "cancelled":
			if e.Kind == "done" && lastClose == "cancelled" && !open {
				continue
			}
			lastClose = e.Kind
			if e.Kind == "cancelled" {
				job = false
				open = false
				add("ClickStopA", nil)
				add("StopLandsA", st("a", "stopped", "a_turns", turns, "a_job", false))
				continue
			}
			open = false
			if asNum(e.Data["running"]) > 0 {
				add("AdoptA", st("a", "done", "a_turns", turns, "a_job", true))
			} else {
				add("FinishA", st("a", "done", "a_turns", turns, "a_job", job))
			}
		}
	}
	return steps
}

func init() { historyProjections["agent_stop_paths"] = agentStopPathsHistory }

// walkAgentStopPaths drives the generated walks over the spec's graph
// through the adapter, comparing the whole Parent state after every
// step. The runner's random walks pick among seventeen actions, most
// disabled, and end at the first disabled pick, so they barely leave
// Init; the walks reach every state (every transition under
// MODEL_COVER=transitions). firstOnly stops at the first mismatch.
func walkAgentStopPaths(t *testing.T, a *asAdapter, walks []genPath, firstOnly bool) (mismatches []string) {
	t.Helper()
	for wi, w := range walks {
		var names []string
		for si, step := range w.Trace {
			name := strings.TrimPrefix(step.Action, "Parent#0.")
			names = append(names, name)
			var err error
			if si == 0 {
				err = a.Init()
			} else if name == "end" {
				// A state with the bounds used up links to itself: the
				// transitions cover takes that link, and nothing happens.
			} else {
				f, ok := agentStopPathsActions["Parent"][name]
				if !ok {
					t.Fatalf("walk %d: no adapter action %q", wi, name)
				}
				_, err = f(a, nil)
				if err == nil && a.gate.off {
					err = errors.New("the adapter refused a step the spec enables (its require disagrees)")
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

func (a *asAdapter) compare(want map[string]any) error {
	got, err := a.GetState()
	if err != nil {
		return err
	}
	var diff []string
	for k, v := range want {
		f, ok := strings.CutPrefix(k, "Parent#0.")
		if !ok {
			continue
		}
		if fmt.Sprint(got[f]) != fmt.Sprint(v) {
			diff = append(diff, fmt.Sprintf("%s: spec %v, got %v", f, v, got[f]))
		}
	}
	if len(diff) > 0 {
		sort.Strings(diff)
		return fmt.Errorf("state: %s", strings.Join(diff, "; "))
	}
	return nil
}

func TestAgentStopPathsPaths(t *testing.T) {
	t.Parallel()
	walks := loadPaths(t, "agent_stop_paths")
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("agent_stop_paths")), "..", "testdata", "agent_stop_paths"))
	if err != nil {
		t.Fatal(err)
	}
	const shards = 4
	for sh := range shards {
		t.Run(fmt.Sprintf("shard%d", sh), func(t *testing.T) {
			t.Parallel()
			a := newASAdapter(t)
			var mine []genPath
			for i := sh; i < len(walks); i += shards {
				mine = append(mine, walks[i])
			}
			for _, m := range walkAgentStopPaths(t, a, mine, false) {
				t.Error(m)
			}
			t.Logf("shard %d: %d walks, steps run %v", sh, len(mine), a.stats)
			for _, id := range a.as {
				checkHistory(t, g, a.traces[id], agentStopPathsHistory)
			}
		})
	}
}

// The random runs, in the exhaustive job only (runMBT skips otherwise).
func TestAgentStopPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newASAdapter(t)
	opts := map[string]any{"max-seq-runs": 200, "max-actions": 8, "max-parallel-runs": 0}
	if err := runMBT(t, "agent_stop_paths", a, agentStopPathsActions, opts); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("steps run: %v", a.stats)
}

// A FinishA that fails the turn must fail the walks: the row says
// failed and the report "failed" where the spec says finished.
func TestAgentStopPathsCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newASAdapter(t)
	a.finishAsError = true
	if len(walkAgentStopPaths(t, a, loadPaths(t, "agent_stop_paths"), true)) == 0 {
		t.Fatal("walks whose FinishA fails the turn passed; the adapter is not checking state")
	}
}

// asNum reads a JSON number off an entry's data (0 when absent).
func asNum(v any) float64 {
	f, _ := v.(float64)
	return f
}
