//go:build !windows

package mbt

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"reflect"
	"slices"
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

// specs/steer_queue.fizz: Enter steers a running turn, Cmd/Ctrl+Enter
// queues behind it, and Stop may swallow a line already written.
//
// The adapter plays one thread's page (web/src/app.tsx Thread) against
// a real serve. The composer's own state -- draft, the sessionStorage
// queue, the stoppedIds a Stop remembers -- has no server record, so the
// adapter keeps it exactly as the page does. What the server decides is
// read off it: a sent line counts as landed only once its input is in
// the transcript, and status is the row's, read when the step that
// changes it has happened. Person actions (Send, Enqueue, Stop, ...) do
// what the page does and do not wait for a render; the page and child
// actions (Start, SteerLands, Finish, ...) wait for the server to show
// the step, so a server that never does it fails the walk.
//
// Which way a race goes is the server's choice, not the runner's: when
// the runner picks a branch the server did not take (Swallow while the
// held interrupt let the prompt land), the step answers
// fmbt.ErrNotImplemented, which ends that walk's check there.
type steerQueueAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	walk  int
	id    string
	ids   []string
	turns int // llm-control turn names, unique across walks

	// The page's view, field for field with the spec's role.
	status     string
	draft      string
	queue      []int
	sending    []sent // non-steer rows sent and not landed, oldest first
	steer      string // none | pending | dropped
	steerRow   *sent  // the pending steer's row
	made       int
	stops      int
	stopSend   bool
	stopSteer  bool
	restoreOne bool // app.tsx restoreOnStop

	stats map[string]int

	// finishAsError is the deliberate bug the wrong-adapter test
	// injects: Finish fails the model request instead of answering it.
	finishAsError bool
}

// sent is one Pending row: its text, and the newest seq the page had
// when it sent it, so only an input recorded after that lands it.
type sent struct {
	id    int
	text  string
	after int64
}

func newSteerQueueAdapter(t *testing.T) *steerQueueAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &steerQueueAdapter{t: t, s: s, dir: control.Dir(s.Home), stats: map[string]int{}}
}

func (a *steerQueueAdapter) text(id int) string {
	return fmt.Sprintf("walk %d message %d", a.walk, id)
}

// Init starts each walk on a fresh session whose first turn is running
// and held: the spec's Init.
func (a *steerQueueAdapter) Init() error {
	a.walk++
	a.status, a.draft, a.queue, a.sending = "", "", nil, nil
	a.steer, a.steerRow = "none", nil
	a.made, a.stops, a.stopSend, a.stopSteer, a.restoreOne = 0, 0, false, false, false
	a.gate.reset()
	a.topUp()
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), fmt.Sprintf("walk %d first prompt", a.walk))
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	row, _, err = a.wait("the first turn to run", func(r serve.Row, _ []serve.Line) bool {
		return r.Status == serve.StatusRunning && len(a.held()) > 0
	})
	a.status = pageStatus(row.Status)
	return err
}

