//go:build !windows

package mbt

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
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

// specs/cross_tab_stop_vs_send.fizz against a real serve: tab A sends to
// a session whose child is gone while tab B, whose row still says
// running, presses Stop (its thread Stop or the palette's).
//
// The spec's steps are split finer than one HTTP request, so the
// adapter makes each one a state it can hold:
//
//   - The ensure gap (a child spawned, no prompt written, unread false)
//     lasts microseconds inside Supervisor.Send. SendA makes the same
//     server state durable: with llm-control's start hold on, a
//     "/think" line (POST /effort) makes serve ensure a child, which
//     parks while it boots and reads nothing. For Interrupt that child
//     is the one Send just spawned: kids[id] is set, unread is false.
//     WriteA is then the real POST of the prompt to that child.
//   - Take is ReleaseStart: the parked child boots and reads its pipe.
//     ChildSignal before a take is ReleaseStart with no prompt written
//     (the child exits on its SIGINT), and ExitStart with one (see
//     ChildSignal for why the death is not the SIGINT's own). Every
//     other interrupt is the real
//     POST, except a Stop on a running turn, whose SIGINT is "on its
//     way" until ChildSignal: the POST is made there, as in
//     stop_interrupt_test.go.
//   - A held Stop released at the take cancels the turn at once: Take,
//     then ChildSignal, in one. The adapter checks that at Take and
//     reports the spec's in-between state until ChildSignal (`ahead`).
//   - WriteA to a child that already died of an early SIGINT is not
//     driven: the real Send holds that dead child and gets EPIPE, but a
//     POST made now would ensure a fresh one. arow "failed" is the
//     adapter's record there; everything else stays read off serve.
//   - HoldExpires is not driven: it takes holdLimit (20 s) and the
//     SIGINT and the unread line then reach the child together, which
//     wins is a race (stop_interrupt_test.go probed it). A walk that
//     takes it follows the spec from there (`detached`), counted in
//     the log.
//
// Init's previous turn replied before it was stopped, so the page's
// restore (stoppedPrompt) can only ever find this walk's prompt: a
// restore of the setup turn's would be a different flow.
//
// phase is read off the row (no live child is "none"), status off the
// row, taken off the transcript; held, timer and sigint are serve's
// internals, which the adapter keeps; arow and B's fields are the
// pages', computed from what the requests returned and what serve
// recorded, the way app.tsx does.

// crossTabBound is how long a SIGINT may take to end a child or a turn.
const crossTabBound = 10 * time.Second

type crossTabShadow struct {
	phase, status, sigint, arow, bstop, bdraft string
	taken, held, timer, bview, brestore, bpal  bool
}

func (s crossTabShadow) state() map[string]any {
	return map[string]any{
		"phase": s.phase, "status": s.status, "taken": s.taken, "held": s.held, "timer": s.timer,
		"sigint": s.sigint, "arow": s.arow, "bview": s.bview, "bstop": s.bstop,
		"brestore": s.brestore, "bpal": s.bpal, "bdraft": s.bdraft,
	}
}

type crossTabAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string
	gate gate

	id      string
	ids     []string
	sh      crossTabShadow
	n       int
	name    string // the llm turn A's prompt will take
	sentSeq int64  // the transcript's last seq once Init settled

	ahead      bool
	detached   bool
	detachedBy map[string]int

	// signalAsFinish is TestCrossTabStopVsSendCatchesWrongAdapter's
	// bug: ChildSignal on a running turn finishes it instead of
	// stopping it.
	signalAsFinish bool
}

func newCrossTabAdapter(t *testing.T) *crossTabAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &crossTabAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

func (a *crossTabAdapter) turnName(p string) string {
	a.n++
	return fmt.Sprintf("%s%05d", p, a.n)
}

