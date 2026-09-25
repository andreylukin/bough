//go:build !windows

package mbt

import (
	"encoding/json"
	"errors"
	"fmt"
	"path/filepath"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/hung_requests.fizz against a real serve: the list poll, a send
// and Stop on one open thread whose requests may answer, fail or never
// answer.
//
// The adapter plays the page (web/src/app.tsx refresh, deliverTo, act
// and Thread.stop) one model step at a time, with the readSeq/inFlight
// bookkeeping exactly as refresh() has it. What the server does is real:
// every list read is a GET /api/sessions (made when the page sends it,
// its answer held until the spec's step), a send that lands is POST
// /prompt (a steer while the turn runs, a new held turn otherwise), and
// Stop is POST /interrupt. `running` is read off the session's row.
//
// A hang is the network's, not the server's: a request that hangs is
// one whose answer the page never gets. A list read that hangs was made
// and its answer is dropped; a send POST that hangs (or fails) never
// reaches serve, which is what the spec says of it.
type hungRequestsAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string
	gate gate

	id     string
	ids    []string
	turns  int // sends, for unique input text
	queued int // llm-control turn names, unique per serve

	// refresh()'s own state, and the spec's view of it.
	reads    []*hrRead // list reads sent and not settled, oldest first
	readSeq  int
	inFlight bool
	skipped  bool
	loadErr  bool

	send     string  // none | post | posthung | wait (on sendRead)
	sendRead *hrRead // the list read deliverTo's finally awaits
	stop     string  // none | waiting

	// proceedSkipsInterrupt is TestHungRequestsPathsCatchWrongAdapter's
	// bug: once the sends settle, Stop sets itself down without POSTing
	// /interrupt.
	proceedSkipsInterrupt bool
}

type hrRead struct {
	seq     int
	hung    bool
	settled bool
	rows    []serve.Row
	err     error
}

func newHungRequestsAdapter(t *testing.T) *hungRequestsAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &hungRequestsAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

// Init is a fresh session whose first turn is held (the spec starts
// running), with the page's list idle.
func (a *hungRequestsAdapter) Init() error {
	a.reads, a.readSeq, a.inFlight, a.skipped, a.loadErr = nil, 0, false, false, false
	a.send, a.sendRead, a.stop = "none", nil, "none"
	a.gate.reset()
	a.topUp()
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "first turn")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	return a.untilRunning("the first turn")
}

// Cleanup ends the walk's turn and archives the session, which ends its
// child: a walk leaves nothing running behind it.
func (a *hungRequestsAdapter) Cleanup() error {
	if a.id == "" {
		return nil
	}
	if err := a.finish(); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Archive(ctx, a.id)
	return err
}

func (a *hungRequestsAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Page", Index: 0}: a}, nil
}

func (a *hungRequestsAdapter) GetState() (map[string]any, error) {
	running, err := a.running()
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"list": a.list(), "skipped": a.skipped, "delayed": a.loadErr,
		"send": a.sendState(), "running": running, "stop": a.stop,
	}, nil
}

// newest is the read whose answer lands (seq == readSeq), nil when none
// is out.
func (a *hungRequestsAdapter) newest() *hrRead {
	for _, r := range a.reads {
		if r.seq == a.readSeq {
			return r
		}
	}
	return nil
}

func (a *hungRequestsAdapter) list() string {
	switch r := a.newest(); {
	case r == nil:
		return "none"
	case r.hung:
		return "hung"
	}
	return "out"
}

func (a *hungRequestsAdapter) sendState() string {
	if a.send != "wait" {
		return a.send
	}
	if a.sendRead.hung {
		return "readhung"
	}
	return "read"
}

func (a *hungRequestsAdapter) running() (bool, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return false, err
	}
	return row.Status == serve.StatusRunning, nil
}

// settle ends a read: its promise resolves, and a send awaiting it in
// deliverTo's finally is over.
func (a *hungRequestsAdapter) settle(r *hrRead) {
	r.settled = true
	a.reads = slicesDelete(a.reads, r)
	if a.send == "wait" && a.sendRead == r {
		a.send, a.sendRead = "none", nil
	}
}

func slicesDelete(rs []*hrRead, r *hrRead) []*hrRead {
	out := rs[:0]
	for _, x := range rs {
		if x != r {
			out = append(out, x)
		}
	}
	return out
}