// Cleanup archives the walk's session, which kills its child and waits
// for the reap: a child left alive would take the next walk's queued
// turns (a landed steer or an answer asks the model again).
func (a *steerQueueAdapter) Cleanup() error {
	if a.id == "" {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Archive(ctx, a.id)
	a.releaseAll(nil)
	a.topUp()
	return err
}

func (a *steerQueueAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

func (a *steerQueueAdapter) GetState() (map[string]any, error) {
	sending := []int{}
	for _, p := range a.sending {
		sending = append(sending, p.id)
	}
	return map[string]any{
		"status":     a.status,
		"draft":      a.draft,
		"queue":      append([]int{}, a.queue...),
		"sending":    sending,
		"steer":      a.steer,
		"made":       a.made,
		"stops":      a.stops,
		"stop_send":  a.stopSend,
		"stop_steer": a.stopSteer,
	}, nil
}

// ---- the person ----

func (a *steerQueueAdapter) live() bool { return a.status == "running" || len(a.sending) > 0 }

func (a *steerQueueAdapter) flushReady() bool {
	return a.status == "idle" && len(a.sending) == 0 && a.steer != "pending" && len(a.queue) > 0
}

// Send is Enter: a steer while the turn is live, else a message.
func (a *steerQueueAdapter) Send() error {
	live := a.live()
	if !a.gate.pass(a.draft != "answer" && a.status != "needs-you" && !a.flushReady() &&
		a.made < 2 && (!live || a.steer != "pending")) {
		return nil
	}
	p, err := a.post(a.made)
	if err != nil {
		return err
	}
	if live {
		a.steer, a.steerRow = "pending", &p
	} else {
		a.sending = append(a.sending, p)
	}
	a.made++
	a.draft = ""
	return nil
}

// Enqueue is Cmd/Ctrl+Enter: the message waits in the tab's queue.
func (a *steerQueueAdapter) Enqueue() error {
	if !a.gate.pass(a.status == "running" && a.draft != "answer" && a.made < 2) {
		return nil
	}
	a.queue = append(a.queue, a.made)
	a.made++
	a.draft = ""
	return nil
}

func (a *steerQueueAdapter) WriteAnswer() error {
	if a.gate.pass(a.status == "needs-you" && a.draft == "") {
		a.draft = "answer"
	}
	return nil
}

// Answer is Enter on the answer draft while its question is armed.
func (a *steerQueueAdapter) Answer() error {
	if !a.gate.pass(a.status == "needs-you" && a.draft == "answer") {
		return nil
	}
	if err := a.answer(fmt.Sprintf("walk %d my answer", a.walk)); err != nil {
		return err
	}
	a.draft = ""
	return a.running("the answered turn to run on")
}

func (a *steerQueueAdapter) Clear() error {
	if a.gate.pass(a.draft != "" && !a.flushReady()) {
		a.draft = ""
	}
	return nil
}

// RemoveQueued drops the queued message the runner's choice names.
func (a *steerQueueAdapter) RemoveQueued(args []fmbt.Arg) error {
	i, ok := argOf(args, "i").(int)
	if !a.gate.pass(len(a.queue) > 0 && !a.flushReady() && ok && i < len(a.queue)) {
		return nil
	}
	a.queue = slices.Delete(a.queue, i, i+1)
	return nil
}

// argOf is the runner's value for one of the action's choices.
func argOf(args []fmbt.Arg, name string) any {
	for _, a := range args {
		if a.Name == name {
			return a.Value
		}
	}
	return nil
}

// Stop is Esc: the page remembers which rows were unlanded, then
// interrupts. A running turn is over once its cancel is recorded; a
// prompt the child has not taken is the held-interrupt case, settled
// by Start or Swallow.
//
// Whether the page puts the stopped prompt back is the product's call
// (a turn that already replied keeps the composer empty); the runner
// names the branch it wants, and a walk whose branch the product did
// not take ends there. A runner that sends no choice validates against
// the node before the fork, which no Stop of a running turn leaves, so
// that walk ends here too, before anything is sent.
func (a *steerQueueAdapter) Stop(args []fmbt.Arg) error {
	if !a.gate.pass(a.live() && a.stops < 1) {
		return nil
	}
	want, chosen := argOf(args, "restored").(string)
	if a.status == "running" && a.draft == "" && !chosen {
		return fmbt.ErrNotImplemented
	}
	a.stops++
	a.stopSend = len(a.sending) > 0
	a.stopSteer = a.steer == "pending"
	a.restoreOne = true
	since := a.newest()
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Interrupt(ctx, a.id); err != nil {
		return err
	}
	if a.status != "running" {
		return nil
	}
	row, lines, err := a.waitFor(stopWait, "the stopped turn's cancel", func(r serve.Row, ls []serve.Line) bool {
		return r.Status != serve.StatusRunning && hasKindAfter(ls, "cancelled", since)
	})
	if err != nil {
		return err
	}
	a.releaseAll(nil) // the cancelled request's file: nobody reads it now
	a.status = pageStatus(row.Status)
	a.restore(lines)
	if chosen && want != a.draft {
		return fmbt.ErrNotImplemented
	}
	return nil
}

// ---- the page and the child ----

// Flush is the flush effect: the queue's head goes to deliver().
func (a *steerQueueAdapter) Flush() error {
	if !a.gate.pass(a.flushReady()) {
		return nil
	}
	p, err := a.post(a.queue[0])
	if err != nil {
		return err
	}
	a.sending = append(a.sending, p)
	a.queue = a.queue[1:]
	return nil
}

// Start: the child takes the oldest unlanded line. After a Stop that
// was held for it, the turn is cancelled at once and the page puts the
// prompt back.
func (a *steerQueueAdapter) Start() error {
	if !a.gate.pass(a.status == "idle" && len(a.sending) > 0) {
		return nil
	}
	p := a.sending[0]
	d := actionTimeout
	if a.stopSend {
		d = stopWait
	}
	row, lines, err := a.waitFor(d, "the sent message's input", func(r serve.Row, ls []serve.Line) bool {
		seq := landedAt(ls, p)
		if seq == 0 {
			return false
		}
		if a.stopSend {
			return r.Status != serve.StatusRunning && hasKindAfter(ls, "cancelled", seq)
		}
		return r.Status == serve.StatusRunning && len(a.held()) > 0
	})
	if err != nil {
		return err
	}
	a.sending = a.sending[1:]
	a.status = pageStatus(row.Status)
	if a.stopSend {
		a.stopSend = false
		a.releaseAll(nil)
		a.restore(lines)
	}
	return nil
}

// SteerLands: the pending steer's input is recorded in the turn, the
// running one or the one a Stop cancelled.
func (a *steerQueueAdapter) SteerLands() error {
	if !a.gate.pass((a.status == "running" || a.stopSteer) && a.steer == "pending") {
		return nil
	}
	p := *a.steerRow
	// After a Stop the steer lands in the turn the Stop cancels: the
	// engine records it before the cancel, so the row still says running
	// for a moment. The spec's step is the landing together with that
	// cancel (the thread stays idle); reading the row between the two
	// reported running (path 47 of the exhaustive walk).
	stopped := a.stopSteer
	row, _, err := a.wait("the steer's input", func(r serve.Row, ls []serve.Line) bool {
		return landedAt(ls, p) != 0 && (!stopped || r.Status != serve.StatusRunning)
	})
	if err != nil {
		return err
	}
	a.steer, a.steerRow, a.stopSteer = "none", nil, false
	a.status = pageStatus(row.Status)
	return nil
}

// SteerAsTurn: a steer the child read with no turn open runs as its
// own turn.
func (a *steerQueueAdapter) SteerAsTurn() error {
	if !a.gate.pass(a.status == "idle" && a.steer == "pending" && len(a.sending) == 0) {
		return nil
	}
	p := *a.steerRow
	row, lines, err := a.waitFor(swallowWait, "the steer's own turn", func(r serve.Row, ls []serve.Line) bool {
		return landedAt(ls, p) != 0 && r.Status == serve.StatusRunning && len(a.held()) > 0
	})
	if err != nil {
		if landedAt(lines, p) != 0 || !swallowedByStop(lines, p) {
			return fmbt.ErrNotImplemented // the server did something else with it
		}
		return err
	}
	a.steer, a.steerRow, a.stopSteer = "none", nil, false
	a.status = pageStatus(row.Status)
	return nil
}

// Finish: the model answers. With a steer pending the steer keeps the
// turn going; otherwise every request the turn makes is answered until
// the turn ends.
func (a *steerQueueAdapter) Finish() error {
	if !a.gate.pass(a.status == "running") {
		return nil
	}
	if a.steer == "pending" {
		p := *a.steerRow
		a.releaseAll(nil)
		row, _, err := a.wait("the steer to carry the turn on", func(r serve.Row, ls []serve.Line) bool {
			return landedAt(ls, p) != 0 && r.Status == serve.StatusRunning && len(a.held()) > 0
		})
		if err != nil {
			return err
		}
		a.steer, a.steerRow, a.stopSteer = "none", nil, false
		a.status = pageStatus(row.Status)
		return nil
	}
	var release *control.Turn
	if a.finishAsError {
		release = &control.Turn{Mode: "error", Error: "model says no"}
	}
	row, _, err := a.wait("the turn to end", func(r serve.Row, _ []serve.Line) bool {
		if r.Status != serve.StatusRunning {
			return true
		}
		a.releaseAll(release)
		return false
	})
	if err != nil {
		return err
	}
	a.status = pageStatus(row.Status)
	return nil
}

// Ask: the held request answers with a tools.ask call.
func (a *steerQueueAdapter) Ask() error {
	if !a.gate.pass(a.status == "running" && a.steer != "pending") {
		return nil
	}
	a.releaseAll(&control.Turn{Call: &control.Call{Name: "ask", Args: map[string]any{"question": fmt.Sprintf("walk %d which one?", a.walk)}}})
	row, _, err := a.wait("the question", func(r serve.Row, _ []serve.Line) bool { return r.Status == serve.StatusNeedsYou })
	if err != nil {
		return err
	}
	a.status = pageStatus(row.Status)
	return nil
}

// AskGone: another client answers the question.
func (a *steerQueueAdapter) AskGone() error {
	if !a.gate.pass(a.status == "needs-you") {
		return nil
	}
	if err := a.answer(fmt.Sprintf("walk %d answered elsewhere", a.walk)); err != nil {
		return err
	}
	return a.running("the turn to run on after the answer")
}

// Swallow is the swallowedByStop timer: a covered row that never landed
// after the stopped turn ended. When the server landed it instead, the
// server took Start or SteerLands, not this.
func (a *steerQueueAdapter) Swallow() error {
	msgs := a.stopSend && len(a.sending) > 0
	steer := a.stopSteer && a.steer == "pending"
	if !a.gate.pass(a.status == "idle" && (msgs || steer)) {
		return nil
	}
	var covered []sent
	if msgs {
		covered = append(covered, a.sending...)
	}
	if steer {
		covered = append(covered, *a.steerRow)
	}
	_, lines, _ := a.waitFor(swallowWait, "a covered row to land", func(_ serve.Row, ls []serve.Line) bool {
		return slices.ContainsFunc(covered, func(p sent) bool { return landedAt(ls, p) != 0 })
	})
	for _, p := range covered {
		if landedAt(lines, p) != 0 || !swallowedByStop(lines, p) {
			return fmbt.ErrNotImplemented
		}
	}
	if steer {
		a.steer, a.steerRow = "dropped", nil
	}
	if msgs {
		ids := []int{}
		for _, p := range a.sending {
			ids = append(ids, p.id)
		}
		a.queue = append(ids, a.queue...)
		a.sending = nil
	}
	a.stopSend, a.stopSteer = false, false
	return nil
}

// stopWait bounds how long a Stop may take to end the turn. It is a
// claim about the product, not slack: a Stop held until the model's
// first token did nothing for 20 s (the supervisor's holdLimit).
const stopWait = 10 * time.Second

// swallowWait is the page's 4 s settle plus a margin: a row that has not
// landed by then is one the page reports swallowed.
const swallowWait = 5 * time.Second

// ---- helpers ----

// pageStatus is the row's status as the flow's composer reads it: a
// finished or stopped turn is idle. Anything else is passed through, so
// a row the flow never expects (error) shows up as a state mismatch.
func pageStatus(s serve.Status) string {
	switch s {
	case serve.StatusDone, serve.StatusStopped:
		return "idle"
	}
	return string(s)
}

// post sends message id the way deliver() does, remembering the newest
// seq it had so an older input with the same text cannot land it.
func (a *steerQueueAdapter) post(id int) (sent, error) {
	p := sent{id: id, text: a.text(id), after: a.newest()}
	ctx, cancel := actionCtx()
	defer cancel()
	return p, a.s.Prompt(ctx, a.id, p.text)
}

// restore is the page's restoreOnStop effect: a turn stopped before any
// reply puts its prompt back in an empty composer.
func (a *steerQueueAdapter) restore(lines []serve.Line) {
	if !a.restoreOne || stoppedPrompt(lines) == "" {
		return
	}
	a.restoreOne = false
	if a.draft == "" {
		a.draft = "msg"
	}
}

func (a *steerQueueAdapter) running(what string) error {
	row, _, err := a.wait(what, func(r serve.Row, _ []serve.Line) bool {
		return r.Status == serve.StatusRunning && len(a.held()) > 0
	})
	if err != nil {
		return err
	}
	a.status = pageStatus(row.Status)
	return nil
}

func (a *steerQueueAdapter) newest() int64 {
	ctx, cancel := actionCtx()
	defer cancel()
	_, lines, err := a.s.GetSession(ctx, a.id)
	if err != nil || len(lines) == 0 {
		return 0
	}
	return lines[len(lines)-1].Seq
}

// answer is POST /api/sessions/{id}/answer with the armed question's id,
// as the page's answer and another client's both send it.
func (a *steerQueueAdapter) answer(text string) error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return err
	}
	if row.Ask == nil {
		return fmt.Errorf("answer: no question armed (status %s)", row.Status)
	}
	b, _ := json.Marshal(map[string]string{"text": text, "ask": row.Ask.ID})
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.s.URL+"/api/sessions/"+url.PathEscape(a.id)+"/answer", bytes.NewReader(b))
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
		return fmt.Errorf("answer: %s", resp.Status)
	}
	return nil
}