// Init is a session whose last turn replied, was stopped and whose
// child has exited: B last polled while it ran.
func (a *crossTabAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	a.sh = crossTabShadow{phase: "none", status: "stopped", bview: true}
	a.ahead, a.detached = false, false
	a.gate.reset()

	first, next := a.turnName("x"), a.turnName("x")
	control.Queue(a.t, a.dir, first, control.Turn{Mode: "block"})
	if err := a.s.Prompt(ctx, a.id, "setup "+first); err != nil {
		return err
	}
	control.WaitTaken(a.t, a.dir, first, actionTimeout)
	// The reply is recorded, then a call that fails at once asks the
	// model again, which holds: a running turn that has replied.
	control.Queue(a.t, a.dir, next, control.Turn{Mode: "block"})
	control.ReleaseWith(a.t, a.dir, first, control.Turn{Text: "reply " + first, Call: &control.Call{Name: "no_such_tool"}})
	control.WaitTaken(a.t, a.dir, next, actionTimeout)
	if _, err := a.wait("the setup turn's reply", actionTimeout, func(r serve.Row, l []serve.Line) bool {
		return r.Status == serve.StatusRunning && hasKind(l, "assistant")
	}); err != nil {
		return err
	}
	if err := a.s.Interrupt(ctx, a.id); err != nil {
		return fmt.Errorf("stop the setup turn: %w", err)
	}
	if _, err := a.wait("the setup turn to stop and its child to exit", crossTabBound, func(r serve.Row, l []serve.Line) bool {
		return r.Status == serve.StatusStopped && !r.Live && lastCloseCancelled(l)
	}); err != nil {
		return err
	}
	control.Release(a.t, a.dir, next)
	_, lines, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return err
	}
	a.sentSeq = lastSeq(lines)
	return nil
}

func hasKind(lines []serve.Line, kind string) bool {
	for _, l := range lines {
		if l.Kind == kind {
			return true
		}
	}
	return false
}

// Cleanup lets a parked child go, archives (kills) the session and
// empties the llm queue, so nothing a walk queued answers the next.
func (a *crossTabAdapter) Cleanup() error {
	if a.detached {
		if a.detachedBy == nil {
			a.detachedBy = map[string]int{}
		}
		a.detachedBy["HoldExpires"]++
	}
	control.ReleaseStart(a.t, a.dir)
	ctx, cancel := actionCtx()
	defer cancel()
	if _, err := a.s.Archive(ctx, a.id); err != nil {
		return err
	}
	ents, _ := os.ReadDir(a.dir)
	for _, e := range ents {
		n := e.Name()
		switch {
		case strings.HasSuffix(n, ".json"):
			os.Remove(filepath.Join(a.dir, n))
		case strings.HasSuffix(n, ".taken"):
			if _, err := os.Stat(filepath.Join(a.dir, strings.TrimSuffix(n, ".taken")+".release")); err != nil {
				control.Release(a.t, a.dir, strings.TrimSuffix(n, ".taken"))
			}
		}
	}
	return nil
}