// refresh is app.tsx's: a poll returns while one read is out; anything
// else sends a new read that supersedes it. A superseded read that is
// only slow still answers (ignored, its seq is stale), so what awaits it
// is released now; a hung one never does.
func (a *hungRequestsAdapter) refresh(poll bool) (*hrRead, error) {
	if poll && a.inFlight {
		a.skipped = true
		return nil, nil
	}
	for _, r := range append([]*hrRead(nil), a.reads...) {
		if !r.hung {
			a.settle(r)
		}
	}
	a.readSeq++
	r := &hrRead{seq: a.readSeq}
	ctx, cancel := actionCtx()
	defer cancel()
	r.rows, r.err = a.s.ListSessions(ctx, false)
	a.reads = append(a.reads, r)
	a.inFlight = true
	return r, nil
}

// land is the newest read's answer or failure: the finally clears
// inFlight (seq == readSeq), and loadErr says which.
func (a *hungRequestsAdapter) land(failed bool) {
	r := a.newest()
	a.settle(r)
	a.inFlight, a.skipped, a.loadErr = false, false, failed
}

func (a *hungRequestsAdapter) PollTick() error {
	if !a.gate.pass(true) {
		return nil
	}
	_, err := a.refresh(true)
	return err
}

func (a *hungRequestsAdapter) ListAnswers() error {
	if !a.gate.pass(a.list() == "out") {
		return nil
	}
	r := a.newest()
	if r.err != nil {
		return fmt.Errorf("the list read the server answered: %w", r.err)
	}
	if !listed(r.rows, a.id) {
		return fmt.Errorf("the list read does not hold session %s", a.id)
	}
	a.land(false)
	return nil
}

func listed(rows []serve.Row, id string) bool {
	for _, r := range rows {
		if r.ID == id {
			return true
		}
	}
	return false
}

func (a *hungRequestsAdapter) ListFails() error {
	if !a.gate.pass(a.list() == "out") {
		return nil
	}
	a.land(true)
	return nil
}

func (a *hungRequestsAdapter) ListHangs() error {
	if !a.gate.pass(a.list() == "out") {
		return nil
	}
	a.newest().hung = true
	return nil
}

// RetryList: the sidebar's Retry, refresh() not as a poll.
func (a *hungRequestsAdapter) RetryList() error {
	if !a.gate.pass(a.loadErr) {
		return nil
	}
	_, err := a.refresh(false)
	return err
}

func (a *hungRequestsAdapter) Send() error {
	if !a.gate.pass(a.send == "none") {
		return nil
	}
	a.send = "post"
	return nil
}

// SendPostAnswers: the POST reaches serve. A running turn takes it as a
// steer, an idle session as a new turn; either way the turn runs. Then
// deliverTo's finally reads the list.
func (a *hungRequestsAdapter) SendPostAnswers() error {
	if !a.gate.pass(a.send == "post") {
		return nil
	}
	was, err := a.running()
	if err != nil {
		return err
	}
	a.topUp()
	a.turns++
	text := fmt.Sprintf("send %d", a.turns)
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, text); err != nil {
		return err
	}
	if was {
		// The steer is recorded in the running turn: a Finish that came
		// before it would leave it to start a turn of its own.
		if _, _, err := a.waitLines("the steer's input", func(_ serve.Row, ls []serve.Line) bool { return hasInput(ls, text) }); err != nil {
			return err
		}
	} else if err := a.untilRunning("the sent prompt's turn"); err != nil {
		return err
	}
	return a.sendFinally()
}

func (a *hungRequestsAdapter) sendFinally() error {
	r, err := a.refresh(false)
	if err != nil {
		return err
	}
	a.send, a.sendRead = "wait", r
	return nil
}

func (a *hungRequestsAdapter) SendPostFails() error {
	if !a.gate.pass(a.send == "post") {
		return nil
	}
	return a.sendFinally()
}

func (a *hungRequestsAdapter) SendPostHangs() error {
	if !a.gate.pass(a.send == "post") {
		return nil
	}
	a.send = "posthung"
	return nil
}

// Stop: Thread.stop awaits every send in flight, then act() POSTs
// /interrupt and reads the list.
func (a *hungRequestsAdapter) Stop() error {
	running, err := a.running()
	if err != nil {
		return err
	}
	if !a.gate.pass(running && a.stop == "none") {
		return nil
	}
	if a.send != "none" {
		a.stop = "waiting"
		return nil
	}
	return a.interrupt()
}

func (a *hungRequestsAdapter) StopProceeds() error {
	if !a.gate.pass(a.stop == "waiting" && a.send == "none") {
		return nil
	}
	a.stop = "none"
	if a.proceedSkipsInterrupt {
		_, err := a.refresh(false) // act's refresh, with no POST before it
		return err
	}
	return a.interrupt()
}