func (a *steerQueueAdapter) wait(what string, ok func(serve.Row, []serve.Line) bool) (serve.Row, []serve.Line, error) {
	return a.waitFor(actionTimeout, what, ok)
}

// waitFor polls the session until ok holds, returning the last row and
// transcript either way.
func (a *steerQueueAdapter) waitFor(d time.Duration, what string, ok func(serve.Row, []serve.Line) bool) (serve.Row, []serve.Line, error) {
	ctx, cancel := context.WithTimeout(context.Background(), d)
	defer cancel()
	var row serve.Row
	var lines []serve.Line
	for {
		r, ls, err := a.s.GetSession(ctx, a.id)
		if err == nil {
			row, lines = r, ls
			if ok(r, ls) {
				return row, lines, nil
			}
		}
		select {
		case <-ctx.Done():
			return row, lines, fmt.Errorf("waiting for %s: last status %q, transcript %s", what, row.Status, kinds(lines))
		case <-time.After(50 * time.Millisecond):
		}
	}
}

// topUp keeps three block turns queued, so every model request the
// session makes -- a new turn, a steer, an answered question -- is held
// until the adapter answers it.
func (a *steerQueueAdapter) topUp() {
	names, _ := filepath.Glob(filepath.Join(a.dir, "*.json"))
	for n := len(names); n < 3; n++ {
		a.turns++
		control.Queue(a.t, a.dir, fmt.Sprintf("q%08d", a.turns), control.Turn{Mode: "block", Text: "answered"})
	}
}

