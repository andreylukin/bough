//go:build !windows

package mbt

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/background_agents.fizz against a real serve: a parent session
// starts background agents over the API (what tools.spawn sends), the
// Work dialog's Stop stops them, and the parent's notices are read off
// its history file.
//
// The parent is a session with no process: a history file with its meta
// line, the way a session from before a serve restart looks. A live
// parent would take every report as a wake turn, and its model calls
// would race the children's for llm-control's one shared queue.

const (
	baMaxRunning    = 1
	baMaxPerSession = 2
)

// baModel is the spec state as the adapter drove it, read only by the
// gate: GetState reports what the server says, never this.
type baModel struct {
	first, second string
	turns, total  int
}

type baAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	cwd  string
	gate gate
	m    baModel

	parent, first, second string
	// firstSlot is how the first child's current turn was opened:
	// "serve" (spawn) or "msg" (a message). GetState reports it for the
	// slot the parent row's running count shows the child holding.
	firstSlot string
	// Turns in flight, by llm-control name; "" when none. secondTurn is
	// queued before the step that drains the second child, since the
	// drain starts its model call at once.
	firstTurn, secondTurn string
	turn                  int
	firsts                []string // every first child, for the trace check
	ran                   map[string]int

	// failAsOK is TestBackgroundAgentsCatchesWrongAdapter's bug: Fail
	// releases the turn as a success.
	failAsOK bool
}

func newBAAdapter(t *testing.T) *baAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &baAdapter{t: t, s: s, dir: control.Dir(s.Home), cwd: s.Dir(t, "work"), ran: map[string]int{}}
}

// Init writes a fresh parent. Each walk's children are gone by now
// (Cleanup), so the running map serve keeps across walks is empty.
func (a *baAdapter) Init() error {
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
	a.parent, a.first, a.second, a.firstSlot = id, "", "", ""
	a.firstTurn, a.secondTurn = "", ""
	a.m = baModel{first: "none", second: "none"}
	a.gate.reset()
	return nil
}

// Cleanup ends everything the walk left going: the queued child is
// dropped, held turns are released, and every child process is ended,
// so the next walk starts with no slot taken and no idle process left.
func (a *baAdapter) Cleanup() error {
	var errs []error
	if a.m.second == "queued" {
		if _, err := a.stop(a.second); err != nil {
			errs = append(errs, err)
		}
	}
	for _, name := range []string{a.firstTurn, a.secondTurn} {
		if name == "" {
			continue
		}
		// Queued and never taken: take it back, or the next walk's
		// first model call answers with it.
		if os.Remove(filepath.Join(a.dir, name+".json")) == nil {
			continue
		}
		control.Release(a.t, a.dir, name)
	}
	a.firstTurn, a.secondTurn = "", ""
	for _, id := range []string{a.first, a.second} {
		if id == "" {
			continue
		}
		if _, err := a.waitChild(id, "its turn to close", func(r *serve.Row) bool { return r == nil || r.Status != serve.StatusRunning }); err != nil {
			errs = append(errs, err)
			continue
		}
		if err := a.end(id); err != nil {
			errs = append(errs, err)
		}
	}
	if err := a.waitParent("no child holding a slot", func(c serve.AgentCount) bool { return c.Running == 0 }); err != nil {
		errs = append(errs, err)
	}
	return errors.Join(errs...)
}

func (a *baAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Parent", Index: 0}: a}, nil
}

// GetState is the Parent role off the API: each child's status from the
// parent's children listing (the Work dialog's rows), total and the
// running count from the parent row, notices from the parent's history
// and the first child's closed turns from its own.
//
// The running count is per parent, not per child, so it is attributed:
// an open turn is credited first (the second child's before the first's
// when they share one slot, since the second's is serve's own start),
// and a slot beyond the open turns goes to a closed child that leaked it.
func (a *baAdapter) GetState() (map[string]any, error) {
	rows, err := a.children(a.parent)
	if err != nil {
		return nil, err
	}
	count, err := a.agents()
	if err != nil {
		return nil, err
	}
	first, second := statusIn(rows, a.first), statusIn(rows, a.second)
	left := count.Running
	holds := func(open bool) bool {
		if open && left > 0 {
			left--
			return true
		}
		return false
	}
	secondHolds := holds(second == "running")
	firstHolds := holds(first == "running")
	if !firstHolds && first != "none" {
		firstHolds = holds(true)
	}
	if !secondHolds && second != "none" && second != "queued" {
		secondHolds = holds(true)
	}
	st := map[string]any{
		"first": first, "first_slot": "", "first_turns": 0, "first_notices": 0,
		"second": second, "second_slot": "", "total": count.Total, "grand": 0,
	}
	if firstHolds {
		st["first_slot"] = a.firstSlot
	}
	if secondHolds {
		st["second_slot"] = "serve"
	}
	if a.first != "" {
		st["first_turns"] = closedTurns(a.entries(a.first))
		st["first_notices"] = a.notices()
		grand, err := a.children(a.first)
		if err != nil {
			return nil, err
		}
		st["grand"] = len(grand)
	}
	return st, nil
}