// interrupt is act(() => api.interrupt(id)): the POST, the turn ending,
// then act's own refresh. A turn that finished while Stop waited is
// interrupted idle; act shows that error and still reads the list.
func (a *hungRequestsAdapter) interrupt() error {
	ctx, cancel := actionCtx()
	defer cancel()
	running, err := a.running()
	if err != nil {
		return err
	}
	if err := a.s.Interrupt(ctx, a.id); err != nil && running {
		return fmt.Errorf("interrupt a running turn: %w", err)
	}
	if _, err := waitRow(a.s, a.id, "the interrupted turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning }); err != nil {
		return err
	}
	// The cancelled model request is still "taken" in llm-control: answer
	// it so it is not counted as a turn in flight.
	a.releaseAll()
	_, err = a.refresh(false)
	return err
}

func (a *hungRequestsAdapter) Finish() error {
	running, err := a.running()
	if err != nil {
		return err
	}
	if !a.gate.pass(running) {
		return nil
	}
	return a.finish()
}

// finish answers every model request the turn makes until it ends: a
// steer carries the turn on to another request.
func (a *hungRequestsAdapter) finish() error {
	_, err := waitRow(a.s, a.id, "the turn to end", func(r serve.Row) bool {
		if r.Status != serve.StatusRunning {
			return true
		}
		a.releaseAll()
		return false
	})
	return err
}

func (a *hungRequestsAdapter) untilRunning(what string) error {
	_, _, err := a.waitLines(what, func(r serve.Row, _ []serve.Line) bool {
		return r.Status == serve.StatusRunning && len(a.held()) > 0
	})
	return err
}

func (a *hungRequestsAdapter) waitLines(what string, ok func(serve.Row, []serve.Line) bool) (serve.Row, []serve.Line, error) {
	return (&steerQueueAdapter{s: a.s, id: a.id}).wait(what, ok)
}

func hasInput(lines []serve.Line, text string) bool {
	for _, l := range lines {
		if l.Kind == "input" && strings.TrimSpace(l.Text) == text {
			return true
		}
	}
	return false
}

// topUp, held and releaseAll are steer_queue's: block turns stay queued
// so every model request the session makes is held until answered.
func (a *hungRequestsAdapter) topUp() {
	names, _ := filepath.Glob(filepath.Join(a.dir, "*.json"))
	for n := len(names); n < 3; n++ {
		a.queued++
		control.Queue(a.t, a.dir, fmt.Sprintf("h%08d", a.queued), control.Turn{Mode: "block", Text: "answered"})
	}
}

func (a *hungRequestsAdapter) held() []string {
	return (&steerQueueAdapter{dir: a.dir}).held()
}

func (a *hungRequestsAdapter) releaseAll() {
	for _, name := range a.held() {
		control.Release(a.t, a.dir, name)
	}
	a.topUp()
}

var hungRequestsActions = map[string]map[string]fmbt.ActionFunc{"Page": {
	"PollTick":        action((*hungRequestsAdapter).PollTick),
	"ListAnswers":     action((*hungRequestsAdapter).ListAnswers),
	"ListFails":       action((*hungRequestsAdapter).ListFails),
	"ListHangs":       action((*hungRequestsAdapter).ListHangs),
	"RetryList":       action((*hungRequestsAdapter).RetryList),
	"Send":            action((*hungRequestsAdapter).Send),
	"SendPostAnswers": action((*hungRequestsAdapter).SendPostAnswers),
	"SendPostFails":   action((*hungRequestsAdapter).SendPostFails),
	"SendPostHangs":   action((*hungRequestsAdapter).SendPostHangs),
	"Stop":            action((*hungRequestsAdapter).Stop),
	"StopProceeds":    action((*hungRequestsAdapter).StopProceeds),
	"Finish":          action((*hungRequestsAdapter).Finish),
}}

// hungRequestsHistory reads a session's transcript as the spec's steps:
// every input after the first is a send that landed (and its list read),
// a cancel is a Stop with nothing in flight, a done a Finish.
func hungRequestsHistory(entries []history.Entry) []tracecheck.Step {
	running := func(b bool) map[string]any { return map[string]any{"Page#0.running": b} }
	step := func(action string, state map[string]any) tracecheck.Step {
		return tracecheck.Step{Action: "Page#0." + action, State: state}
	}
	var steps []tracecheck.Step
	cancelled := false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			if len(steps) == 0 {
				steps = append(steps, tracecheck.Step{Action: "Init", State: running(true)})
			} else {
				steps = append(steps, step("Send", nil), step("SendPostAnswers", running(true)), step("ListAnswers", running(true)))
			}
			cancelled = false
		case "cancelled":
			steps = append(steps, step("Stop", running(false)), step("ListAnswers", running(false)))
			cancelled = true
		case "done":
			if !cancelled {
				steps = append(steps, step("Finish", running(false)))
			}
			cancelled = true // a turn's done is its last word
		}
	}
	return steps
}