// held lists the requests in flight: turns taken and not yet released.
func (a *steerQueueAdapter) held() []string {
	taken, _ := filepath.Glob(filepath.Join(a.dir, "*.taken"))
	var out []string
	for _, f := range taken {
		name := strings.TrimSuffix(filepath.Base(f), ".taken")
		if _, err := os.Stat(filepath.Join(a.dir, name+".release")); errors.Is(err, os.ErrNotExist) {
			out = append(out, name)
		}
	}
	return out
}

// releaseAll answers every request in flight, as turn says when it is
// not nil, and queues more turns behind them.
func (a *steerQueueAdapter) releaseAll(turn *control.Turn) {
	for _, name := range a.held() {
		if turn == nil {
			control.Release(a.t, a.dir, name)
		} else {
			control.ReleaseWith(a.t, a.dir, name, *turn)
		}
	}
	a.topUp()
}

// landedAt is the seq of the input that landed p (page: an input
// recorded after p was sent, with its text), 0 when none has.
func landedAt(lines []serve.Line, p sent) int64 {
	for _, l := range lines {
		if l.Kind == "input" && l.Seq > p.after && strings.TrimSpace(l.Text) == p.text {
			return l.Seq
		}
	}
	return 0
}

func hasKindAfter(lines []serve.Line, kind string, seq int64) bool {
	return slices.ContainsFunc(lines, func(l serve.Line) bool { return l.Kind == kind && l.Seq > seq })
}