// Spawn is tools.spawn({background}) as the running agent sends it.
// Past the budget it must be refused and change nothing.
func (a *baAdapter) Spawn() error {
	if a.m.total >= baMaxPerSession {
		if !a.gate.pass(true) {
			return nil
		}
		status, _, err := a.spawn(a.parent, "")
		if err == nil && status != http.StatusTooManyRequests {
			return fmt.Errorf("spawn past the budget answered %d, want 429", status)
		}
		return nil
	}
	if !a.gate.pass(a.m.first == "none" || (a.m.first == "running" && a.m.turns == 0)) {
		return nil
	}
	a.m.total++
	if a.m.first == "none" {
		name := a.queue()
		id, err := a.spawnOK(name, false)
		if err != nil {
			return err
		}
		a.first, a.firstSlot, a.firstTurn = id, "serve", name
		a.firsts = append(a.firsts, id)
		a.m.first = "running"
		return a.taken(id, name)
	}
	id, err := a.spawnOK("second", true)
	if err != nil {
		return err
	}
	a.second, a.m.second = id, "queued"
	_, err = a.waitChild(id, "queued", func(r *serve.Row) bool { return r != nil && r.Status == serve.StatusQueued })
	return err
}

// AgentSpawns is the first child calling tools.spawn itself: depth 1.
func (a *baAdapter) AgentSpawns() error {
	if !a.gate.pass(a.m.first == "running") {
		return nil
	}
	status, _, err := a.spawn(a.first, "")
	if err == nil && status != http.StatusConflict {
		return fmt.Errorf("a background agent's spawn answered %d, want 409", status)
	}
	return nil
}

func (a *baAdapter) Finish() error {
	if !a.gate.pass(a.m.first == "running") {
		return nil
	}
	return a.closeFirst(control.Turn{}, serve.StatusDone)
}

func (a *baAdapter) Fail() error {
	if !a.gate.pass(a.m.first == "running" && a.m.turns == 0) {
		return nil
	}
	if a.failAsOK {
		return a.closeFirst(control.Turn{}, serve.StatusDone)
	}
	return a.closeFirst(control.Turn{Mode: "error", Error: "model says no"}, serve.StatusError)
}

// Stop is the Work dialog's Stop agent on the running first child.
func (a *baAdapter) Stop() error {
	if !a.gate.pass(a.m.first == "running") {
		return nil
	}
	a.drainNext()
	was, err := a.stop(a.first)
	if err != nil {
		return err
	}
	if was != "running" {
		return fmt.Errorf("stop of the running first child answered %q, want \"running\"", was)
	}
	// The interrupt cancels the held model call: nothing to release.
	a.firstTurn = ""
	return a.closed(serve.StatusStopped)
}

func (a *baAdapter) FinishSecond() error {
	if !a.gate.pass(a.m.second == "running") {
		return nil
	}
	control.Release(a.t, a.dir, a.secondTurn)
	a.secondTurn = ""
	a.m.second = "done"
	_, err := a.waitChild(a.second, "done", func(r *serve.Row) bool { return r != nil && r.Status == serve.StatusDone })
	return err
}

func (a *baAdapter) StopQueued() error {
	if !a.gate.pass(a.m.second == "queued") {
		return nil
	}
	was, err := a.stop(a.second)
	if err != nil {
		return err
	}
	if was != "queued" {
		return fmt.Errorf("stop of the queued child answered %q, want \"queued\"", was)
	}
	a.m.second = "none"
	a.m.total--
	_, err = a.waitChild(a.second, "its row to go", func(r *serve.Row) bool { return r == nil })
	return err
}

