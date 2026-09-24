//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
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

// specs/turn_lifecycle.fizz against a real serve: one web session
// prompted, its turn streamed, recorded, run through a native call and
// ended by done, an error or a dead child.
//
// The spec's fields are mostly the page's (send, stream, call, behind),
// so the adapter plays the page: it keeps what a page would keep, and
// moves it only on what the server tells it, over the same SSE stream
// and GET the page uses. Each action makes the server do one thing and
// then waits for the event or row that proves it did; a step the server
// never takes times out and fails the walk. status is always read off
// the server (the row, or StatusOf over the history the row is derived
// from). head and steer are derived: the Go stage computes them with the
// page's rule (turnHeader) so the runner can compare the whole role;
// the browser stage reads them off the DOM, which is where they can be
// wrong.
//
// The model is llm-control. A turn holds one model request at a time:
// Stream puts a fragment on screen, a release with text and an unknown
// tool records an assistant entry and a failed call (no running row) and
// asks again (Reply), and a release with a bash call waiting on a gate
// file is a native call that runs until the adapter opens the gate.
type turnLifecycleAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	id     string
	ids    []string
	events chan serve.Event
	stream *servetest.Stream

	n       int    // names and texts are unique across walks: one queue per serve
	text    string // this send's text
	held    string // the model request the turn holds, "" when none
	next    string // queued for the request after a native call
	gateF   string // the running native call's gate file
	fetched int64  // the last history seq the transcript has

	// The page's state, as the spec names it.
	status, send, streamS, call string
	behind                      bool
	callID                      string

	ran map[string]int // actions that ran past the gate, for the coverage log

	// failAsOK is the deliberate bug TestTurnLifecycleCatchesWrongAdapter
	// injects: Fail releases the turn as a success.
	failAsOK bool
}

func newTurnLifecycleAdapter(t *testing.T) *turnLifecycleAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &turnLifecycleAdapter{t: t, s: s, dir: control.Dir(s.Home)}
	t.Cleanup(func() {
		if a.stream != nil {
			a.stream.Close()
		}
	})
	return a
}

// on is gate.pass that also counts what ran: a walk that is only ever
// gated off checks nothing, and the log says so.
func (a *turnLifecycleAdapter) on(action string, enabled bool) bool {
	if !a.gate.pass(enabled) {
		return false
	}
	if a.ran == nil {
		a.ran = map[string]int{}
	}
	a.ran[action]++
	return true
}

func (a *turnLifecycleAdapter) name(prefix string) string {
	a.n++
	return fmt.Sprintf("%s%05d", prefix, a.n)
}