func init() { historyProjections["hung_requests"] = hungRequestsHistory }

// TestHungRequestsPaths walks the generated paths (every settled state,
// every link with MODEL_COVER=transitions) on four serves, then checks
// each transcript they wrote against the graph.
func TestHungRequestsPaths(t *testing.T) {
	t.Parallel()
	paths := loadWalks(t, "hung_requests", envCover())
	const workers = 4
	for w := range workers {
		t.Run(fmt.Sprintf("serve%d", w), func(t *testing.T) {
			t.Parallel()
			a := newHungRequestsAdapter(t)
			for j := w; j < len(paths); j += workers {
				if err := walkRole(a, "Page", hungRequestsActions, &a.gate, paths[j]); err != nil {
					t.Errorf("path %d: %v", j, err)
				}
			}
			g, err := tracecheck.Load(testdataDir("hung_requests"))
			if err != nil {
				t.Fatal(err)
			}
			for _, id := range a.ids {
				checkHistory(t, g, sessionHistory(t, a.s.Home, id), hungRequestsHistory)
			}
		})
	}
}

// A Stop that never reaches serve once its sends settle shows only on
// StopProceeds, so this walks every link until a path fails.
func TestHungRequestsPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newHungRequestsAdapter(t)
	a.proceedSkipsInterrupt = true
	for _, p := range loadWalks(t, "hung_requests", tracecheck.CoverTransitions) {
		if err := walkRole(a, "Page", hungRequestsActions, &a.gate, p); err != nil {
			t.Logf("caught: %v", err)
			return
		}
	}
	t.Fatal("every path passed with a Stop that never POSTs /interrupt after waiting; the walk is not checking state")
}

func TestHungRequests(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newHungRequestsAdapter(t)
	opts := map[string]any{"max-seq-runs": 300, "max-actions": 10, "max-parallel-runs": 0}
	if err := runMBT(t, "hung_requests", a, hungRequestsActions, opts); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// --- shared by the hung_* flows ---

func testdataDir(spec string) string {
	return filepath.Join(filepath.Dir(specPath(spec)), "..", "testdata", spec)
}

func loadWalks(t *testing.T, spec string, cover tracecheck.Cover) []tracecheck.Walk {
	t.Helper()
	raw, err := pathsJSONCover(spec, cover)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []tracecheck.Walk `json:"paths"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		t.Fatal(err)
	}
	return doc.Paths
}

// walkRole drives one generated path through an adapter and compares
// the role's state after every step. Every action on a path is enabled
// in the spec, so the gate closing is itself a mismatch.
func walkRole(m fmbt.Model, role string, actions map[string]map[string]fmbt.ActionFunc, g *gate, p tracecheck.Walk) (err error) {
	defer func() {
		if cerr := m.Cleanup(); err == nil {
			err = cerr
		}
	}()
	roles, err := m.GetRoles()
	if err != nil {
		return err
	}
	var r fmbt.Role
	for _, x := range roles {
		r = x
	}
	sr := r.(interface {
		GetState() (map[string]any, error)
	})
	prefix := role + "#0."
	for i, st := range p.Trace {
		name := strings.TrimPrefix(st.Action, prefix)
		if st.Action == "Init" {
			err = m.Init()
		} else if f, ok := actions[role][name]; ok {
			_, err = f(r, nil)
		} else {
			err = fmt.Errorf("no action %q", st.Action)
		}
		if err == nil && g.off {
			err = errors.New("the adapter found the action disabled")
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, name, err)
		}
		got, err := sr.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, name, err)
		}
		gj, _ := json.Marshal(got)
		var have map[string]any
		json.Unmarshal(gj, &have)
		for k, want := range st.State {
			field, ok := strings.CutPrefix(k, prefix)
			if !ok {
				continue
			}
			wj, _ := json.Marshal(want)
			hj, _ := json.Marshal(have[field])
			if string(wj) != string(hj) {
				return fmt.Errorf("step %d (%s): %s: want %s, got %s", i, name, field, wj, hj)
			}
		}
	}
	return nil
}