// swallowedByStop is app.tsx's: a done or cancel recorded after p with
// no input after it.
func swallowedByStop(lines []serve.Line, p sent) bool {
	return (hasKindAfter(lines, "done", p.after) || hasKindAfter(lines, "cancelled", p.after)) && !hasKindAfter(lines, "input", p.after)
}

// stoppedPrompt is app.tsx's: the last non-steer prompt, when a cancel
// came after it and no assistant reply did.
func stoppedPrompt(lines []serve.Line) string {
	i := len(lines) - 1
	for i >= 0 && (lines[i].Kind != "input" || lines[i].Data["steer"] == true) {
		i--
	}
	if i < 0 {
		return ""
	}
	after := lines[i+1:]
	if !slices.ContainsFunc(after, func(l serve.Line) bool { return l.Kind == "cancelled" }) ||
		slices.ContainsFunc(after, func(l serve.Line) bool { return l.Kind == "assistant" }) {
		return ""
	}
	return lines[i].Text
}

func kinds(lines []serve.Line) string {
	var b strings.Builder
	for _, l := range lines {
		if l.Kind == "meta" || strings.HasSuffix(l.Kind, "-delta") {
			continue
		}
		fmt.Fprintf(&b, "%d:%s", l.Seq, l.Kind)
		if l.Kind == "input" {
			fmt.Fprintf(&b, "(%q steer=%v)", l.Text, l.Data["steer"] == true)
		}
		b.WriteString(" ")
	}
	return b.String()
}

