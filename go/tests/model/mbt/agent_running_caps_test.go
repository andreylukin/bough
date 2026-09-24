//go:build !windows

package mbt

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"reflect"
	"sort"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/agent_running_caps.fizz against a real serve: two parents (A
// and B, sessions with no process, like background_agents' parent)
// start background agents over the API with different max_running, a
// person messages and stops them, and the adapter reads each child's
// state off the parents' children listings, running counts and the
// children's history files.
//
// Drain and Launch are serve's own: a close starts drainQueue in the
// same event and launch writes the prompt at once, so a client never
// sees a close with its drain still pending, nor a drained child
// before launch (the spec's "booting" is one exec wide). The walk
// therefore does not act on those two links and compares serve with
// the state the spec reaches once they have run (arcGraph.closure). A
// person's step taken in the spec BEFORE a pending drain is driven only
// when it leads where it would after the drain; where the order
// matters (a Stop of a queued child the drain was about to start, a
// message that takes the slot the drain was about to) serve cannot be
// put there from outside and the walk ends at that step.
//
// Every child is held at boot (llm-control's hold_boot) until the
// adapter has queued its model turn, so each turn is taken by the
// child it was meant for even when a close drains two at once.
const arcConfig = "- id: llm\n  plugin: llm-control\n  config:\n    hold_boot: true\n"

const (
	arcCapA    = 2
	arcCapB    = 1
	arcDefault = 16
	arcDrain   = "Serve#0.Drain"
	arcLaunch  = "Serve#0.Launch"
)

var arcKids = []string{"a1", "a2", "b1"}

type arcAdapter struct {
	t   *testing.T
	s   *servetest.Server
	dir string // llm-control's queue
	cwd string

	a, b  string            // the parents' ids
	ids   map[string]string // kid → session id, this walk
	turns map[string]string // kid → its held model turn
	// cap, msgs are the client's own: the last max_running a spawn
	// sent, the messages sent. over is counted from what serve did: a
	// spawn that started while the running total was at the cap, or a
	// drain that left it above.
	cap, msgs, over int
	turn, walk      int
	a1s             []string // every a1, for the trace check
	compared        map[int]bool
	// wrongCap is TestAgentRunningCapsCatchesWrongAdapter's bug: B's
	// spawn sends A's cap while the adapter records B's.
	wrongCap bool
}

func newArcAdapter(t *testing.T) *arcAdapter {
	s := servetest.Start(t, servetest.Options{Config: arcConfig})
	return &arcAdapter{t: t, s: s, dir: control.Dir(s.Home), cwd: s.Dir(t, "work")}
}

// Init writes two fresh parents. The previous walk's children are
// gone (Cleanup), so serve's running map and queue are empty.
func (a *arcAdapter) Init() error {
	a.walk++
	var err error
	if a.a, err = a.parent(); err != nil {
		return err
	}
	if a.b, err = a.parent(); err != nil {
		return err
	}
	a.ids, a.turns = map[string]string{}, map[string]string{}
	a.cap, a.msgs, a.over = 0, 0, 0
	return nil
}

func (a *arcAdapter) parent() (string, error) {
	id := history.NewID()
	path := filepath.Join(a.s.Home, ".bough", "history", id+".jsonl")
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return "", err
	}
	if err := os.WriteFile(path, nil, 0o644); err != nil {
		return "", err
	}
	if _, err := history.AppendFile(path, "meta", map[string]any{"cwd": a.cwd}); err != nil {
		return "", err
	}
	return id, nil
}

func (a *arcAdapter) parentOf(kid string) string {
	if kid == "b1" {
		return a.b
	}
	return a.a
}