func (a *crossTabAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

func (a *crossTabAdapter) GetState() (map[string]any, error) {
	st := a.sh.state()
	if a.detached || a.ahead {
		return st, nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, lines, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return nil, err
	}
	st["status"] = string(row.Status)
	st["taken"] = inputAfter(lines, a.sentSeq)
	switch {
	case !row.Live:
		st["phase"] = "none"
	case a.sh.phase == "none":
		st["phase"] = "a live child"
	}
	return st, nil
}

func (a *crossTabAdapter) wait(what string, d time.Duration, ok func(serve.Row, []serve.Line) bool) (serve.Row, error) {
	deadline := time.Now().Add(d)
	for {
		ctx, cancel := actionCtx()
		row, lines, err := a.s.GetSession(ctx, a.id)
		cancel()
		if err == nil && ok(row, lines) {
			return row, nil
		}
		if time.Now().After(deadline) {
			return row, fmt.Errorf("waiting %s for %s: row %s live=%v, transcript %v (err %v)", d, what, row.Status, row.Live, lineKinds(lines), err)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// real says whether the step is driven against serve.
func (a *crossTabAdapter) real() bool { return !a.detached }

// interrupt is the spec's interrupt(): the real POST where it is made
// now, checked against what the spec says it returns.
func (a *crossTabAdapter) interrupt() (bool, error) {
	sh := &a.sh
	switch sh.phase {
	case "none":
		if a.real() {
			ctx, cancel := actionCtx()
			defer cancel()
			var e *servetest.APIError
			if err := a.s.Interrupt(ctx, a.id); !errors.As(err, &e) || e.Status != http.StatusNotFound {
				return false, fmt.Errorf("stop with no child: got %v, want 404", err)
			}
		}
		return false, nil
	case "ensuring", "unread":
		if a.real() {
			ctx, cancel := actionCtx()
			defer cancel()
			if err := a.s.Interrupt(ctx, a.id); err != nil {
				return false, fmt.Errorf("stop a %s child: %w", sh.phase, err)
			}
		}
		if sh.phase == "ensuring" {
			if sh.sigint == "" {
				sh.sigint = "early"
			}
		} else {
			sh.held, sh.timer = true, true
		}
	default:
		// A running turn: the POST is made at ChildSignal.
		if sh.sigint == "" {
			sh.sigint = "sent"
		}
	}
	return true, nil
}

func (a *crossTabAdapter) SendA() error {
	sh := &a.sh
	if !a.gate.pass(sh.arow == "" && sh.phase == "none") {
		return nil
	}
	if a.real() {
		control.HoldStart(a.t, a.dir)
		ctx, cancel := actionCtx()
		defer cancel()
		if err := a.s.Effort(ctx, a.id, "medium"); err != nil {
			return fmt.Errorf("ensure a child: %w", err)
		}
		control.WaitHeld(a.t, a.dir, actionTimeout)
	}
	sh.phase, sh.arow = "ensuring", "sending"
	return nil
}

func (a *crossTabAdapter) WriteA() error {
	sh := &a.sh
	if !a.gate.pass(sh.arow == "sending") {
		return nil
	}
	if sh.phase == "none" {
		sh.arow = "failed" // not driven; see the file comment
		return nil
	}
	if a.real() {
		a.name = a.turnName("a")
		control.Queue(a.t, a.dir, a.name, control.Turn{Mode: "block", Text: "finished " + a.name})
		ctx, cancel := actionCtx()
		defer cancel()
		if err := a.s.Prompt(ctx, a.id, "turn "+a.name); err != nil {
			return fmt.Errorf("write A's prompt to the booting child: %w", err)
		}
	}
	sh.phase, sh.arow = "unread", "accepted"
	return nil
}

func (a *crossTabAdapter) Take() error {
	sh := &a.sh
	if !a.gate.pass(sh.phase == "unread" && sh.sigint == "") {
		return nil
	}
	if a.real() {
		control.ReleaseStart(a.t, a.dir)
		if sh.held {
			if _, err := a.wait("the held stop to cancel the taken prompt", crossTabBound, func(r serve.Row, l []serve.Line) bool {
				return inputAfter(l, a.sentSeq) && lastCloseCancelled(l) && r.Status == serve.StatusStopped && !r.Live
			}); err != nil {
				return err
			}
			a.ahead = true
		} else {
			if _, err := a.wait("the prompt to become a running turn", actionTimeout, func(r serve.Row, l []serve.Line) bool {
				return inputAfter(l, a.sentSeq) && r.Status == serve.StatusRunning
			}); err != nil {
				return err
			}
			control.WaitTaken(a.t, a.dir, a.name, actionTimeout)
		}
	}
	sh.phase, sh.status, sh.taken, sh.timer = "turn", "running", true, false
	if sh.held {
		sh.held, sh.sigint = false, "sent"
	}
	return nil
}

func (a *crossTabAdapter) HoldExpires() error {
	sh := &a.sh
	if !a.gate.pass(sh.timer) {
		return nil
	}
	a.detached = true
	sh.timer, sh.held = false, false
	if sh.sigint == "" {
		sh.sigint = "expired"
	}
	return nil
}

func (a *crossTabAdapter) ChildSignal() error {
	sh := &a.sh
	if !a.gate.pass(sh.sigint != "" && sh.phase != "none") {
		return nil
	}
	switch {
	case !a.real():
	case a.ahead:
		// Take already saw the cancel and the exit.
		a.ahead = false
	case sh.phase == "turn":
		if a.signalAsFinish {
			control.Release(a.t, a.dir, a.name)
			if _, err := a.wait("the turn to finish", actionTimeout, func(r serve.Row, _ []serve.Line) bool { return r.Status != serve.StatusRunning }); err != nil {
				return err
			}
			break
		}
		ctx, cancel := actionCtx()
		defer cancel()
		if err := a.s.Interrupt(ctx, a.id); err != nil {
			return fmt.Errorf("stop the running turn: %w", err)
		}
		if _, err := a.wait("the stop to cancel the running turn", crossTabBound, func(r serve.Row, l []serve.Line) bool {
			return lastCloseCancelled(l) && r.Status == serve.StatusStopped && !r.Live
		}); err != nil {
			return err
		}
	case sh.phase == "unread":
		// Not ReleaseStart: the parked child is past main's
		// signal.Notify, so it would keep the SIGINT, boot, take the
		// prompt and only then cancel it (seen on every such path). The
		// real gap is microseconds after exec, before that handler,
		// where the SIGINT kills the child before it reads a line; this
		// is that death, at the same point of the boot.
		control.ExitStart(a.t, a.dir)
		if _, err := a.wait("the booting child to die", crossTabBound, func(r serve.Row, _ []serve.Line) bool { return !r.Live }); err != nil {
			return err
		}
		control.ReleaseStart(a.t, a.dir)
	default:
		// No line in its pipe: the parked child boots with the SIGINT
		// it was sent and exits on it, reading nothing but "/think".
		control.ReleaseStart(a.t, a.dir)
		if _, err := a.wait("the booting child to die of its SIGINT", crossTabBound, func(r serve.Row, _ []serve.Line) bool { return !r.Live }); err != nil {
			return err
		}
	}
	if sh.phase == "turn" {
		sh.status = "stopped"
	}
	sh.phase, sh.sigint, sh.held, sh.timer = "none", "", false, false
	return nil
}

func (a *crossTabAdapter) PollA() error {
	sh := &a.sh
	if !a.gate.pass(sh.arow == "accepted" && sh.taken) {
		return nil
	}
	sh.arow = "landed"
	return nil
}

func (a *crossTabAdapter) StopB() error {
	sh := &a.sh
	if !a.gate.pass(sh.bview && sh.bstop != "stopping") {
		return nil
	}
	sh.bstop, sh.brestore = "stopping", true
	ok, err := a.interrupt()
	if err != nil {
		return err
	}
	if !ok {
		sh.bstop = "failed"
	}
	return nil
}

func (a *crossTabAdapter) PaletteStopB() error {
	sh := &a.sh
	if !a.gate.pass(sh.bview) {
		return nil
	}
	if _, err := a.interrupt(); err != nil {
		return err
	}
	sh.bpal = true
	return nil
}

// PollB is B's list poll: its row takes serve's status, and once it is
// not live the Thread's effects run, the restore reading the real
// transcript through stoppedPrompt as app.tsx does.
func (a *crossTabAdapter) PollB() error {
	sh := &a.sh
	if !a.gate.pass(sh.bview != (sh.status == "running")) {
		return nil
	}
	sh.bview = sh.status == "running"
	if sh.bview {
		return nil
	}
	sh.bstop, sh.bpal = "", false
	restore := sh.taken && sh.status == "stopped"
	if a.real() && !a.ahead {
		ctx, cancel := actionCtx()
		defer cancel()
		_, lines, err := a.s.GetSession(ctx, a.id)
		if err != nil {
			return err
		}
		restore = stoppedPrompt(lines) != ""
	}
	if sh.brestore && restore && sh.bdraft == "" {
		sh.brestore, sh.bdraft = false, "restored"
	}
	return nil
}

var crossTabActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"SendA":        action((*crossTabAdapter).SendA),
	"WriteA":       action((*crossTabAdapter).WriteA),
	"Take":         action((*crossTabAdapter).Take),
	"HoldExpires":  action((*crossTabAdapter).HoldExpires),
	"ChildSignal":  action((*crossTabAdapter).ChildSignal),
	"PollA":        action((*crossTabAdapter).PollA),
	"StopB":        action((*crossTabAdapter).StopB),
	"PaletteStopB": action((*crossTabAdapter).PaletteStopB),
	"PollB":        action((*crossTabAdapter).PollB),
}, "": {
	// deadlock_detection is off, and fizz links a state with nothing
	// enabled (a lost prompt, B settled) to itself as "end": nothing
	// happens. The runner offers it too, and a missing action panics.
	"end": func(any, []fmbt.Arg) (any, error) { return nil, nil },
}}

func crossTabOptions() map[string]any {
	return map[string]any{"max-seq-runs": 40, "max-actions": 8, "max-parallel-runs": 0}
}

// crossTabHistory reads the abstract trace off a transcript. The first
// turn is Init's setup and is skipped. A's prompt that became a turn is
// SendA, WriteA and Take; a cancel after it is a Stop reaching the
// running turn and the child's death of it; a finish has no step here.
// Pages' steps, holds and a
// prompt lost unread leave nothing here, and the graph has the paths
// that never took them.
func crossTabHistory(entries []history.Entry) []tracecheck.Step {
	q := func(kv ...any) map[string]any {
		m := map[string]any{}
		for i := 0; i < len(kv); i += 2 {
			m["Session#0."+kv[i].(string)] = kv[i+1]
		}
		return m
	}
	step := func(action string, state map[string]any) tracecheck.Step {
		return tracecheck.Step{Action: "Session#0." + action, State: state}
	}
	steps := []tracecheck.Step{{Action: "Init", State: q("status", "stopped", "phase", "none")}}
	inputs, open := 0, false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			if e.Data["steer"] != nil {
				continue
			}
			inputs++
			if inputs == 2 {
				steps = append(steps, step("SendA", nil), step("WriteA", nil),
					step("Take", q("status", "running", "taken", true)))
				open = true
			}
		case "cancelled":
			if open {
				steps = append(steps, step("StopB", nil), step("ChildSignal", q("status", "stopped", "phase", "none")))
				open = false
			}
		case "done":
			// A finish: no step of this flow, so the check refuses it.
			if open {
				steps = append(steps, step("Finish", q("status", "done")))
				open = false
			}
		}
	}
	return steps
}