// Init opens a fresh idle session in the same serve and subscribes to
// its events the way the page does when it is selected.
func (a *turnLifecycleAdapter) Init() error {
	if a.stream != nil {
		a.stream.Close()
		a.stream = nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	st, err := a.s.Events(context.Background(), row.ID)
	if err != nil {
		return err
	}
	ch := make(chan serve.Event, 256)
	go func() {
		defer close(ch)
		for {
			ev, err := st.Next()
			if err != nil {
				return
			}
			ch <- ev
		}
	}()
	a.id, a.stream, a.events = row.ID, st, ch
	a.ids = append(a.ids, row.ID)
	a.text, a.held, a.next, a.gateF, a.fetched, a.callID = "", "", "", "", 0, ""
	a.status, a.send, a.streamS, a.call, a.behind = string(row.Status), "none", "", "none", false
	a.gate.reset()
	return nil
}

// Cleanup ends a turn the walk left open (by killing its child: the
// walk is over, nobody reads this session again) and empties the queue
// of turns nobody will take, so the next walk's requests take its own.
func (a *turnLifecycleAdapter) Cleanup() error {
	if a.gateF != "" {
		os.WriteFile(a.gateF, nil, 0o644)
	}
	if a.held != "" || a.next != "" || a.status == "running" {
		if err := a.kill(); err != nil {
			return err
		}
	}
	a.dropQueued()
	a.held, a.next, a.gateF = "", "", ""
	return nil
}

// dropQueued removes turns queued but not taken.
func (a *turnLifecycleAdapter) dropQueued() {
	left, _ := filepath.Glob(filepath.Join(a.dir, "*.json"))
	for _, f := range left {
		os.Remove(f)
	}
}

func (a *turnLifecycleAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

func (a *turnLifecycleAdapter) GetState() (map[string]any, error) {
	head, steer := turnHeader(a.status, a.send, a.streamS)
	return map[string]any{
		"status": a.status, "send": a.send, "stream": a.streamS, "call": a.call,
		"behind": a.behind, "head": head, "steer": steer,
	}, nil
}

// turnComposer and turnHeader are app.tsx's composerStatus and the
// header's rule over it (an error status wins over a send it never
// started), as the spec writes them.
func turnComposer(status, send, stream string) string {
	pending := send == "sending" || send == "accepted" || send == "recorded"
	switch {
	case status == "error":
		return ""
	case status != "running" && pending && send == "sending":
		return "Sending"
	case status != "running" && pending:
		return "Waiting"
	case status != "running":
		return ""
	case stream != "":
		return "Working"
	}
	return "Waiting"
}

func turnHeader(status, send, stream string) (string, bool) {
	c := turnComposer(status, send, stream)
	steer := status == "running" || c == "Waiting"
	if c == "Sending" || c == "Waiting" || status == "running" {
		if c == "" {
			c = "Working"
		}
		return c, steer
	}
	return strings.ToUpper(status[:1]) + status[1:], steer
}

// apply is the page's reducer for one event: streamed fragments, the
// entry that supersedes them, native call rows by id, and a turn's end
// or its child's death clearing what was live.
func (a *turnLifecycleAdapter) apply(ev serve.Event) {
	switch ev.Kind {
	case "assistant-delta", "thinking-delta":
		switch a.streamS {
		case "":
			a.streamS = "fresh"
		case "rec":
			a.streamS = "rec_fresh"
		}
	case "assistant", "thinking":
		if ev.Seq > 0 && (a.streamS == "fresh" || a.streamS == "rec_fresh") {
			a.streamS = "rec"
		}
	case "call":
		id, _ := ev.Extra["id"].(string)
		if id == "" {
			return
		}
		if ev.Extra["phase"] == "start" {
			a.call, a.callID = "native", id
		} else if id == a.callID && a.call == "native" {
			a.call = "native_done"
		}
	case "done", "cancelled", "exit":
		// What the spec says the page does when a turn ends, however it
		// ends; app.tsx clears native rows only on done/cancelled, which
		// the browser stage is there to catch.
		a.streamS, a.call = "", "none"
	}
}

// await applies events until one satisfies ok.
func (a *turnLifecycleAdapter) await(what string, ok func(serve.Event) bool) error {
	timer := time.NewTimer(actionTimeout)
	defer timer.Stop()
	for {
		select {
		case ev, open := <-a.events:
			if !open {
				return fmt.Errorf("waiting for %s: the event stream ended", what)
			}
			a.apply(ev)
			if ok(ev) {
				return nil
			}
		case <-timer.C:
			return fmt.Errorf("waiting for %s: no such event after %s", what, actionTimeout)
		}
	}
}

// settle applies what the server sends once an action's own event is in
// (activity, usage, a hook), so the next action starts from a quiet
// stream.
func (a *turnLifecycleAdapter) settle() {
	for {
		select {
		case ev, open := <-a.events:
			if !open {
				return
			}
			a.apply(ev)
		case <-time.After(150 * time.Millisecond):
			return
		}
	}
}

func kindIs(kinds ...string) func(serve.Event) bool {
	return func(ev serve.Event) bool {
		for _, k := range kinds {
			if ev.Kind == k {
				return true
			}
		}
		return false
	}
}

func (a *turnLifecycleAdapter) entries() ([]history.Entry, error) {
	return history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
}

// endStatus waits for the row to leave running and reads its status.
func (a *turnLifecycleAdapter) endStatus() error {
	row, err := waitRow(a.s, a.id, "the turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning })
	a.status = string(row.Status)
	return err
}

// kill is the archive kill, undone: the child dies, the session stays.
func (a *turnLifecycleAdapter) kill() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return err
	}
	if _, err := a.s.Archive(ctx, a.id); err != nil {
		return err
	}
	if row.Live {
		if err := a.await("the child's exit", kindIs("exit")); err != nil {
			return err
		}
	}
	_, err = a.s.Unarchive(ctx, a.id)
	return err
}

// --- the person ---

func (a *turnLifecycleAdapter) Send() error {
	if !a.on("Send", a.send == "none" && a.status != "running" && !a.behind) {
		return nil
	}
	a.text = "send " + a.name("s")
	a.send = "sending"
	return nil
}

func (a *turnLifecycleAdapter) Retry() error {
	if a.on("Retry", a.send == "failed") {
		a.send = "sending"
	}
	return nil
}

func (a *turnLifecycleAdapter) Edit() error {
	if a.on("Edit", a.send == "failed") {
		a.send = "none"
	}
	return nil
}

// --- the request ---

// Accept is POST /prompt answering 200. status is what the history
// says just before the new input: adopting an interrupted session must
// close its dangling turn (cancelled) before the input opens the next,
// so that prefix reads stopped, not an open turn.
func (a *turnLifecycleAdapter) Accept() error {
	if !a.on("Accept", a.send == "sending") {
		return nil
	}
	es, err := a.entries()
	if err != nil {
		return err
	}
	before := int64(0)
	if len(es) > 0 {
		before = es[len(es)-1].Seq
	}
	a.held = a.name("t")
	control.Queue(a.t, a.dir, a.held, control.Turn{Mode: "block"})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, a.text); err != nil {
		return err
	}
	a.send = "accepted"
	deadline := time.Now().Add(actionTimeout)
	for {
		es, err := a.entries()
		if err != nil {
			return err
		}
		for i, e := range es {
			if e.Seq <= before || e.Kind != "input" {
				continue
			}
			st, _ := serve.StatusOf(es[:i], true)
			a.status = string(st)
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("the accepted send %q was never recorded", a.text)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// Refuse is POST /prompt answering 409 because the session is archived:
// archive, send, unarchive. The session's status must survive it.
func (a *turnLifecycleAdapter) Refuse() error {
	if !a.on("Refuse", a.send == "sending") {
		return nil
	}
	if err := a.kill(); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if _, err := a.s.Archive(ctx, a.id); err != nil {
		return err
	}
	err := a.s.Prompt(ctx, a.id, a.text)
	var apiErr *servetest.APIError
	if !errors.As(err, &apiErr) || apiErr.Status != 409 {
		return fmt.Errorf("prompt to an archived session: want 409, got %v", err)
	}
	if _, err := a.s.Unarchive(ctx, a.id); err != nil {
		return err
	}
	a.send = "failed"
	a.settle()
	row, _, err := a.s.GetSession(ctx, a.id)
	a.status = string(row.Status)
	return err
}

// --- the child ---

// Input: the child records the send and asks the model. There is no
// "input" event; the page learns from the turn's first event, so the
// adapter waits for that and for the row and the history to agree.
func (a *turnLifecycleAdapter) Input() error {
	if !a.on("Input", a.send == "accepted") {
		return nil
	}
	control.WaitTaken(a.t, a.dir, a.held, actionTimeout)
	if err := a.await("the turn's first event", func(ev serve.Event) bool { return ev.Seq > 0 }); err != nil {
		return err
	}
	row, err := waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
	if err != nil {
		return err
	}
	a.status, a.send, a.behind = string(row.Status), "recorded", true
	a.settle()
	return nil
}

func (a *turnLifecycleAdapter) Delta() error {
	if !a.on("Delta", a.status == "running" && a.call == "none") {
		return nil
	}
	control.Stream(a.t, a.dir, a.held, "fragment ", actionTimeout)
	return a.await("the streamed fragment", kindIs("assistant-delta"))
}

// Reply records the assistant entry and goes on: the release answers
// with the text and a call to a tool that does not exist, which fails
// at once (a recorded call, never a running row) and asks again.
func (a *turnLifecycleAdapter) Reply() error {
	if !a.on("Reply", a.status == "running" && (a.streamS == "fresh" || a.streamS == "rec_fresh")) {
		return nil
	}
	next := a.name("t")
	control.Queue(a.t, a.dir, next, control.Turn{Mode: "block"})
	control.ReleaseWith(a.t, a.dir, a.held, control.Turn{Text: "reply " + next, Call: &control.Call{Name: "no_such_tool"}})
	if err := a.await("the assistant entry", func(ev serve.Event) bool { return ev.Kind == "assistant" && ev.Seq > 0 }); err != nil {
		return err
	}
	if err := a.await("the failed call", kindIs("call")); err != nil {
		return err
	}
	control.WaitTaken(a.t, a.dir, next, actionTimeout)
	a.held, a.behind = next, true
	a.settle()
	return nil
}

func (a *turnLifecycleAdapter) NativeCall() error {
	if !a.on("NativeCall", a.status == "running" && a.call == "none" && a.streamS == "") {
		return nil
	}
	a.next = a.name("t")
	a.gateF = filepath.Join(a.s.Root, a.next+".gate")
	control.Queue(a.t, a.dir, a.next, control.Turn{Mode: "block"})
	cmd := fmt.Sprintf("while [ ! -e %s ]; do sleep 0.05; done; echo gate open", a.gateF)
	control.ReleaseWith(a.t, a.dir, a.held, control.Turn{Call: &control.Call{Name: "bash", Args: map[string]any{"command": cmd}}})
	a.held = ""
	return a.await("the native call's running row", func(ev serve.Event) bool { return ev.Kind == "call" && ev.Extra["phase"] == "start" })
}

// endCall opens the running call's gate: its end is recorded and the
// model is asked again.
func (a *turnLifecycleAdapter) endCall() error {
	if err := os.WriteFile(a.gateF, nil, 0o644); err != nil {
		return err
	}
	id := a.callID
	if err := a.await("the call's recorded end", func(ev serve.Event) bool {
		return ev.Kind == "call" && ev.Seq > 0 && ev.Extra["id"] == id && ev.Extra["phase"] == nil
	}); err != nil {
		return err
	}
	control.WaitTaken(a.t, a.dir, a.next, actionTimeout)
	a.held, a.next, a.gateF, a.behind = a.next, "", "", true
	return nil
}

func (a *turnLifecycleAdapter) CallEnd() error {
	if !a.on("CallEnd", a.status == "running" && a.call == "native") {
		return nil
	}
	if err := a.endCall(); err != nil {
		return err
	}
	a.settle()
	return nil
}

// Finish ends the turn with done. A native call still running is let
// finish first: the engine waits on it before it asks the model again.
func (a *turnLifecycleAdapter) Finish() error {
	if !a.on("Finish", a.status == "running") {
		return nil
	}
	if a.gateF != "" {
		if err := a.endCall(); err != nil {
			return err
		}
	}
	return a.end(control.Turn{})
}

func (a *turnLifecycleAdapter) Fail() error {
	if !a.on("Fail", a.status == "running" && a.streamS == "" && a.call == "none") {
		return nil
	}
	if a.failAsOK {
		return a.end(control.Turn{})
	}
	return a.end(control.Turn{Mode: "error", Error: "model says no"})
}

func (a *turnLifecycleAdapter) end(turn control.Turn) error {
	if turn.Mode == "" {
		control.ReleaseWith(a.t, a.dir, a.held, control.Turn{Text: "finished " + a.held})
	} else {
		control.ReleaseWith(a.t, a.dir, a.held, turn)
	}
	a.held = ""
	if err := a.await("the turn's done", kindIs("done")); err != nil {
		return err
	}
	a.behind = true
	err := a.endStatus()
	a.settle()
	return err
}

// Crash kills the child mid-turn (the archive kill) and brings the
// session back; the open turn with no child reads interrupted.
func (a *turnLifecycleAdapter) Crash() error {
	if !a.on("Crash", a.status == "running") {
		return nil
	}
	if err := a.kill(); err != nil {
		return err
	}
	if a.gateF != "" {
		// The orphaned call, if it outlived its child, may exit now.
		os.WriteFile(a.gateF, nil, 0o644)
	}
	a.dropQueued()
	a.held, a.next, a.gateF, a.behind = "", "", "", true
	err := a.endStatus()
	a.settle()
	return err
}

// --- the page ---

// CatchUp is the GET the page runs after a recorded event. It checks
// that what the page is waiting on is really in the transcript: the
// send's input, the entry that supersedes the stream, the call's line.
func (a *turnLifecycleAdapter) CatchUp() error {
	if !a.on("CatchUp", a.behind) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, lines, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return err
	}
	var input, assistant, call bool
	for _, l := range lines {
		if l.Seq <= a.fetched {
			continue
		}
		a.fetched = l.Seq
		switch l.Kind {
		case "input":
			input = input || l.Text == a.text
		case "assistant", "thinking":
			assistant = true
		case "call":
			call = call || l.Data["id"] == a.callID
		}
	}
	if a.send == "recorded" && !input {
		return fmt.Errorf("catch-up: the recorded send %q is not in the transcript", a.text)
	}
	if (a.streamS == "rec" || a.streamS == "rec_fresh") && !assistant {
		return errors.New("catch-up: no entry supersedes the streamed text")
	}
	if a.call == "native_done" && !call {
		return fmt.Errorf("catch-up: the ended call %s is not in the transcript", a.callID)
	}
	a.behind = false
	switch a.streamS {
	case "rec":
		a.streamS = ""
	case "rec_fresh":
		a.streamS = "fresh"
	}
	if a.call == "native_done" {
		a.call = "none"
	}
	if a.send == "recorded" {
		a.send = "none"
	}
	return nil
}

var turnLifecycleActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Send":       action((*turnLifecycleAdapter).Send),
	"Retry":      action((*turnLifecycleAdapter).Retry),
	"Edit":       action((*turnLifecycleAdapter).Edit),
	"Accept":     action((*turnLifecycleAdapter).Accept),
	"Refuse":     action((*turnLifecycleAdapter).Refuse),
	"Input":      action((*turnLifecycleAdapter).Input),
	"Delta":      action((*turnLifecycleAdapter).Delta),
	"Reply":      action((*turnLifecycleAdapter).Reply),
	"NativeCall": action((*turnLifecycleAdapter).NativeCall),
	"CallEnd":    action((*turnLifecycleAdapter).CallEnd),
	"Finish":     action((*turnLifecycleAdapter).Finish),
	"Fail":       action((*turnLifecycleAdapter).Fail),
	"Crash":      action((*turnLifecycleAdapter).Crash),
	"CatchUp":    action((*turnLifecycleAdapter).CatchUp),
}}

func turnLifecycleOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

// turnLifecycleHistory reads the abstract trace off a transcript. The
// page-only steps leave nothing in history, so they are put back where
// the next recorded step needs them: a CatchUp before a send or a
// native call that a catch-up must have preceded, a Delta before the
// Reply it superseded. Refuse, Retry and Edit leave no trace and change
// nothing an accepted send does not.
func turnLifecycleHistory(entries []history.Entry) []tracecheck.Step {
	status := func(s string) map[string]any { return map[string]any{"Session#0.status": s} }
	steps := []tracecheck.Step{{Action: "Init", State: status("idle")}}
	add := func(action string, state map[string]any) {
		steps = append(steps, tracecheck.Step{Action: "Session#0." + action, State: state})
	}
	var open, failed, sent, behind, rec bool
	catchUp := func() {
		if behind {
			add("CatchUp", nil)
			behind, rec = false, false
		}
	}
	for i, e := range entries {
		switch e.Kind {
		case "input":
			if sent {
				catchUp()
			}
			sent, open, failed = true, true, false
			add("Send", nil)
			add("Accept", nil)
			add("Input", status("running"))
			behind = true
		case "cancelled":
			// The resumed child closing the turn its dead one left open.
			if open && e.Data["interrupted"] == true {
				add("Crash", status("interrupted"))
				open, behind = false, true
			}
		case "assistant":
			// Reply's entry is followed by its failed call; a Finish's
			// reply is the turn's last word.
			if i+1 < len(entries) && entries[i+1].Kind == "call" && entries[i+1].Data["error"] != nil {
				add("Delta", nil)
				add("Reply", nil)
				behind, rec = true, true
			}
		case "call":
			if e.Data["error"] == nil {
				if rec {
					catchUp()
				}
				add("NativeCall", nil)
				add("CallEnd", nil)
				behind = true
			}
		case "error":
			failed = true
		case "done":
			if !open {
				continue
			}
			if failed {
				if rec {
					catchUp()
				}
				add("Fail", status("error"))
			} else {
				add("Finish", status("done"))
			}
			open, behind, rec = false, true, false
		}
	}
	return steps
}