// withArgs is action for a method that reads the runner's choices.
func withArgs(f func(*steerQueueAdapter, []fmbt.Arg) error) fmbt.ActionFunc {
	return func(m any, args []fmbt.Arg) (any, error) { return nil, f(m.(*steerQueueAdapter), args) }
}

// steerQueueActions counts what each walk step did, so a green run can
// say how much of it was checked: done (performed), skipped (a disabled
// step, or one after it) or notTaken (the server took another branch).
func steerQueueActions(a *steerQueueAdapter) map[string]map[string]fmbt.ActionFunc {
	acts := map[string]map[string]fmbt.ActionFunc{"Session": {}, "": {}}
	for role, m := range steerQueueActionTable {
		for name, f := range m {
			acts[role][name] = func(m any, args []fmbt.Arg) (any, error) {
				was := a.gate.off
				v, err := f(m, args)
				switch {
				case errors.Is(err, fmbt.ErrNotImplemented):
					a.stats["notTaken "+name]++
				case err != nil:
					a.stats["failed "+name]++
				case was || a.gate.off:
					a.stats["skipped"]++
				default:
					a.stats["done "+name]++
				}
				return v, err
			}
		}
	}
	return acts
}

var steerQueueActionTable = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Send":         action((*steerQueueAdapter).Send),
	"Enqueue":      action((*steerQueueAdapter).Enqueue),
	"WriteAnswer":  action((*steerQueueAdapter).WriteAnswer),
	"Answer":       action((*steerQueueAdapter).Answer),
	"Clear":        action((*steerQueueAdapter).Clear),
	"RemoveQueued": withArgs((*steerQueueAdapter).RemoveQueued),
	"Stop":         withArgs((*steerQueueAdapter).Stop),
	"Flush":        action((*steerQueueAdapter).Flush),
	"Start":        action((*steerQueueAdapter).Start),
	"SteerLands":   action((*steerQueueAdapter).SteerLands),
	"SteerAsTurn":  action((*steerQueueAdapter).SteerAsTurn),
	"Finish":       action((*steerQueueAdapter).Finish),
	"Ask":          action((*steerQueueAdapter).Ask),
	"AskGone":      action((*steerQueueAdapter).AskGone),
	"Swallow":      action((*steerQueueAdapter).Swallow),
}, "": {
	// fizz links a state with nothing enabled to itself as "end" (a
	// quiet thread with its bounds used up); the runner walks it as a
	// model action, and nothing happens.
	"end": func(any, []fmbt.Arg) (any, error) { return nil, nil },
}}