func init() { historyProjections["cross_tab_stop_vs_send"] = crossTabHistory }

// walkCrossTabPaths drives every generated path and compares the
// adapter's state with the path's after every step; every path runs,
// so one run reports every divergence.
func walkCrossTabPaths(t *testing.T, a *crossTabAdapter, cover tracecheck.Cover) error {
	t.Helper()
	b, err := pathsJSONCover("cross_tab_stop_vs_send", cover)
	if err != nil {
		return err
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		return err
	}
	a.detachedBy = nil
	var errs []error
	for i, p := range doc.Paths {
		if err := a.walk(p.Trace); err != nil {
			var names []string
			for _, s := range p.Trace {
				names = append(names, strings.TrimPrefix(s.Action, "Session#0."))
			}
			errs = append(errs, fmt.Errorf("path %d %v: %w", i, names, err))
		}
	}
	t.Logf("cross_tab_stop_vs_send: %d paths (%s), left what the adapter drives: %v", len(doc.Paths), cover, a.detachedBy)
	return errors.Join(errs...)
}

func (a *crossTabAdapter) walk(trace []tracecheck.Step) (err error) {
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	for j, s := range trace {
		if s.Action == "Init" {
			err = a.Init()
		} else {
			role, name := "", s.Action
			if n, ok := strings.CutPrefix(s.Action, "Session#0."); ok {
				role, name = "Session", n
			}
			f, ok := crossTabActions[role][name]
			if !ok {
				return fmt.Errorf("step %d: no adapter action %q", j, name)
			}
			_, err = f(a, nil)
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", j, s.Action, err)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w", j, s.Action, err)
		}
		var diff []string
		for k, v := range s.State {
			f, ok := strings.CutPrefix(k, "Session#0.")
			if ok && fmt.Sprint(got[f]) != fmt.Sprint(v) {
				diff = append(diff, fmt.Sprintf("%s: spec %v, got %v", f, v, got[f]))
			}
		}
		if len(diff) > 0 {
			sort.Strings(diff)
			return fmt.Errorf("step %d (%s): %s", j, s.Action, strings.Join(diff, "; "))
		}
	}
	return nil
}