// Cleanup ends everything the walk left: queued children dropped,
// boots and held turns released, every process ended, so the next walk
// starts with no slot taken.
func (a *arcAdapter) Cleanup() error {
	var errs []error
	for _, k := range arcKids {
		if row := a.row(k); row != nil && row.Queued {
			if _, err := a.stop(k); err != nil {
				errs = append(errs, err)
			}
		}
	}
	for id := range control.Booting(a.dir) {
		control.ReleaseBoot(a.t, a.dir, id)
	}
	for k, name := range a.turns {
		if name == "" {
			continue
		}
		// Queued and never taken: take it back, or the next walk's
		// first model call answers with it.
		if os.Remove(filepath.Join(a.dir, name+".json")) != nil {
			control.Release(a.t, a.dir, name)
		}
		delete(a.turns, k)
	}
	for _, k := range arcKids {
		id := a.ids[k]
		if id == "" {
			continue
		}
		if err := a.waitKid(k, "its turn to close", func(r *serve.Row) bool {
			return r == nil || r.Queued || r.Status != serve.StatusRunning
		}); err != nil {
			errs = append(errs, err)
			continue
		}
		if err := a.end(k); err != nil {
			errs = append(errs, err)
		}
	}
	deadline := time.Now().Add(actionTimeout)
	for {
		ra, qa := a.agents(a.a)
		rb, qb := a.agents(a.b)
		if ra+qa+rb+qb == 0 {
			break
		}
		if time.Now().After(deadline) {
			errs = append(errs, fmt.Errorf("cleanup: slots still held: A %d/%d, B %d/%d (running/queued)", ra, qa, rb, qb))
			break
		}
		time.Sleep(50 * time.Millisecond)
	}
	return errors.Join(errs...)
}