// Message is the parent (or a person) prompting the finished first
// child again.
func (a *baAdapter) Message() error {
	if !a.gate.pass(slices.Contains([]string{"done", "error", "stopped"}, a.m.first) && a.m.turns < 2) {
		return nil
	}
	name := a.queue()
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.first, "task "+name); err != nil {
		return err
	}
	a.firstTurn, a.firstSlot, a.m.first = name, "msg", "running"
	return a.taken(a.first, name)
}

// Exit ends the first child's idle process (SIGINT exits an idle
// headless child) and gives a second report the time to land.
func (a *baAdapter) Exit() error {
	if !a.gate.pass(slices.Contains([]string{"done", "error", "stopped"}, a.m.first) && a.m.turns > 0) {
		return nil
	}
	if err := a.end(a.first); err != nil {
		return err
	}
	time.Sleep(500 * time.Millisecond)
	return nil
}

// closeFirst releases the first child's held turn as turn says, then
// waits for the close (closed).
func (a *baAdapter) closeFirst(turn control.Turn, want serve.Status) error {
	a.drainNext()
	if turn.Mode == "" {
		control.Release(a.t, a.dir, a.firstTurn)
	} else {
		control.ReleaseWith(a.t, a.dir, a.firstTurn, turn)
	}
	a.firstTurn = ""
	return a.closed(want)
}

// drainNext queues the model call of the child the coming close drains.
func (a *baAdapter) drainNext() {
	if a.m.second == "queued" {
		a.secondTurn = a.queue()
	}
}

// closed waits for the first child's turn to close as want, its notice,
// and the queued child's start.
func (a *baAdapter) closed(want serve.Status) error {
	a.m.first = string(want)
	a.m.turns++
	if _, err := a.waitChild(a.first, string(want), func(r *serve.Row) bool { return r != nil && r.Status == want }); err != nil {
		return err
	}
	if err := a.waitNotices(a.m.turns); err != nil {
		return err
	}
	if a.m.second == "queued" {
		a.m.second = "running"
		if err := a.taken(a.second, a.secondTurn); err != nil {
			return err
		}
	}
	// A duplicate report (the exit that follows a stop or a failure
	// re-triggers it) lands within a poll or two.
	time.Sleep(300 * time.Millisecond)
	return nil
}

// queue puts a held model turn on llm-control's queue. Names are unique
// across walks and at most one waits untaken at a time, so the child
// whose call takes it is the one the adapter meant.
func (a *baAdapter) queue() string {
	a.turn++
	name := fmt.Sprintf("b%04d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	return name
}

// taken waits for child's model call to take name and its row to say
// running.
func (a *baAdapter) taken(child, name string) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		if _, err := os.Stat(filepath.Join(a.dir, name+".taken")); err == nil {
			break
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("llm-control: turn %s not taken after %s", name, actionTimeout)
		}
		time.Sleep(10 * time.Millisecond)
	}
	_, err := a.waitChild(child, "running", func(r *serve.Row) bool { return r != nil && r.Status == serve.StatusRunning })
	return err
}

// spawnOK starts a child that must be accepted, queued as wantQueued.
func (a *baAdapter) spawnOK(prompt string, wantQueued bool) (string, error) {
	status, reply, err := a.spawn(a.parent, "task "+prompt)
	if err != nil {
		return "", err
	}
	if status != http.StatusCreated || reply.Session.ID == "" {
		return "", fmt.Errorf("spawn answered %d", status)
	}
	if reply.Queued != wantQueued {
		return reply.Session.ID, fmt.Errorf("spawn queued = %v, want %v", reply.Queued, wantQueued)
	}
	return reply.Session.ID, nil
}

type spawnReply struct {
	Session serve.Row `json:"session"`
	Queued  bool      `json:"queued"`
}

// spawn is the POST tools.spawn({background}) makes. A non-2xx is not
// an error here: the status is the answer.
func (a *baAdapter) spawn(parent, prompt string) (int, spawnReply, error) {
	var r spawnReply
	status, err := a.call(http.MethodPost, "/api/sessions", map[string]any{
		"cwd": a.cwd, "prompt": prompt, "spawnedBy": parent,
		"maxPerSession": baMaxPerSession, "maxRunning": baMaxRunning,
	}, &r)
	return status, r, err
}