func steerQueueOptions() map[string]any {
	return map[string]any{"max-seq-runs": 200, "max-actions": 8, "max-parallel-runs": 0}
}

// steerQueueHistory reads the abstract trace off a transcript. The
// composer's own steps (Enqueue, Clear, a draft) leave nothing there,
// so the projection takes the one path every transcript also is: each
// message was sent and started (a flushed one looks the same), each
// steer sent and landed, a cancel is Stop, a close is Finish, an ask is
// Ask and its answer AskGone. The check is on status alone.
func steerQueueHistory(entries []history.Entry) []tracecheck.Step {
	status := func(s string) map[string]any { return map[string]any{"Session#0.status": s} }
	step := func(action, s string) tracecheck.Step {
		return tracecheck.Step{Action: "Session#0." + action, State: status(s)}
	}
	var steps []tracecheck.Step
	cur, cancelled := "", false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			switch {
			case len(steps) == 0:
				steps = append(steps, tracecheck.Step{Action: "Init", State: status("running")})
			case e.Data["steer"] == true:
				steps = append(steps, step("Send", cur), step("SteerLands", "running"))
			default:
				steps = append(steps, step("Send", "idle"), step("Start", "running"))
			}
			cur, cancelled = "running", false
		case "ask":
			steps = append(steps, step("Ask", "needs-you"))
			cur = "needs-you"
		case "ask/answer":
			steps = append(steps, step("AskGone", "running"))
			cur = "running"
		case "cancelled":
			steps = append(steps, step("Stop", "idle"))
			cur, cancelled = "idle", true
		case "done":
			if !cancelled && cur != "" && cur != "idle" {
				steps = append(steps, step("Finish", "idle"))
			}
			cur = "idle"
		}
	}
	return steps
}

func init() { historyProjections["steer_queue"] = steerQueueHistory }

func TestSteerQueue(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSteerQueueAdapter(t)
	if err := runMBT(t, "steer_queue", a, steerQueueActions(a), steerQueueOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("walk steps: %v", a.stats)
	g, err := tracecheck.Load(fizzCheck(t, "steer_queue"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), steerQueueHistory)
	}
}

// The runner's walks pick actions at random, disabled ones included, and
// stop checking at the first disabled one: with 15 actions and a few
// enabled in any state, its walks rarely get past two steps (one run:
// ~30 steps checked, ~760 skipped). So every path the generator wrote
// for the browser stage (testdata/steer_queue/paths.json, all 525
// transitions) is also walked here, step by step, against the same
// adapter, comparing the whole role state after each step. A fork's
// branch is read off the path's next state and passed as the runner
// would pass it. A path whose branch the server did not take stops
// there and is counted, not failed.
func TestSteerQueuePaths(t *testing.T) {
	t.Parallel()
	paths := loadPaths(t, "steer_queue")
	const shards = 4
	for sh := range shards {
		t.Run(fmt.Sprintf("shard%d", sh), func(t *testing.T) {
			t.Parallel()
			a := newSteerQueueAdapter(t)
			acts := steerQueueActions(a)
			for i := sh; i < len(paths); i += shards {
				walkSteerPath(t, a, acts, i, paths[i])
			}
			t.Logf("shard %d: %v", sh, a.stats)
			g, err := tracecheck.Load(filepath.Join(specPath("x"), "..", "..", "testdata", "steer_queue"))
			if err != nil {
				t.Fatal(err)
			}
			for _, id := range a.ids {
				checkHistory(t, g, sessionHistory(t, a.s.Home, id), steerQueueHistory)
			}
		})
	}
}