// end SIGINTs a live child (an idle headless child exits on it).
func (a *arcAdapter) end(k string) error {
	row := a.row(k)
	if row == nil || !row.Live {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Interrupt(ctx, a.ids[k]); err != nil {
		var apiErr *servetest.APIError
		if !errors.As(err, &apiErr) || apiErr.Status != http.StatusNotFound {
			return err
		}
	}
	return a.waitKid(k, "its process to exit", func(r *serve.Row) bool { return r == nil || !r.Live })
}

// GetState is the Serve role as serve shows it, but for drain and
// stop_b1 (serve's own bookkeeping, not in any listing) and the
// client's cap, msgs and over.
//
// st: a kid's row in its parent's children listing (none without one,
// queued, booting while it has no history file, else its derived
// status with done read as idle). slots: the kids with open work,
// checked against each parent's running count, which is serve's
// s.running; a count that disagrees shows as an extra entry. queue:
// the queued kids oldest first (no listing exposes s.queue's order,
// and a drain out of order shows in st). born: every kid with a row,
// oldest first (ids are time-ordered).
func (a *arcAdapter) GetState() (map[string]any, error) {
	type kid struct{ name, id, st string }
	var kids []kid
	st := map[string]any{}
	for _, k := range arcKids {
		s := "none"
		if row := a.row(k); row != nil {
			switch {
			case row.Queued:
				s = "queued"
			case !a.hasHistory(a.ids[k]):
				s = "booting"
			case row.Status == serve.StatusDone:
				s = "idle"
			default:
				s = string(row.Status)
			}
			kids = append(kids, kid{k, a.ids[k], s})
		}
		st[k] = s
	}
	sort.Slice(kids, func(i, j int) bool { return kids[i].id < kids[j].id })
	queue, born := []any{}, []any{}
	for _, k := range kids {
		born = append(born, k.name)
		if k.st == "queued" {
			queue = append(queue, k.name)
		}
	}
	var slots []string
	for _, p := range []struct{ name, id string }{{"A", a.a}, {"B", a.b}} {
		running, _ := a.agents(p.id)
		open := 0
		for _, k := range arcKids {
			if a.parentOf(k) == p.id && (st[k] == "running" || st[k] == "booting") {
				slots = append(slots, k)
				open++
			}
		}
		if running != open {
			slots = append(slots, fmt.Sprintf("%s holds %d", p.name, running))
		}
	}
	sort.Strings(slots)
	sl := []any{}
	for _, s := range slots {
		sl = append(sl, s)
	}
	return map[string]any{
		"cap": a.cap, "st": st, "queue": queue, "slots": sl, "born": born,
		"msgs": a.msgs, "over": a.over,
	}, nil
}

// act runs one of the spec's person/agent steps and then lets serve
// settle: every child it drained is booted in turn onto a held model
// turn of its own. over is judged from the running totals around it.
func (a *arcAdapter) act(f func(before int) error) error {
	queued := map[string]bool{}
	for _, k := range arcKids {
		if row := a.row(k); row != nil && row.Queued {
			queued[k] = true
		}
	}
	before := a.running()
	if err := f(before); err != nil {
		return err
	}
	if err := a.settle(); err != nil {
		return err
	}
	drained := false
	for k := range queued {
		if row := a.row(k); row != nil && !row.Queued {
			drained = true
		}
	}
	if drained && a.running() > arcEff(a.cap) {
		a.over++
	}
	return nil
}

func arcEff(c int) int {
	if c <= 0 {
		return arcDefault
	}
	return c
}

func (a *arcAdapter) spawn(k string, cap int) error {
	return a.act(func(before int) error {
		a.cap = cap
		sent := cap
		if a.wrongCap && k == "b1" {
			sent = arcCapA
		}
		ctx, cancel := actionCtx()
		defer cancel()
		row, queued, err := a.s.CreateAgent(ctx, a.parentOf(k), fmt.Sprintf("task %s of walk %d", k, a.walk), sent, 10)
		if err != nil {
			return err
		}
		if row.ID == "" {
			return fmt.Errorf("spawn %s: no session in the answer", k)
		}
		a.ids[k] = row.ID
		if k == "a1" {
			a.a1s = append(a.a1s, row.ID)
		}
		if !queued && before >= arcEff(cap) {
			a.over++
		}
		return nil
	})
}

func (a *arcAdapter) SpawnA1() error { return a.spawn("a1", arcCapA) }
func (a *arcAdapter) SpawnA2() error { return a.spawn("a2", arcCapA) }
func (a *arcAdapter) SpawnB1() error { return a.spawn("b1", arcCapB) }

// MessageA1 is a person prompting a1 again after its turn closed.
func (a *arcAdapter) MessageA1() error {
	return a.act(func(int) error {
		name := a.queueTurn()
		ctx, cancel := actionCtx()
		defer cancel()
		if err := a.s.Prompt(ctx, a.ids["a1"], "again "+name); err != nil {
			return err
		}
		a.msgs++
		return a.taken("a1", name)
	})
}

func (a *arcAdapter) finish(k string) error {
	return a.act(func(int) error {
		name := a.turns[k]
		if name == "" {
			return fmt.Errorf("finish %s: no turn held", k)
		}
		n := a.notices(k)
		control.Release(a.t, a.dir, name)
		delete(a.turns, k)
		return a.closed(k, serve.StatusDone, n)
	})
}

func (a *arcAdapter) FinishA1() error { return a.finish("a1") }
func (a *arcAdapter) FinishA2() error { return a.finish("a2") }
func (a *arcAdapter) FinishB1() error { return a.finish("b1") }

// StopB1 is the Work dialog's Stop agent on b1.
func (a *arcAdapter) StopB1() error {
	return a.act(func(int) error {
		row := a.row("b1")
		if row == nil {
			return fmt.Errorf("stop b1: no row")
		}
		n := a.notices("b1")
		was, err := a.stop("b1")
		if err != nil {
			return err
		}
		if row.Queued {
			if was != "queued" {
				return fmt.Errorf("stop of queued b1 answered %q", was)
			}
			return a.waitKid("b1", "its row to go", func(r *serve.Row) bool { return r == nil })
		}
		if was != "running" {
			return fmt.Errorf("stop of running b1 answered %q", was)
		}
		// The interrupt cancels the held model call: nothing to release.
		delete(a.turns, "b1")
		return a.closed("b1", serve.StatusStopped, n)
	})
}

// closed waits for k's turn to close as want and for its report to
// reach the parent: the report and the drain start in the same event,
// and the report is the slower of the two.
func (a *arcAdapter) closed(k string, want serve.Status, notices int) error {
	if err := a.waitKid(k, string(want), func(r *serve.Row) bool { return r != nil && r.Status == want }); err != nil {
		return err
	}
	deadline := time.Now().Add(actionTimeout)
	for a.notices(k) <= notices {
		if time.Now().After(deadline) {
			return fmt.Errorf("%s: no report to its parent after it closed", k)
		}
		time.Sleep(20 * time.Millisecond)
	}
	return nil
}

// settle boots every child serve has started and is holding at boot,
// oldest first, each onto a model turn of its own.
func (a *arcAdapter) settle() error {
	time.Sleep(150 * time.Millisecond)
	for {
		var next string
		for _, k := range arcKids {
			row := a.row(k)
			if row == nil || row.Queued || a.hasHistory(a.ids[k]) {
				continue
			}
			if next == "" || a.ids[k] < a.ids[next] {
				next = k
			}
		}
		if next == "" {
			return nil
		}
		if err := a.boot(next); err != nil {
			return err
		}
	}
}

func (a *arcAdapter) boot(k string) error {
	id := a.ids[k]
	deadline := time.Now().Add(actionTimeout)
	for {
		if _, ok := control.Booting(a.dir)[id]; ok {
			break
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("%s: started but never held at boot", k)
		}
		time.Sleep(10 * time.Millisecond)
	}
	name := a.queueTurn()
	control.ReleaseBoot(a.t, a.dir, id)
	return a.taken(k, name)
}

func (a *arcAdapter) queueTurn() string {
	a.turn++
	name := fmt.Sprintf("r%05d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	return name
}

// taken waits for k's model call to take name and its row to say
// running.
func (a *arcAdapter) taken(k, name string) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		if _, err := os.Stat(filepath.Join(a.dir, name+".taken")); err == nil {
			break
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("%s: turn %s not taken after %s", k, name, actionTimeout)
		}
		time.Sleep(10 * time.Millisecond)
	}
	a.turns[k] = name
	return a.waitKid(k, "running", func(r *serve.Row) bool {
		return r != nil && r.Status == serve.StatusRunning && a.hasHistory(a.ids[k])
	})
}