// stop is the Work dialog's Stop: POST /stop naming the parent.
func (a *baAdapter) stop(id string) (string, error) {
	var r struct {
		Was string `json:"was"`
	}
	status, err := a.call(http.MethodPost, "/api/sessions/"+url.PathEscape(id)+"/stop", map[string]string{"parent": a.parent}, &r)
	if err == nil && status != http.StatusOK {
		err = fmt.Errorf("stop %s answered %d", id, status)
	}
	return r.Was, err
}

// end SIGINTs a live child (an idle headless child exits on it) and
// waits for its process to be gone.
func (a *baAdapter) end(id string) error {
	row, err := a.waitChild(id, "a row", func(*serve.Row) bool { return true })
	if err != nil || row == nil || !row.Live {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Interrupt(ctx, id); err != nil {
		var apiErr *servetest.APIError
		if !errors.As(err, &apiErr) || apiErr.Status != http.StatusNotFound {
			return err
		}
	}
	_, err = a.waitChild(id, "its process to exit", func(r *serve.Row) bool { return r == nil || !r.Live })
	return err
}

func (a *baAdapter) children(parent string) ([]serve.Row, error) {
	var r struct {
		Children []serve.Row `json:"children"`
	}
	status, err := a.call(http.MethodGet, "/api/sessions/"+url.PathEscape(parent)+"/children", nil, &r)
	if err == nil && status != http.StatusOK {
		err = fmt.Errorf("children of %s answered %d", parent, status)
	}
	return r.Children, err
}

func (a *baAdapter) agents() (serve.AgentCount, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.parent)
	if err != nil || row.Agents == nil {
		return serve.AgentCount{}, err
	}
	return *row.Agents, nil
}

// waitChild polls the parent's children listing until ok holds for
// id's row (nil when it has none).
func (a *baAdapter) waitChild(id, what string, ok func(*serve.Row) bool) (*serve.Row, error) {
	deadline := time.Now().Add(actionTimeout)
	var last *serve.Row
	for {
		rows, err := a.children(a.parent)
		if err == nil {
			last = nil
			for i := range rows {
				if rows[i].ID == id {
					last = &rows[i]
				}
			}
			if ok(last) {
				return last, nil
			}
		}
		if time.Now().After(deadline) {
			b, _ := json.Marshal(last)
			return last, fmt.Errorf("waiting for %s of child %s: last row %s, last error %v", what, id, b, err)
		}
		time.Sleep(50 * time.Millisecond)
	}
}

func (a *baAdapter) waitParent(what string, ok func(serve.AgentCount) bool) error {
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

func (a *baAdapter) waitNotices(n int) error {
	deadline := time.Now().Add(actionTimeout)
	for a.notices() < n {
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for the parent's notice %d from %s: it has %d", n, a.first, a.notices())
		}
		time.Sleep(50 * time.Millisecond)
	}
	return nil
}

// notices counts the reports from the first child in the parent's file.
func (a *baAdapter) notices() int {
	n := 0
	for _, e := range a.entries(a.parent) {
		if e.Kind == "notice" && e.Data["from"] == a.first {
			n++
		}
	}
	return n
}

func (a *baAdapter) entries(id string) []history.Entry {
	entries, _ := history.Read(filepath.Join(a.s.Home, ".bough", "history", id+".jsonl"))
	return entries
}

// call is one API request; servetest has verbs only for plain sessions.
func (a *baAdapter) call(method, path string, body, out any) (int, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	var rd io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return 0, err
		}
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, a.s.URL+path, rd)
	if err != nil {
		return 0, err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return resp.StatusCode, err
	}
	if resp.StatusCode/100 == 2 && out != nil {
		if err := json.Unmarshal(raw, out); err != nil {
			return resp.StatusCode, fmt.Errorf("%s %s: decode: %w: %s", method, path, err, raw)
		}
	}
	return resp.StatusCode, nil
}

// statusIn is id's status in the children rows, "none" without a row.
func statusIn(rows []serve.Row, id string) string {
	for _, r := range rows {
		if id != "" && r.ID == id {
			return string(r.Status)
		}
	}
	return "none"
}

// closedTurns counts a session's closed turns the way StatusOf reads
// them: done or cancelled closes the open turn, and the done a loop
// writes right after a cancel is bookkeeping.
func closedTurns(entries []history.Entry) int {
	n, open, lastClose := 0, false, ""
	for _, e := range entries {
		switch e.Kind {
		case "input":
			open, lastClose = true, ""
		case "done", "cancelled":
			if e.Kind == "done" && !open && lastClose == "cancelled" {
				continue
			}
			if open {
				n++
			}
			open, lastClose = false, e.Kind
		}
	}
	return n
}