type genPath struct {
	Trace []tracecheck.Step `json:"trace"`
}

func loadPaths(t *testing.T, spec string) []genPath {
	t.Helper()
	b, err := pathsJSON(spec)
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Paths []genPath `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		t.Fatal(err)
	}
	if len(f.Paths) == 0 {
		t.Fatal("paths.json has no paths")
	}
	return f.Paths
}

// walkSteerPath runs one generated path from Init, failing on the first step
// whose action errs, is refused by the adapter's own require, or leaves
// a state other than the path's.
func walkSteerPath(t *testing.T, a *steerQueueAdapter, acts map[string]map[string]fmbt.ActionFunc, n int, p genPath) {
	t.Helper()
	defer a.Cleanup()
	if err := a.Init(); err != nil {
		t.Errorf("path %d: Init: %v", n, err)
		return
	}
	var names []string
	for i, st := range p.Trace {
		want := roleState(st.State)
		if i > 0 {
			name := strings.TrimPrefix(st.Action, "Session#0.")
			names = append(names, name)
			f := acts["Session"][name]
			if f == nil {
				f = acts[""][name]
			}
			if f == nil {
				t.Errorf("path %d: no adapter action for %q", n, st.Action)
				return
			}
			_, err := f(a, forkArgs(name, roleState(p.Trace[i-1].State), want))
			if errors.Is(err, fmbt.ErrNotImplemented) {
				return
			}
			if err != nil {
				t.Errorf("path %d %v: %v", n, names, err)
				return
			}
			if a.gate.off {
				t.Errorf("path %d %v: the adapter refused a step the spec enables (its require disagrees)", n, names)
				return
			}
		}
		got, _ := a.GetState()
		if !reflect.DeepEqual(jsonRound(got), jsonRound(want)) {
			t.Errorf("path %d %v: state\n got %v\nwant %v", n, names, jsonRound(got), jsonRound(want))
			return
		}
	}
}

// roleState is a path state's Session#0 fields by bare name.
func roleState(s map[string]any) map[string]any {
	out := map[string]any{}
	for k, v := range s {
		if f, ok := strings.CutPrefix(k, "Session#0."); ok {
			out[f] = v
		}
	}
	return out
}

// forkArgs is the choice the runner would pass for a step that forks:
// which queued message RemoveQueued drops, whether Stop restores.
func forkArgs(action string, before, after map[string]any) []fmbt.Arg {
	switch action {
	case "RemoveQueued":
		b, _ := before["queue"].([]any)
		c, _ := after["queue"].([]any)
		for i := range b {
			if i >= len(c) || !reflect.DeepEqual(b[i], c[i]) {
				return []fmbt.Arg{{Name: "i", Value: i}}
			}
		}
	case "Stop":
		if before["status"] == "running" && before["draft"] == "" {
			return []fmbt.Arg{{Name: "restored", Value: after["draft"]}}
		}
	}
	return nil
}

func jsonRound(v any) any {
	b, _ := json.Marshal(v)
	var out any
	_ = json.Unmarshal(b, &out)
	return out
}

// A Finish that fails the model request leaves the row in error, which
// the flow never allows; the run must say so.
func TestSteerQueueCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSteerQueueAdapter(t)
	a.finishAsError = true
	if err := runMBT(t, "steer_queue", a, steerQueueActions(a), steerQueueOptions()); err == nil {
		t.Fatal("a run whose Finish fails the turn passed; the runner is not checking state")
	}
}