func (a *arcAdapter) stop(k string) (string, error) {
	var r struct {
		Was string `json:"was"`
	}
	err := a.api(http.MethodPost, "/api/sessions/"+url.PathEscape(a.ids[k])+"/stop", map[string]string{"parent": a.parentOf(k)}, &r)
	return r.Was, err
}

// row is k's row in its parent's children listing, nil without one.
func (a *arcAdapter) row(k string) *serve.Row {
	id := a.ids[k]
	if id == "" {
		return nil
	}
	var r struct {
		Children []serve.Row `json:"children"`
	}
	if err := a.api(http.MethodGet, "/api/sessions/"+url.PathEscape(a.parentOf(k))+"/children", nil, &r); err != nil {
		return nil
	}
	for i := range r.Children {
		if r.Children[i].ID == id {
			return &r.Children[i]
		}
	}
	return nil
}

func (a *arcAdapter) waitKid(k, what string, ok func(*serve.Row) bool) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		row := a.row(k)
		if ok(row) {
			return nil
		}
		if time.Now().After(deadline) {
			b, _ := json.Marshal(row)
			return fmt.Errorf("waiting for %s of %s: last row %s", what, k, b)
		}
		time.Sleep(30 * time.Millisecond)
	}
}

// agents is a parent row's running and queued counts.
func (a *arcAdapter) agents(parent string) (running, queued int) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, parent)
	if err != nil || row.Agents == nil {
		return 0, 0
	}
	return row.Agents.Running, row.Agents.Queued
}

func (a *arcAdapter) running() int {
	ra, _ := a.agents(a.a)
	rb, _ := a.agents(a.b)
	return ra + rb
}

func (a *arcAdapter) hasHistory(id string) bool {
	_, err := os.Stat(filepath.Join(a.s.Home, ".bough", "history", id+".jsonl"))
	return id != "" && err == nil
}

// notices counts k's reports in its parent's transcript.
func (a *arcAdapter) notices(k string) int {
	entries, _ := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.parentOf(k)+".jsonl"))
	n := 0
	for _, e := range entries {
		if e.Kind == "notice" && e.Data["from"] == a.ids[k] {
			n++
		}
	}
	return n
}

func (a *arcAdapter) api(method, path string, body, out any) error {
	ctx, cancel := actionCtx()
	defer cancel()
	var rd *bytes.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return err
		}
		rd = bytes.NewReader(b)
	} else {
		rd = bytes.NewReader(nil)
	}
	req, err := http.NewRequestWithContext(ctx, method, a.s.URL+path, rd)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode/100 != 2 {
		return fmt.Errorf("%s %s answered %d", method, path, resp.StatusCode)
	}
	if out == nil {
		return nil
	}
	return json.NewDecoder(resp.Body).Decode(out)
}

var arcActions = map[string]func(*arcAdapter) error{
	"SpawnA1":   (*arcAdapter).SpawnA1,
	"SpawnA2":   (*arcAdapter).SpawnA2,
	"SpawnB1":   (*arcAdapter).SpawnB1,
	"MessageA1": (*arcAdapter).MessageA1,
	"FinishA1":  (*arcAdapter).FinishA1,
	"FinishA2":  (*arcAdapter).FinishA2,
	"FinishB1":  (*arcAdapter).FinishB1,
	"StopB1":    (*arcAdapter).StopB1,
}

// arcGraph is the checked-in graph with each node's links, for the
// closure under serve's own steps.
type arcGraph struct {
	g   *tracecheck.Graph
	out map[int][]int
}

func loadArcGraph() (*arcGraph, error) {
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("agent_running_caps")), "..", "testdata", "agent_running_caps"))
	if err != nil {
		return nil, err
	}
	x := &arcGraph{g: g, out: map[int][]int{}}
	for i, l := range g.Links {
		x.out[l.Src] = append(x.out[l.Src], i)
	}
	return x, nil
}