func init() { historyProjections["turn_lifecycle"] = turnLifecycleHistory }

// The fizzbee-mbt runner picks each next action at random among all
// fourteen, enabled or not, and stops checking a walk at the first
// disabled one; from Init only Send is enabled, so almost every walk
// ends at step one or two (a 300-walk run got one Accept past the gate
// and never an Input). The runner still walks Send/Accept/Refuse/Retry;
// the turn itself is walked by TestTurnLifecyclePaths, which drives the
// adapter down every path the generator wrote (paths.json covers every
// transition) and compares the whole role state at every node.
func TestTurnLifecycle(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newTurnLifecycleAdapter(t)
	err := runMBT(t, "turn_lifecycle", a, turnLifecycleActions, turnLifecycleOptions())
	t.Logf("actions run past the gate: %v", a.ran)
	if err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkTurnHistories(t, a)
}

func TestTurnLifecyclePaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newTurnLifecycleAdapter(t)
	err := walkPaths(a, "turn_lifecycle", "Session#0", turnLifecycleActions["Session"])
	t.Logf("actions run: %v", a.ran)
	if err != nil {
		t.Fatal(err)
	}
	checkTurnHistories(t, a)
}

// Every transcript the walks wrote is itself a path in the model.
func checkTurnHistories(t *testing.T, a *turnLifecycleAdapter) {
	t.Helper()
	g, err := tracecheck.Load(fizzCheck(t, "turn_lifecycle"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), turnLifecycleHistory)
	}
}