func checkCrossTabHistories(t *testing.T, a *crossTabAdapter) {
	t.Helper()
	g, err := tracecheck.Load(fizzCheck(t, "cross_tab_stop_vs_send"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), crossTabHistory)
	}
	t.Logf("trace-checked %d sessions' histories", len(a.ids))
}

// TestCrossTabStopVsSendPaths walks the generated paths against a real
// serve: the cover. It needs no graph server.
func TestCrossTabStopVsSendPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newCrossTabAdapter(t)
	if err := walkCrossTabPaths(t, a, envCover()); err != nil {
		t.Fatalf("spec path: %v", err)
	}
	checkCrossTabHistories(t, a)
}

// TestCrossTabStopVsSend is fizzbee-mbt's random walks over the same
// adapter (part of the exhaustive MODEL_COVER=transitions run).
func TestCrossTabStopVsSend(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newCrossTabAdapter(t)
	if err := runMBT(t, "cross_tab_stop_vs_send", a, crossTabActions, crossTabOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkCrossTabHistories(t, a)
}

// The projection is only a check if a transcript the model forbids is
// refused: A's prompt recorded and then finished, which this flow never
// does (it ends at the Stop).
func TestCrossTabStopVsSendHistoryProjection(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	g, err := tracecheck.Load(fizzCheck(t, "cross_tab_stop_vs_send"))
	if err != nil {
		t.Fatal(err)
	}
	in := history.Entry{Kind: "input", Data: map[string]any{"text": "hi"}}
	cancelled, done := history.Entry{Kind: "cancelled"}, history.Entry{Kind: "done"}
	setup := func(more ...history.Entry) []history.Entry {
		return append([]history.Entry{{Kind: "meta"}, in, {Kind: "assistant"}, cancelled, done}, more...)
	}
	for name, es := range map[string][]history.Entry{
		"lost":    setup(),
		"running": setup(in),
		"stopped": setup(in, cancelled, done),
	} {
		if v := g.Check(crossTabHistory(es)); v != nil {
			t.Errorf("%s: %v", name, v)
		}
	}
	if v := g.Check(crossTabHistory(setup(in, done))); v == nil {
		t.Error("a finished turn passed the trace check")
	} else {
		t.Logf("refused as expected: %v", v)
	}
}

// A ChildSignal wired to finish the running turn instead of stopping it
// must fail: the row says done where the spec says stopped. It shows on
// one link, which a cover of the states need not take.
func TestCrossTabStopVsSendCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newCrossTabAdapter(t)
	a.signalAsFinish = true
	err := walkCrossTabPaths(t, a, tracecheck.CoverTransitions)
	if err == nil {
		t.Fatal("paths whose ChildSignal finishes the turn passed; the adapter is not checking state")
	}
	t.Logf("caught as expected: %v", err)
}