// closure runs Drain and Launch from n until neither is enabled. They
// exclude each other (Launch needs a booting child, Drain none) and
// each has one outcome, so the result is one node.
func (x *arcGraph) closure(n int) int {
	for {
		next := -1
		for _, i := range x.out[n] {
			if l := x.g.Links[i]; l.Type == "action" && (l.Name == arcDrain || l.Name == arcLaunch) {
				next = l.Dest
				break
			}
		}
		if next < 0 {
			return n
		}
		n = next
	}
}

func (x *arcGraph) step(n int, action string) (int, bool) {
	for _, i := range x.out[n] {
		if l := x.g.Links[i]; l.Type == "action" && l.Name == action {
			return l.Dest, true
		}
	}
	return 0, false
}

// settled counts the states serve can be seen in: the closures of the
// graph's settled states.
func (x *arcGraph) settled() int {
	out := map[int]bool{}
	for _, n := range x.g.Nodes {
		if n.Name == "yield" {
			out[x.closure(n.Index)] = true
		}
	}
	return len(out)
}

// seen is a node's state as the adapter can report it: no drain (never
// visible) and no stop_b1 (serve's stopReq, set only by a stop inside
// launch's exec, which a client cannot land).
func (x *arcGraph) seen(n int) map[string]any {
	out := map[string]any{}
	for k, v := range x.g.Nodes[n].State {
		f, ok := strings.CutPrefix(k, "Serve#0.")
		if ok && f != "drain" && f != "stop_b1" {
			out[f] = v
		}
	}
	return out
}