// A walk where Fail ends the turn as done must fail: otherwise the green
// walks above prove nothing. The runner's walks never reach Fail (see
// TestTurnLifecycle), so this drives the paths.
func TestTurnLifecycleCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newTurnLifecycleAdapter(t)
	a.failAsOK = true
	err := walkPaths(a, "turn_lifecycle", "Session#0", turnLifecycleActions["Session"])
	if err == nil {
		t.Fatal("paths whose Fail ends the turn as done passed; the walk is not checking state")
	}
	t.Logf("caught: %v", err)
}

// pathModel is what walkPaths drives: the adapter as the runner sees it.
type pathModel interface {
	Init() error
	Cleanup() error
	GetState() (map[string]any, error)
}

// walkPaths drives m down every path in testdata/<spec>/paths.json and,
// after Init and after each action, compares m's state with the node
// the path reaches. It stops at the first path that disagrees.
func walkPaths(m pathModel, spec, role string, actions map[string]fmbt.ActionFunc) error {
	raw, err := pathsJSON(spec)
	if err != nil {
		return err
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		return err
	}
	for pi, p := range doc.Paths {
		var names []string
		for _, st := range p.Trace {
			names = append(names, strings.TrimPrefix(st.Action, role+"."))
		}
		fail := func(i int, format string, args ...any) error {
			m.Cleanup()
			return fmt.Errorf("path %d %v, step %d (%s): %s", pi, names, i, names[i], fmt.Sprintf(format, args...))
		}
		for i, st := range p.Trace {
			if i == 0 {
				if err := m.Init(); err != nil {
					return fail(i, "%v", err)
				}
			} else {
				f, ok := actions[names[i]]
				if !ok {
					return fail(i, "no such action")
				}
				if _, err := f(m, nil); err != nil {
					return fail(i, "%v", err)
				}
			}
			got, err := m.GetState()
			if err != nil {
				return fail(i, "%v", err)
			}
			for k, v := range st.State {
				field, ok := strings.CutPrefix(k, role+".")
				if !ok {
					continue
				}
				if !reflect.DeepEqual(got[field], v) {
					return fail(i, "%s: model %v, server %v (whole state %v)", field, v, got[field], got)
				}
			}
		}
		if err := m.Cleanup(); err != nil {
			return fmt.Errorf("path %d cleanup: %w", pi, err)
		}
	}
	return nil
}