// baAction counts the steps a walk really took (the gate still open
// after it), so a green run can say what it exercised.
func baAction(name string, f func(*baAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*baAdapter)
		err := f(a)
		if !a.gate.off {
			a.ran[name]++
		}
		return nil, err
	}
}

var backgroundAgentsActions = map[string]map[string]fmbt.ActionFunc{"Parent": {
	"Spawn":        baAction("Spawn", (*baAdapter).Spawn),
	"AgentSpawns":  baAction("AgentSpawns", (*baAdapter).AgentSpawns),
	"Finish":       baAction("Finish", (*baAdapter).Finish),
	"Fail":         baAction("Fail", (*baAdapter).Fail),
	"Stop":         baAction("Stop", (*baAdapter).Stop),
	"FinishSecond": baAction("FinishSecond", (*baAdapter).FinishSecond),
	"StopQueued":   baAction("StopQueued", (*baAdapter).StopQueued),
	"Message":      baAction("Message", (*baAdapter).Message),
	"Exit":         baAction("Exit", (*baAdapter).Exit),
}}

// A walk's first step is Spawn in one of nine picks, and the runner
// stops checking at the first disabled pick, so most walks end at once;
// those cost one parent file. The runs are many for the few that go
// deep.
func backgroundAgentsOptions() map[string]any {
	return map[string]any{"max-seq-runs": 300, "max-actions": 10, "max-parallel-runs": 0}
}

// backgroundAgentsHistory reads the first child's own transcript as the
// spec's first-child steps: its first input is the Spawn that started
// it, a later one a Message; a turn closes as Stop (cancelled), Fail (an
// error inside it) or Finish. The second child and the parent's side
// leave nothing in this file, and the first child's lifecycle is a path
// in the graph without them.
func backgroundAgentsHistory(entries []history.Entry) []tracecheck.Step {
	state := func(first string, turns int) map[string]any {
		return map[string]any{"Parent#0.first": first, "Parent#0.first_turns": turns}
	}
	steps := []tracecheck.Step{{Action: "Init", State: state("none", 0)}}
	turns, open, failed, lastClose := 0, false, false, ""
	for _, e := range entries {
		switch e.Kind {
		case "input":
			act := "Parent#0.Message"
			if len(steps) == 1 {
				act = "Parent#0.Spawn"
			}
			open, failed, lastClose = true, false, ""
			steps = append(steps, tracecheck.Step{Action: act, State: state("running", turns)})
		case "error":
			failed = true
		case "done", "cancelled":
			if !open || (e.Kind == "done" && lastClose == "cancelled") {
				continue
			}
			turns++
			open, lastClose = false, e.Kind
			switch {
			case e.Kind == "cancelled":
				steps = append(steps, tracecheck.Step{Action: "Parent#0.Stop", State: state("stopped", turns)})
			case failed:
				steps = append(steps, tracecheck.Step{Action: "Parent#0.Fail", State: state("error", turns)})
			default:
				steps = append(steps, tracecheck.Step{Action: "Parent#0.Finish", State: state("done", turns)})
			}
		}
	}
	return steps
}

func init() { historyProjections["background_agents"] = backgroundAgentsHistory }

func TestBackgroundAgents(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newBAAdapter(t)
	if err := runMBT(t, "background_agents", a, backgroundAgentsActions, backgroundAgentsOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "background_agents"))
	if err != nil {
		t.Fatal(err)
	}
	if len(a.firsts) == 0 {
		t.Fatal("no walk spawned a child; the run checked nothing")
	}
	for _, id := range a.firsts {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), backgroundAgentsHistory)
	}
	t.Logf("steps run: %v; trace-checked %d first-child transcripts", a.ran, len(a.firsts))
}

// A run proves nothing unless a server that breaks the model fails it:
// this adapter releases Fail as a success.
func TestBackgroundAgentsCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newBAAdapter(t)
	a.failAsOK = true
	if err := runMBT(t, "background_agents", a, backgroundAgentsActions, backgroundAgentsOptions()); err == nil {
		t.Fatal("a run whose Fail ends the turn as done passed; the runner is not checking state")
	}
}