// arcWalks walks every path of cover against the adapter. It returns
// the divergences, and the links it could not drive (see the file
// comment), by action.
func arcWalks(t *testing.T, a *arcAdapter, cover tracecheck.Cover, first bool) (cut map[string]int, err error) {
	t.Helper()
	x, err := loadArcGraph()
	if err != nil {
		return nil, err
	}
	b, err := pathsJSONCover("agent_running_caps", cover)
	if err != nil {
		return nil, err
	}
	var doc struct {
		Paths []tracecheck.Walk `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		return nil, err
	}
	cut = map[string]int{}
	var errs []error
	for i, p := range doc.Paths {
		if err := a.walkLinks(x, p.Links, cut); err != nil {
			var names []string
			for _, s := range p.Trace[1:] {
				names = append(names, strings.TrimPrefix(s.Action, "Serve#0."))
			}
			errs = append(errs, fmt.Errorf("path %d %v: %w", i, names, err))
			if first {
				break
			}
		}
	}
	return cut, errors.Join(errs...)
}

func (a *arcAdapter) walkLinks(x *arcGraph, links []int, cut map[string]int) (err error) {
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	if err := a.Init(); err != nil {
		return err
	}
	if err := a.compare(x, 0, "Init"); err != nil {
		return err
	}
	for j, li := range links {
		l := x.g.Links[li]
		// Not a step of the role: a fork, serve's own Drain and Launch,
		// or the "end" stutter fizz adds for the liveness checks.
		if l.Type != "action" || !strings.HasPrefix(l.Name, "Serve#0.") || l.Name == arcDrain || l.Name == arcLaunch {
			continue
		}
		// Serve has already run every pending drain: take the step from
		// there, and only if it ends where the spec's order ends.
		from, ok := x.step(x.closure(l.Src), l.Name)
		if !ok || !reflect.DeepEqual(x.seen(x.closure(from)), x.seen(x.closure(l.Dest))) {
			cut[l.Name]++
			return nil
		}
		act := strings.TrimPrefix(l.Name, "Serve#0.")
		f := arcActions[act]
		if f == nil {
			return fmt.Errorf("link %d: no adapter action %q", j, act)
		}
		if err := f(a); err != nil {
			return fmt.Errorf("link %d (%s): %w", j, act, err)
		}
		if err := a.compare(x, x.closure(l.Dest), act); err != nil {
			return fmt.Errorf("link %d (%s): %w", j, act, err)
		}
	}
	return nil
}

func (a *arcAdapter) compare(x *arcGraph, n int, act string) error {
	if a.compared == nil {
		a.compared = map[int]bool{}
	}
	a.compared[n] = true
	got, err := a.GetState()
	if err != nil {
		return err
	}
	g, _ := json.Marshal(got)
	w, _ := json.Marshal(x.seen(n))
	var gn, wn map[string]any
	json.Unmarshal(g, &gn)
	json.Unmarshal(w, &wn)
	if !reflect.DeepEqual(gn, wn) {
		var diff []string
		for k := range wn {
			if !reflect.DeepEqual(gn[k], wn[k]) {
				gb, _ := json.Marshal(gn[k])
				wb, _ := json.Marshal(wn[k])
				diff = append(diff, fmt.Sprintf("%s is %s, the spec says %s", k, gb, wb))
			}
		}
		sort.Strings(diff)
		return fmt.Errorf("after %s: %s", act, strings.Join(diff, "; "))
	}
	return nil
}

// agentRunningCapsHistory reads a1's own transcript as the spec's a1
// steps: its first input is SpawnA1, a later one MessageA1, a closed
// turn FinishA1. a1 is never stopped, so a cancelled turn is a step the
// graph does not have. Nothing else leaves anything in a1's file, and
// a1's lifecycle is a path without the others.
func agentRunningCapsHistory(entries []history.Entry) []tracecheck.Step {
	msgs := func(n int) map[string]any { return map[string]any{"Serve#0.msgs": n} }
	steps := []tracecheck.Step{{Action: "Init", State: msgs(0)}}
	n, open, lastClose := 0, false, ""
	for _, e := range entries {
		switch e.Kind {
		case "input":
			act := "Serve#0.MessageA1"
			if len(steps) == 1 {
				act = "Serve#0.SpawnA1"
			} else {
				n++
			}
			open, lastClose = true, ""
			steps = append(steps, tracecheck.Step{Action: act, State: msgs(n)})
		case "done", "cancelled":
			if !open || (e.Kind == "done" && lastClose == "cancelled") {
				continue
			}
			open, lastClose = false, e.Kind
			act := "Serve#0.FinishA1"
			if e.Kind == "cancelled" {
				act = "Serve#0.StopA1"
			}
			steps = append(steps, tracecheck.Step{Action: act, State: msgs(n)})
		}
	}
	return steps
}

func init() { historyProjections["agent_running_caps"] = agentRunningCapsHistory }

// TestAgentRunningCapsPaths walks the checked-in graph against a real
// serve (every settled state; MODEL_COVER=transitions, every link) and
// then replays every a1's transcript on the graph.
func TestAgentRunningCapsPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newArcAdapter(t)
	cut, err := arcWalks(t, a, envCover(), false)
	if err != nil {
		t.Fatalf("spec path: %v", err)
	}
	if len(a.a1s) == 0 {
		t.Fatal("no walk spawned a1; the run checked nothing")
	}
	g, err := tracecheck.Load(fizzCheck(t, "agent_running_caps"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.a1s {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), agentRunningCapsHistory)
	}
	x, err := loadArcGraph()
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("compared serve in %d of %d states it can be seen in; trace-checked %d a1 transcripts; links not drivable ahead of a pending drain: %v",
		len(a.compared), x.settled(), len(a.a1s), cut)
}

// A transcript the model forbids must be refused: a1 messaged twice.
func TestAgentRunningCapsHistoryRejects(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(fizzCheck(t, "agent_running_caps"))
	if err != nil {
		t.Fatal(err)
	}
	in := history.Entry{Kind: "input", Data: map[string]any{"text": "x"}}
	done := history.Entry{Kind: "done"}
	if v := g.Check(agentRunningCapsHistory([]history.Entry{in, done, in, done})); v != nil {
		t.Fatalf("a spawn and one message: %v", v)
	}
	if v := g.Check(agentRunningCapsHistory([]history.Entry{in, done, in, done, in})); v == nil {
		t.Fatal("a1 messaged twice passed the trace check")
	} else {
		t.Logf("refused as expected: %v", v)
	}
}

// A run proves nothing unless serve doing the wrong thing fails it: B's
// spawn sends A's cap, so serve starts b1 where the spec queues it
// (SpawnB1 while a1 runs: a state every cover reaches). The first
// divergence is enough.
func TestAgentRunningCapsCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newArcAdapter(t)
	a.wrongCap = true
	_, err := arcWalks(t, a, envCover(), true)
	if err == nil {
		t.Fatal("a run whose SpawnB1 sends A's cap passed; the walk is not checking state")
	}
	if !strings.Contains(err.Error(), "the spec says") {
		t.Fatalf("the wrong adapter failed, but not on state: %v", err)
	}
	t.Logf("caught as expected: %v", err)
}
