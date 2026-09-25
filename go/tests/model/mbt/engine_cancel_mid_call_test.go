//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/operation"
	"github.com/unreallabsai/unreal-agent/harness/tool"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/unreal/boughcall"
	"github.com/andreylukin/bough/internal/unreal/fake"
	"github.com/andreylukin/bough/internal/unreal/session"
	"github.com/andreylukin/bough/internal/unreal/toolreg"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/engine_cancel_mid_call.fizz against the engine itself: one
// in-process session.Runtime (the actor, the Gate, the coordinator, the
// harness store, boughcall and the history it writes), driven the way
// the TUI drives it: Submit is Enter, Steer is Enter mid-turn, Cancel is
// Esc. serve cannot reach this flow: its Stop is a SIGINT and the child
// exits after the cancel's done (stop_interrupt covers that), so late
// rows, a prompt sent while cancelling and the leak paths exist only in
// the TUI and in-process.
//
// Every walk runs in its own synctest bubble. The engine's timers
// (cancelWait 1 s, pendingCancelFor 2 s, turn settle) run on the
// bubble's clock, which moves only when the adapter sleeps, and
// synctest.Wait after each action lets every goroutine the action woke
// settle before the state is read. So the spec's fair timer steps
// (CancelWaitExpires, PendingCancelLapses) happen exactly when the walk
// takes them and never in between, and a walk is deterministic.
//
// What is real and what is the adapter's:
//   - The model is ecLLM, an adapter that holds every request until the
//     walk answers it: req is "a request is held". A request the Gate
//     mutes never reaches it.
//   - The call is the "hold" tool, run by the real boughcall handler.
//     ecHandler wraps it and holds back the cancel the actor sends for
//     the walk's call until CallReportsCanceled (then boughcall cancels
//     the tool, which reports canceled) or CallIgnoresCancel (the tool
//     finishes first and reports ok: the race of a call ending as Esc
//     lands). Without that hold, boughcall reports a cancelled call
//     canceled within its 2 s grace, and "late" could not outlast it.
//   - The adopted job of the spec's Init is a hold call from a setup
//     turn that the turn settle adopted; JobFinishes finishes it.
//   - turn, wake, turns, closes, req, row, shows, stray, pending and job
//     are read off history, the model and the tools after every step,
//     and leak is a request that really carried a cancelled call's
//     result to the model. call is read as running or not; whether a
//     running call is its cancel's or outlived it, and parked, waiting,
//     quiet, pending_cancel and the steer's stage (Gate and actor
//     internals no reader sees) come from ecShadow, the spec ported to
//     Go, which is itself checked against the graph at every step.
//   - A steer the spec has "typed" is one the person has typed and not
//     sent: Runtime.Steer is called at SteerLands, or at Stop, just
//     before the cancel, as a steer and Esc pressed together.
//   - Time: see ecStep, passGrace and inThisCancel. A walk that needs
//     more time than an engine timer it has not taken allows (an idle
//     Esc's 2 s outlasting a harness grace plus a cancelWait) cannot
//     happen in the engine and is counted, not failed.

const ecSpec = "engine_cancel_mid_call"

// ---- the model ----

// ecLLM is the llm row's engine adapter: every request waits for the
// walk to answer it or for its context to end.
type ecLLM struct {
	mu   sync.Mutex
	live []*ecReq
	seen []ullm.Request
}

type ecReq struct {
	req    ullm.Request
	answer chan ullm.Response
}

func (l *ecLLM) AgentAdapter(agentllm.Options) (agentllm.Adapter, error) { return l, nil }
func (l *ecLLM) Provider() string                                        { return "fake" }
func (l *ecLLM) Model() string                                           { return "fake-model" }
func (l *ecLLM) Close() error                                            { return nil }

func (l *ecLLM) Respond(ctx context.Context, req ullm.Request, _ ullm.RequestOptions) (ullm.Response, error) {
	r := &ecReq{req: req, answer: make(chan ullm.Response, 1)}
	l.mu.Lock()
	l.live = append(l.live, r)
	l.seen = append(l.seen, req)
	l.mu.Unlock()
	select {
	case resp := <-r.answer:
		return resp, nil
	case <-ctx.Done():
		l.mu.Lock()
		l.live = slices.DeleteFunc(l.live, func(x *ecReq) bool { return x == r })
		l.mu.Unlock()
		return ullm.Response{}, ctx.Err()
	}
}

func (l *ecLLM) held() int {
	l.mu.Lock()
	defer l.mu.Unlock()
	return len(l.live)
}

func (l *ecLLM) requests() []ullm.Request {
	l.mu.Lock()
	defer l.mu.Unlock()
	return slices.Clone(l.seen)
}

// answer replies to the one request held.
func (l *ecLLM) answer(out ...ullm.Item) error {
	l.mu.Lock()
	if len(l.live) != 1 {
		n := len(l.live)
		l.mu.Unlock()
		return fmt.Errorf("%d model requests held, want 1", n)
	}
	r := l.live[0]
	l.live = nil
	l.mu.Unlock()
	r.answer <- ullm.Response{Stop: ullm.StopComplete, Output: out, Usage: ullm.Usage{InputTokens: 10, OutputTokens: 1}}
	return nil
}

// ---- the tool and its handler ----

// ecTools is the "hold" tool: a call runs until the walk finishes it or
// its context is cancelled.
type ecTools struct {
	mu      sync.Mutex
	running map[string]chan struct{}
}

func (k *ecTools) tool() agenttools.Tool {
	obj := agenttools.Object(nil, map[string]any{"text": agenttools.Prop("string", "text")})
	return agenttools.Tool{Name: "hold", Description: "runs until released", Schema: obj,
		Detail: func(args json.RawMessage) string {
			var a struct{ Text string }
			_ = json.Unmarshal(args, &a)
			return a.Text
		},
		Call: func(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
			done := make(chan struct{})
			k.mu.Lock()
			k.running[c.ID] = done
			k.mu.Unlock()
			defer func() {
				k.mu.Lock()
				delete(k.running, c.ID)
				k.mu.Unlock()
			}()
			select {
			case <-done:
				return agenttools.Result{Text: "held and released"}, nil
			case <-ctx.Done():
				return agenttools.Result{}, ctx.Err()
			}
		}}
}

func (k *ecTools) isRunning(id string) bool {
	k.mu.Lock()
	defer k.mu.Unlock()
	_, ok := k.running[id]
	return ok
}

func (k *ecTools) finish(id string) error {
	k.mu.Lock()
	done, ok := k.running[id]
	k.mu.Unlock()
	if !ok {
		return fmt.Errorf("call %s is not running", id)
	}
	close(done)
	return nil
}

// ecHandler is boughcall with the cancel of a call held back until the
// walk says how the call answers it.
type ecHandler struct {
	*boughcall.Handler
	mu       sync.Mutex
	ops      map[string]operation.ID // call id → its op
	asked    map[string]string       // call id → the cancel's reason, held
	canceled map[string]bool         // call ids a cancel was sent for, ever
}

func (h *ecHandler) AddRemoteJob(op operation.Operation) error {
	if st, err := operation.DecodeRemoteJobState(op); err == nil {
		var pl toolreg.Plan
		if json.Unmarshal(st.Plan.Data, &pl) == nil && pl.Call != "" {
			h.mu.Lock()
			h.ops[pl.Call] = op.ID
			h.mu.Unlock()
		}
	}
	return h.Handler.AddRemoteJob(op)
}

func (h *ecHandler) CancelRemoteJob(id operation.ID, reason string) error {
	h.mu.Lock()
	defer h.mu.Unlock()
	for call, op := range h.ops {
		if op == id {
			h.asked[call] = reason
			h.canceled[call] = true
		}
	}
	return nil
}

// release sends the held cancel of call on to boughcall.
func (h *ecHandler) release(call string) error {
	h.mu.Lock()
	reason, ok := h.asked[call]
	op := h.ops[call]
	delete(h.asked, call)
	h.mu.Unlock()
	if !ok {
		return fmt.Errorf("no cancel was sent for call %s", call)
	}
	return h.Handler.CancelRemoteJob(op, reason)
}

func (h *ecHandler) wasCanceled(call string) bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.canceled[call]
}

// ---- the spec, in Go ----

// ecShadow is specs/engine_cancel_mid_call.fizz's Engine role, action
// for action. The walk checks it against the graph at every step, so a
// port that drifts from the spec fails as such; the adapter reports its
// hidden fields and checks the rest against the engine.
type ecShadow struct {
	Turn          string
	Wake          bool
	Turns, Closes int
	Req           bool
	Call          string
	Parked        bool
	Waiting       bool
	Row, Shows    string
	Steer         string
	Pending       bool
	PendingCancel bool
	Job           string
	Stray, Leak   bool
	Quiet         bool

	// cancels counts the cancelled entries the steps so far write; not
	// spec state (the history projection matches entries against it).
	cancels int
}

func newEcShadow() ecShadow {
	return ecShadow{Turn: "idle", Call: "none", Steer: "none", Job: "running"}
}

func (s ecShadow) state() map[string]any {
	return map[string]any{
		"turn": s.Turn, "wake": s.Wake, "turns": s.Turns, "closes": s.Closes, "req": s.Req,
		"call": s.Call, "parked": s.Parked, "waiting": s.Waiting, "row": s.Row, "shows": s.Shows,
		"steer": s.Steer, "pending": s.Pending, "pending_cancel": s.PendingCancel, "job": s.Job,
		"stray": s.Stray, "leak": s.Leak, "quiet": s.Quiet,
	}
}

const ecMaxTurns = 2

func (s *ecShadow) openUserTurn() {
	s.Turns++
	s.Turn, s.Wake, s.Closes = "open", false, 0
	if s.Steer == "note" || s.Steer == "parked" {
		s.Steer = "delivered"
	}
	s.Waiting = false
	if s.PendingCancel {
		s.PendingCancel, s.Quiet = false, true
		s.Turn, s.Closes, s.Req = "idle", 1, false
		s.cancels++
	} else {
		s.Parked, s.Quiet, s.Req = false, false, true
	}
}

func (s *ecShadow) closeCancel() {
	s.Turn = "idle"
	s.Closes++
	s.cancels++
	if s.Pending {
		s.Pending = false
		s.openUserTurn()
	}
}

func (s *ecShadow) asked() {
	if s.Steer == "parked" {
		s.Steer = "delivered"
	}
	s.Parked = false
	if s.Turn == "idle" {
		s.Turn, s.Wake, s.Closes = "open", true, 0
	}
	s.Req = true
}

func (s *ecShadow) sendWaiting() {
	if !s.Waiting {
		return
	}
	s.Waiting = false
	if s.Req {
		return
	}
	if s.Quiet && s.Turn != "open" {
		s.Leak = true
	}
	if s.Turn == "idle" && s.Row == "orphan" {
		s.Row = "wake"
	}
	s.asked()
}

func (s *ecShadow) reported(shows string) {
	switch {
	case s.Call == "cancelling":
		s.Row = "turn"
	case s.Turn == "idle" && (s.Parked || s.Req):
		s.Row = "orphan"
	case s.Turn == "idle":
		s.Row = "wake"
	default:
		s.Row = "next"
	}
	own := s.Call == "cancelling"
	s.Call, s.Shows = "none", shows
	switch {
	case s.Parked:
		s.Parked = false
		if own && s.Turn == "cancelling" && !s.Req {
			s.closeCancel()
		}
	case own && s.Turn == "cancelling" && !s.Req:
		opens := s.Pending
		s.closeCancel()
		if !opens {
			if s.Quiet {
				s.Leak = true
			}
			s.asked()
		}
	case s.Req:
		s.Waiting = true
	default:
		if s.Quiet && s.Turn != "open" {
			s.Leak = true
		}
		s.asked()
	}
}

// apply takes action name on s; false when its require does not hold.
func (s *ecShadow) apply(name string) bool {
	switch name {
	case "Prompt":
		if !(s.Turn == "idle" && s.Turns < ecMaxTurns) {
			return false
		}
		s.openUserTurn()
	case "StopBeforeSubmitCrosses":
		if !(s.Turn == "idle" && !s.PendingCancel && s.Turns < ecMaxTurns) {
			return false
		}
		s.PendingCancel = true
	case "PendingCancelLapses":
		if !s.PendingCancel {
			return false
		}
		s.PendingCancel = false
	case "Steer":
		if !(s.Turn == "open" && s.Steer == "none") {
			return false
		}
		s.Steer = "typed"
	case "SteerLands":
		if !(s.Turn == "open" && s.Steer == "typed") {
			return false
		}
		if s.Req {
			s.Steer = "queued"
		} else {
			s.Steer, s.Req = "delivered", true
		}
	case "CallStarts":
		if !(s.Turn == "open" && s.Req && s.Call == "none") {
			return false
		}
		s.Call, s.Req = "running", false
		if s.Steer == "queued" {
			s.Steer, s.Req = "delivered", true
		}
		s.sendWaiting()
	case "CallReturns":
		if !(s.Turn == "open" && s.Call == "running" && !s.Req) {
			return false
		}
		s.Call, s.Req = "none", true
	case "ModelAnswers":
		if !s.Req {
			return false
		}
		s.Req = false
		switch {
		case s.Turn == "idle":
			s.Stray = true
		case s.Turn == "cancelling":
			if s.Call != "cancelling" {
				s.closeCancel()
			}
		case s.Steer == "queued":
			s.Steer, s.Req = "delivered", true
		case s.Call != "running" && !s.Waiting:
			s.Turn = "idle"
			s.Closes++
		}
		s.sendWaiting()
	case "Stop":
		if !(s.Turn == "open" || s.Turn == "cancelling") {
			return false
		}
		if s.Turn != "open" {
			return true
		}
		switch s.Steer {
		case "typed":
			if s.Req {
				s.Steer = "note"
			} else {
				s.Steer = "parked"
			}
		case "queued":
			s.Steer = "note"
		}
		s.Quiet, s.Req, s.Waiting = true, false, false
		if s.Call == "running" {
			s.Parked, s.Call, s.Turn = true, "cancelling", "cancelling"
		} else {
			s.closeCancel()
		}
	case "PromptDuringCancel":
		if !(s.Turn == "cancelling" && !s.Pending && s.Turns < ecMaxTurns) {
			return false
		}
		s.Pending = true
	case "CallReportsCanceled", "CallIgnoresCancel":
		if !(s.Call == "cancelling" || s.Call == "late") {
			return false
		}
		if name == "CallReportsCanceled" {
			s.reported("cancelled")
		} else {
			s.reported("ok")
		}
	case "CancelWaitExpires":
		if s.Turn != "cancelling" {
			return false
		}
		if s.Call == "cancelling" {
			s.Call = "late"
		}
		s.closeCancel()
	case "JobFinishes":
		if !(s.Job == "running" && s.Turn == "cancelling") {
			return false
		}
		s.Job = "reported"
		s.asked()
	case "end":
	default:
		return false
	}
	return true
}

// ---- the adapter ----

// errInfeasible stops a walk the engine's own clock cannot follow: a
// step that has to wait (a cancel's second, the harness's grace) past a
// deadline the walk has not taken yet, such as an idle Esc's 2 s.
var errInfeasible = errors.New("the walk needs time the engine's clock does not give it")

type ecAdapter struct {
	t    *testing.T
	gate gate
	sh   ecShadow

	rt    *session.Runtime
	hist  *history.Store
	llm   *ecLLM
	tools *ecTools
	h     *ecHandler

	base      int      // history entries the setup wrote
	ahead     []string // the walk's actions after this one
	call      string   // the current call's id, "" before the first
	calls     int
	callTurn  map[string]int // call id → index of its turn's prompt
	callAt    time.Time      // the current call's start
	cancelled []string       // calls a Stop cancelled, in order
	stopping  bool           // a Stop on an open turn whose cancelled is not written yet
	stopSeen  int            // cancelled entries in history when it was pressed
	stopAt    time.Time
	escAt     time.Time // an idle Esc no submit has taken yet
	prompts   int
	pending   string // the prompt sent while cancelling
	steerText string
	steered   int
	resultsIn map[string]bool // cancelled calls whose result reached the model
	leakSeen  bool

	// wrong is TestEngineCancelMidCallCatchesWrongAdapter's bug:
	// CallReportsCanceled lets the call finish instead of cancelling it.
	wrong bool
}

// Init opens a session and runs the setup turn the spec's Init starts
// after: a hold call the turn settle adopted as a job.
func (a *ecAdapter) Init() error {
	dir := a.t.TempDir()
	h, err := history.Open(filepath.Join(dir, "history", "s1.jsonl"))
	if err != nil {
		return err
	}
	a.hist, a.llm = h, &ecLLM{}
	a.tools = &ecTools{running: map[string]chan struct{}{}}
	reg := agenttools.NewRegistry()
	if _, err := reg.Register(a.tools.tool()); err != nil {
		return err
	}
	llmSrc := a.llm
	d := session.Deps{
		SessionID: "s1",
		Store:     filepath.Join(dir, "engine"),
		Scratch:   func() string { return filepath.Join(dir, "scratch") },
		Cwd:       dir,
		// An hour: a walk's clock moves a few seconds at most, so no call
		// of a walk is ever adopted; the setup sleeps it out.
		Config:  session.Config{TurnSettle: time.Hour},
		LLM:     func() (agentllm.Source, string, error) { return llmSrc, "fake", nil },
		Tools:   reg,
		History: h,
	}
	d.Registry = func(string) (tool.Registry, []operation.RemoteJobHandler) {
		a.h = &ecHandler{
			Handler: boughcall.New(boughcall.Options{Tools: reg.Lookup, Session: "s1",
				SpillDir: func() string { return filepath.Join(dir, "calls") }}),
			ops: map[string]operation.ID{}, asked: map[string]string{}, canceled: map[string]bool{},
		}
		return toolreg.New(toolreg.Config{Tools: reg.Tools()}), []operation.RemoteJobHandler{a.h}
	}
	rt, err := session.Open(context.Background(), d)
	if err != nil {
		return err
	}
	a.rt = rt
	a.sh = newEcShadow()
	a.call, a.calls, a.cancelled, a.callTurn = "", 0, nil, map[string]int{}
	a.stopping, a.stopSeen = false, 0
	a.prompts, a.pending, a.steerText, a.steered = 0, "", "", 0
	a.resultsIn, a.leakSeen = map[string]bool{}, false
	a.gate.reset()

	a.rt.Submit("setup: start the job")
	a.settle()
	if err := a.llm.answer(fake.Text("calling job"), fake.Call("job", "hold", `{"text":"job"}`)); err != nil {
		return fmt.Errorf("setup: %w\n%s", err, dumpEntries(a.hist.Entries()))
	}
	a.settle()
	time.Sleep(time.Hour)
	a.settle()
	es := a.hist.Entries()
	if n := count(es, "done"); n != 1 || !a.tools.isRunning("job") || a.llm.held() != 0 {
		return fmt.Errorf("setup: the job was not adopted (done %d, job running %v)\n%s", n, a.tools.isRunning("job"), dumpEntries(es))
	}
	a.base = len(es)
	a.stopAt, a.escAt = time.Time{}, time.Time{}
	return nil
}

// Cleanup closes the session: every goroutine of the bubble must end.
func (a *ecAdapter) Cleanup() error {
	if a.rt == nil {
		return nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	err := a.rt.Close(ctx)
	a.rt = nil
	return err
}

// ---- time ----

// ecStep is how far the clock moves after every step: the harness
// slurps its inbox and op updates until they have been idle for a
// millisecond. It keeps a call's start, its Stop and their timers from
// ever sharing an instant.
const ecStep = 5 * time.Millisecond

// graceWait is how long the harness holds an input back after the model
// asked for calls (coordinator toolCallRunGracePeriod): the input waits
// for those calls to finish, or for the grace to pass. It starts a few
// slurp milliseconds after the answer, hence the margin.
const graceWait = time.Second + 10*time.Millisecond

// The engine timers a walk takes on purpose: the open cancel's
// cancelWait and an idle Esc's pendingCancelFor.
const (
	cancelWait       = time.Second
	pendingCancelFor = 2 * time.Second
)

// deadlines are the engine timers running that the walk has not taken:
// the open cancel's (stopping) and the idle Esc's (escAt), zero when
// none runs.
func (a *ecAdapter) deadlines() (cancel, esc time.Time) {
	if a.stopping {
		cancel = a.stopAt.Add(cancelWait)
	}
	if !a.escAt.IsZero() {
		esc = a.escAt.Add(pendingCancelFor)
	}
	return cancel, esc
}

// sleepTo moves the clock to t, or fails the walk as infeasible when
// that would run out a timer the walk has not taken; skip is the one
// the step itself runs out ("cancel" or "esc").
func (a *ecAdapter) sleepTo(t time.Time, skip string) error {
	cancel, esc := a.deadlines()
	if skip != "cancel" && !cancel.IsZero() && !t.Before(cancel) ||
		skip != "esc" && !esc.IsZero() && !t.Before(esc) {
		return errInfeasible
	}
	time.Sleep(time.Until(t))
	synctest.Wait()
	return nil
}

// settle lets everything the action woke run to a stop, and notes the
// timers it ended.
func (a *ecAdapter) settle() {
	synctest.Wait()
	a.notice()
	d := ecStep
	for _, dl := range []time.Time{a.stopAt.Add(cancelWait), a.escAt.Add(pendingCancelFor)} {
		if cancel, esc := a.deadlines(); dl.Equal(cancel) && !cancel.IsZero() || dl.Equal(esc) && !esc.IsZero() {
			d = min(d, time.Until(dl)-time.Millisecond)
		}
	}
	if d > 0 {
		time.Sleep(d)
	}
	synctest.Wait()
	a.notice()
}

// notice clears the timers the engine is done with: a cancel whose
// cancelled is written, an Esc a submit took (the prompt sent while
// cancelling is submitted when the cancel closes).
func (a *ecAdapter) notice() {
	if a.stopping && a.view().cancels > a.stopSeen {
		a.stopping = false
	}
	if a.pending != "" && !a.pendingPrompt() {
		a.pending, a.escAt = "", time.Time{}
	}
}

// passGrace lets the harness's grace for the running call pass, so an
// input or result that waits on it goes to the model at once.
func (a *ecAdapter) passGrace() error {
	if a.call == "" || !a.tools.isRunning(a.call) {
		return nil
	}
	if t := a.callAt.Add(graceWait); time.Now().Before(t) {
		return a.sleepTo(t, "")
	}
	return nil
}

// ---- reading the engine ----

// ecView is the turn structure history shows.
type ecView struct {
	open    bool // an opening input with no done after it
	wake    bool // the last opening input was a wake
	turns   int
	closes  int
	stray   bool
	cancels int
	lastIn  int // index of the last opening input, -1 when none
}

func opening(e history.Entry) bool { return e.Kind == "input" && e.Data["steer"] != true }

func (a *ecAdapter) entries() []history.Entry { return a.hist.Entries()[a.base:] }

func (a *ecAdapter) view() ecView { return viewOf(a.entries()) }

func viewOf(es []history.Entry) ecView {
	v := ecView{lastIn: -1}
	for i, e := range es {
		switch e.Kind {
		case "input":
			if !opening(e) {
				continue
			}
			v.open, v.lastIn, v.closes = true, i, 0
			v.wake = e.Data["wake"] == true
			if !v.wake {
				v.turns++
			}
		case "done":
			v.open = false
			v.closes++
		case "cancelled":
			v.cancels++
		case "assistant":
			if !v.open {
				v.stray = true
			}
		}
	}
	return v
}

func (a *ecAdapter) turn(v ecView) string {
	switch {
	case a.stopping && v.cancels == a.stopSeen:
		return "cancelling"
	case v.open:
		return "open"
	}
	return "idle"
}

// turnsOf is render.tsx groupTurns reduced to what places a row: the
// turn each entry lands in, and each turn's prompt (-1 for none).
func turnsOf(es []history.Entry) (turnOf []int, prompt []int) {
	turnOf = make([]int, len(es))
	cur, curDone := -1, false
	for i, e := range es {
		turnOf[i] = -1
		switch {
		case e.Kind == "engine" || e.Kind == "meta":
			continue
		case e.Kind == "input" && e.Data["steer"] == true && cur >= 0 && prompt[cur] >= 0 && !curDone:
		case e.Kind == "input" && e.Data["wake"] == true && cur >= 0 && prompt[cur] < 0 && !curDone:
			prompt[cur] = i
		case e.Kind == "input":
			prompt = append(prompt, i)
			cur, curDone = len(prompt)-1, false
		case (e.Kind == "done" || e.Kind == "cancelled") && (cur < 0 || curDone):
			// the done after a cancelled: the turn already closed
			continue
		default:
			if cur < 0 || curDone {
				prompt = append(prompt, -1)
				cur, curDone = len(prompt)-1, false
			}
			if e.Kind == "done" || e.Kind == "cancelled" {
				curDone = true
			}
		}
		turnOf[i] = cur
	}
	return turnOf, prompt
}

// row places the last cancelled call that reported, as groupTurns does:
// in its own turn, in the body of a later turn, in the wake turn it
// opened, or in a turn of its own with no prompt.
func (a *ecAdapter) row() (row, shows string) {
	es := a.entries()
	turnOf, prompt := turnsOf(es)
	for j := len(a.cancelled) - 1; j >= 0; j-- {
		id := a.cancelled[j]
		at := slices.IndexFunc(es, func(e history.Entry) bool { return e.Kind == "call" && e.Data["id"] == id })
		if at < 0 {
			continue
		}
		e := es[at]
		switch {
		case e.Data["canceled"] == true:
			shows = "cancelled"
		case e.Data["error"] != nil && e.Data["error"] != "":
			shows = "failed"
		default:
			shows = "ok"
		}
		p := prompt[turnOf[at]]
		switch {
		case p < 0:
			row = "orphan"
		case p == a.callTurn[id]:
			row = "turn"
		case p > at:
			row = "wake"
		default:
			row = "next"
		}
		return row, shows
	}
	return "", ""
}

func (a *ecAdapter) callRunning() bool {
	if a.call == "" || !a.tools.isRunning(a.call) {
		return false
	}
	return !slices.ContainsFunc(a.entries(), func(e history.Entry) bool { return e.Kind == "call" && e.Data["id"] == a.call })
}

func (a *ecAdapter) job() string {
	switch {
	case a.h.wasCanceled("job"):
		return "cancelled"
	case a.tools.isRunning("job"):
		return "running"
	}
	return "reported"
}

func (a *ecAdapter) pendingPrompt() bool {
	return a.pending != "" && !slices.ContainsFunc(a.entries(), func(e history.Entry) bool {
		return opening(e) && e.Data["text"] == a.pending
	})
}

// GetState is the role's state: the spec's hidden fields (parked,
// waiting, quiet, pending_cancel, the steer's stage, whether a running
// call is its cancel's or outlived it) from the shadow, the rest read
// off history, the model, the tools and the handler.
func (a *ecAdapter) GetState() map[string]any {
	st := a.sh.state()
	v := a.view()
	st["turn"], st["wake"], st["turns"], st["closes"] = a.turn(v), v.wake, v.turns, v.closes
	st["req"] = a.llm.held() > 0
	switch {
	case !a.callRunning():
		st["call"] = "none"
	case a.sh.Call == "none":
		st["call"] = "running"
	}
	st["row"], st["shows"] = a.row()
	st["stray"], st["pending"], st["job"], st["leak"] = v.stray, a.pendingPrompt(), a.job(), a.leakSeen
	return st
}

// newResult reports whether a request that reached the model since
// request from carried a cancelled call's result for the first time.
// The harness answers every call at once with a "still running"
// placeholder; the call's own result is the second tool result with
// its id.
func (a *ecAdapter) newResult(from int) bool {
	hit := false
	for _, r := range a.llm.requests()[from:] {
		seen := map[string]int{}
		for _, it := range r.Input {
			tr, ok := it.Data.(ullm.ToolResult)
			if !ok || !slices.Contains(a.cancelled, tr.CallID) {
				continue
			}
			if seen[tr.CallID]++; seen[tr.CallID] == 2 && !a.resultsIn[tr.CallID] {
				a.resultsIn[tr.CallID] = true
				hit = true
			}
		}
	}
	return hit
}

// ---- actions ----

// step takes action name: the shadow first (its require is the gate),
// then the engine the way the TUI would.
func (a *ecAdapter) step(name string) error {
	before := a.sh
	if !a.gate.pass(a.sh.apply(name)) {
		a.sh = before
		return nil
	}
	from := len(a.llm.requests())
	if err := a.drive(name, before); err != nil {
		return err
	}
	a.settle()
	// leak is the engine's: a request that answers a cancelled call
	// really reached the model when the spec says it did.
	if a.newResult(from) && a.sh.Leak && !before.Leak {
		a.leakSeen = true
	}
	return nil
}

func (a *ecAdapter) drive(name string, before ecShadow) error {
	switch name {
	case "Prompt":
		a.prompts++
		a.rt.Submit(fmt.Sprintf("prompt %d", a.prompts))
		a.escAt = time.Time{}
	case "StopBeforeSubmitCrosses":
		// Esc while idle: the cancel waits pendingCancelFor for the
		// submit it crossed.
		a.rt.Cancel()
		a.escAt = time.Now()
	case "PendingCancelLapses":
		if err := a.sleepTo(a.escAt.Add(pendingCancelFor), "esc"); err != nil {
			return err
		}
		a.escAt = time.Time{}
	case "Steer":
		// typed in the composer: nothing reaches the engine yet
		a.steered++
		a.steerText = fmt.Sprintf("steer %d", a.steered)
	case "SteerLands":
		if !a.rt.Steer(a.steerText) {
			return fmt.Errorf("the open turn refused the steer")
		}
		if !before.Req {
			a.settle()
			if err := a.passGrace(); err != nil {
				return err
			}
		}
		a.settle()
		return a.steerRecorded()
	case "CallStarts":
		a.calls++
		a.call = fmt.Sprintf("c%d", a.calls)
		a.callTurn[a.call] = a.view().lastIn
		a.callAt = time.Now()
		if err := a.llm.answer(fake.Text("calling "+a.call), fake.Call(a.call, "hold", `{"text":"`+a.call+`"}`)); err != nil {
			return err
		}
		a.settle()
		if !a.tools.isRunning(a.call) {
			return fmt.Errorf("call %s did not start", a.call)
		}
		if a.sh.Req {
			// a steer or a result waits on the new call's grace
			return a.passGrace()
		}
	case "CallReturns":
		return a.tools.finish(a.call)
	case "ModelAnswers":
		return a.llm.answer(fake.Text("an answer"))
	case "Stop":
		if before.Turn != "open" {
			// a second Esc while cancelling: cancelTurn returns at once
			a.rt.Cancel()
			return nil
		}
		if before.Call == "running" && a.inThisCancel("JobFinishes") {
			if err := a.passGrace(); err != nil {
				return err
			}
		}
		if !a.escAt.IsZero() && a.inThisCancel("PendingCancelLapses") {
			// The idle Esc lapses while this cancel waits: the Stop
			// comes late enough that its second outlasts the Esc's two.
			if t := a.escAt.Add(pendingCancelFor - cancelWait + 10*time.Millisecond); time.Now().Before(t) {
				if err := a.sleepTo(t, ""); err != nil {
					return err
				}
			}
		}
		if before.Steer == "typed" {
			// the steer and Esc pressed together
			if !a.rt.Steer(a.steerText) {
				return fmt.Errorf("the open turn refused the steer")
			}
		}
		if before.Call == "running" {
			a.cancelled = append(a.cancelled, a.call)
		}
		a.stopping, a.stopSeen, a.stopAt = true, a.view().cancels, time.Now()
		a.rt.Cancel()
		if before.Steer == "typed" || before.Steer == "queued" {
			a.settle()
			return a.steerRecorded()
		}
	case "PromptDuringCancel":
		a.prompts++
		a.pending = fmt.Sprintf("prompt %d", a.prompts)
		a.rt.Submit(a.pending)
	case "CallReportsCanceled":
		if a.wrong {
			return a.tools.finish(a.call)
		}
		return a.h.release(a.call)
	case "CallIgnoresCancel":
		return a.tools.finish(a.call)
	case "CancelWaitExpires":
		return a.sleepTo(a.stopAt.Add(cancelWait), "cancel")
	case "JobFinishes":
		if err := a.tools.finish("job"); err != nil {
			return err
		}
		a.settle()
		return a.passGrace()
	case "end":
	default:
		return fmt.Errorf("no action %q", name)
	}
	return nil
}

// inThisCancel: the walk takes action before the cancel a Stop is about
// to start can close. A Stop then picks its moment, as a person's Esc
// does: after the call's grace when the job finishes during the cancel
// (its result would wait on that grace, which a person pressing Esc
// within a second of the call starting sees), after the idle Esc's
// first second when that Esc lapses during the cancel.
func (a *ecAdapter) inThisCancel(action string) bool {
	for _, n := range a.ahead {
		switch n {
		case action:
			return true
		case "CancelWaitExpires", "CallReportsCanceled", "CallIgnoresCancel", "ModelAnswers", "Prompt":
			return false
		}
	}
	return false
}

func (a *ecAdapter) steerRecorded() error {
	if slices.ContainsFunc(a.entries(), func(e history.Entry) bool {
		return e.Kind == "input" && e.Data["steer"] == true && e.Data["text"] == a.steerText
	}) {
		return nil
	}
	return fmt.Errorf("steer %q was not recorded", a.steerText)
}

// ---- walks ----

// walkEngineCancel runs one generated path in a bubble of its own. It
// returns the first step whose action fails, is refused by the adapter's
// own require, or leaves a state other than the path's, then whether
// the walk's history is a path in the graph. A walk the engine's clock
// cannot follow returns errInfeasible.
func walkEngineCancel(t *testing.T, g *tracecheck.Graph, n int, p genPath, wrong bool) (err error) {
	t.Helper()
	synctest.Test(t, func(bt *testing.T) {
		a := &ecAdapter{t: bt, wrong: wrong}
		defer func() {
			if cerr := a.Cleanup(); cerr != nil && err == nil {
				err = fmt.Errorf("path %d: cleanup: %w", n, cerr)
			}
		}()
		if ierr := a.Init(); ierr != nil {
			err = fmt.Errorf("path %d: Init: %w", n, ierr)
			return
		}
		var names []string
		for i, st := range p.Trace {
			want := ecRole(st.State)
			if i > 0 {
				name := strings.TrimPrefix(st.Action, "Engine#0.")
				names = append(names, name)
				a.ahead = a.ahead[:0]
				for _, s := range p.Trace[i+1:] {
					a.ahead = append(a.ahead, strings.TrimPrefix(s.Action, "Engine#0."))
				}
				if serr := a.step(name); serr != nil {
					if errors.Is(serr, errInfeasible) {
						err = fmt.Errorf("path %d %v: %w", n, names, serr)
						return
					}
					err = fmt.Errorf("path %d %v: %w\n%s", n, names, serr, dumpEntries(a.hist.Entries()))
					return
				}
				if a.gate.off {
					err = fmt.Errorf("path %d %v: the shadow refused a step the graph enables", n, names)
					return
				}
			}
			if sh := a.sh.state(); !reflect.DeepEqual(jsonRound(sh), jsonRound(want)) {
				err = fmt.Errorf("path %d %v: the Go port of the spec drifted\n got %v\nwant %v", n, names, jsonRound(sh), jsonRound(want))
				return
			}
			if got := a.GetState(); !reflect.DeepEqual(jsonRound(got), jsonRound(want)) {
				err = fmt.Errorf("path %d %v: state\n got %v\nwant %v\n%s", n, names, jsonRound(got), jsonRound(want), dumpEntries(a.hist.Entries()))
				return
			}
		}
		steps := engineCancelMidCallHistory(a.hist.Entries())
		if v := g.Check(steps); v != nil {
			b, _ := json.Marshal(steps)
			err = fmt.Errorf("path %d %v: history trace is not a path in the model: %v\ntrace: %s\n%s", n, names, v, b, dumpEntries(a.hist.Entries()))
		}
	})
	return err
}

func ecGraph(t *testing.T) *tracecheck.Graph {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath(ecSpec)), "..", "testdata", ecSpec))
	if err != nil {
		t.Fatal(err)
	}
	return g
}

// ecRole is a path state's Engine#0 fields by bare name.
func ecRole(s map[string]any) map[string]any {
	out := map[string]any{}
	for k, v := range s {
		if f, ok := strings.CutPrefix(k, "Engine#0."); ok {
			out[f] = v
		}
	}
	return out
}

// Every path over the checked-in graph (every settled state; every
// transition under MODEL_COVER=transitions), walked against the engine
// step by step, with the whole role state compared after each step and
// each walk's history replayed on the graph. Walks the engine's clock
// cannot follow are counted, and must stay few.
func TestEngineCancelMidCallPaths(t *testing.T) {
	t.Parallel()
	paths := loadPaths(t, ecSpec)
	g := ecGraph(t)
	const shards = 4
	var mu sync.Mutex
	infeasible := 0
	t.Run("walks", func(t *testing.T) {
		for sh := range shards {
			t.Run(fmt.Sprintf("shard%d", sh), func(t *testing.T) {
				t.Parallel()
				for i := sh; i < len(paths); i += shards {
					err := walkEngineCancel(t, g, i, paths[i], false)
					switch {
					case errors.Is(err, errInfeasible):
						t.Log(err)
						mu.Lock()
						infeasible++
						mu.Unlock()
					case err != nil:
						t.Error(err)
					}
				}
			})
		}
	})
	t.Logf("%d walks, %d the engine's clock cannot follow", len(paths), infeasible)
	if infeasible*10 > len(paths) {
		t.Errorf("%d of %d walks were infeasible; the adapter's timing is off", infeasible, len(paths))
	}
}

// A CallReportsCanceled that lets the call finish instead of cancelling
// it shows ok where the spec says cancelled: a walk through that
// transition must fail.
func TestEngineCancelMidCallCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	b, err := pathsJSONCover(ecSpec, tracecheck.CoverTransitions)
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Paths []genPath `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		t.Fatal(err)
	}
	g := ecGraph(t)
	for i, p := range f.Paths {
		if !slices.ContainsFunc(p.Trace, func(s tracecheck.Step) bool { return s.Action == "Engine#0.CallReportsCanceled" }) {
			continue
		}
		if err := walkEngineCancel(t, g, i, p, true); err != nil && !errors.Is(err, errInfeasible) {
			t.Logf("caught: %.400s", err)
			return
		}
	}
	t.Fatal("walks whose CallReportsCanceled finishes the call passed; the walks are not checking state")
}

// ---- history ----

// engineCancelMidCallHistory reads the abstract trace off a transcript
// (the setup turn that adopted the job is skipped). History records a
// prompt, a steer, a reply ("calling <id>" is the model starting call
// <id>, any other text is ModelAnswers), a call's end row, the job's end
// row, and a cancel's cancelled + done. The steps that leave nothing
// behind are placed with the spec's own rules, run alongside:
//   - Stop goes where the cancel first shows: before the job's row or a
//     canceled row while the turn is open, or at the cancelled.
//   - A cancelled while the spec still waits on the call is
//     CancelWaitExpires.
//   - A user prompt right after a cancel's done is one sent while
//     cancelling (a prompt after the close would follow the wake the
//     close opens, when it opens one).
//   - A steer is Steer then SteerLands; an idle Esc is not told apart
//     from a Stop of the turn it cancelled (the same entries).
//   - A prompt cancelled as soon as a cancel's close records it is the
//     prompt sent while cancelling, cut by a second Esc, or a new prompt
//     an idle Esc cancelled: the reading whose steps the spec agrees
//     with, entry for entry, is the one taken.
//
// Each step carries what history shows once the entries of that step
// are all written: closes, turns, wake and stray.
func engineCancelMidCallHistory(entries []history.Entry) []tracecheck.Step {
	steps := projectEngineCancel(entries, false)
	if !ecAgrees(steps) {
		if alt := projectEngineCancel(entries, true); ecAgrees(alt) {
			return alt
		}
	}
	return steps
}

// ecAgrees replays steps on the Go port of the spec and compares what
// each step carries.
func ecAgrees(steps []tracecheck.Step) bool {
	sh := newEcShadow()
	for i, s := range steps {
		if i > 0 && !sh.apply(strings.TrimPrefix(s.Action, "Engine#0.")) {
			return false
		}
		st := sh.state()
		for k, v := range s.State {
			if f, _ := strings.CutPrefix(k, "Engine#0."); st[f] != v {
				return false
			}
		}
	}
	return true
}

func projectEngineCancel(entries []history.Entry, cutPending bool) []tracecheck.Step {
	start := slices.IndexFunc(entries, func(e history.Entry) bool { return e.Kind == "done" })
	if start < 0 {
		return nil
	}
	es := entries[start+1:]
	obs := func(upto int) map[string]any {
		v := viewOf(es[:upto])
		return map[string]any{"Engine#0.closes": v.closes, "Engine#0.turns": v.turns,
			"Engine#0.wake": v.wake, "Engine#0.stray": v.stray}
	}
	sh := newEcShadow()
	steps := []tracecheck.Step{{Action: "Init", State: obs(0)}}
	take := func(name string) {
		sh.apply(name) // a step the spec refuses shows in the check
		steps = append(steps, tracecheck.Step{Action: "Engine#0." + name})
	}
	// promptAfter: the cancel starting at i closes with a user prompt
	// right after its done.
	// next is the index of the first entry after i that is not
	// bookkeeping, len(es) when none.
	next := func(i int, skipDone bool) int {
		for j := i + 1; j < len(es); j++ {
			switch k := es[j].Kind; {
			case k == "engine" || k == "meta" || skipDone && k == "done":
				continue
			}
			return j
		}
		return len(es)
	}
	// cutAt: the prompt at i is cancelled the moment it is recorded.
	cutAt := func(i int) bool { j := next(i, false); return j < len(es) && es[j].Kind == "cancelled" }
	promptAfter := func(i int) bool {
		c := slices.IndexFunc(es[i:], func(e history.Entry) bool { return e.Kind == "cancelled" })
		if c < 0 {
			return false
		}
		j := next(i+c, true)
		// A prompt cancelled as it is recorded is an idle Esc's (below).
		return j < len(es) && opening(es[j]) && es[j].Data["wake"] != true && (cutPending || !cutAt(j))
	}
	// wakeFor: call's result later opens a wake turn (it was not parked).
	wakeFor := func(i int, call string) bool {
		return slices.ContainsFunc(es[i:], func(e history.Entry) bool {
			// []string as the engine appends it, []any read back from disk
			names, _ := e.Data["calls"].([]string)
			if calls, ok := e.Data["calls"].([]any); ok {
				for _, c := range calls {
					s, _ := c.(string)
					names = append(names, s)
				}
			}
			return e.Kind == "input" && e.Data["wake"] == true && slices.Contains(names, call)
		})
	}
	lastCall := ""
	stop := func(i int) {
		take("Stop")
		if sh.Turn == "cancelling" && promptAfter(i) {
			take("PromptDuringCancel")
		}
	}
	// What the steps taken so far wrote: user turns and cancels. An entry
	// they account for is theirs, not a new step's (the prompt a cancel's
	// close opens, the cancelled a report's close writes).
	userIns, cancels := 0, 0
	for i, e := range es {
		owed := e.Kind == "input" && e.Data["wake"] == true ||
			opening(e) && e.Data["wake"] != true && userIns < sh.Turns
		if (e.Kind == "input" || e.Kind == "assistant" || e.Kind == "call") && !owed {
			// a new step starts: the last one's entries are all written
			steps[len(steps)-1].State = obs(i)
		}
		switch e.Kind {
		case "input":
			switch {
			case e.Data["steer"] == true:
				take("Steer")
				take("SteerLands")
			case e.Data["wake"] == true:
				// opened by the step before
			case owed:
				userIns++
			default:
				// A prompt cancelled as it is recorded is an idle Esc's
				// (the parked call stays parked), or a Stop right after
				// it (the prompt unparked it: its result later wakes).
				if cutAt(i) && !(sh.Parked && wakeFor(i, lastCall)) &&
					sh.Turn == "idle" && !sh.PendingCancel && sh.Turns < ecMaxTurns {
					take("StopBeforeSubmitCrosses")
				}
				take("Prompt")
				userIns++
			}
		case "assistant":
			if text, _ := e.Data["text"].(string); strings.HasPrefix(text, "calling ") {
				lastCall = strings.TrimPrefix(text, "calling ")
				take("CallStarts")
			} else {
				take("ModelAnswers")
			}
		case "call":
			canceled := e.Data["canceled"] == true
			report := "CallIgnoresCancel"
			if canceled {
				report = "CallReportsCanceled"
			}
			switch {
			case e.Data["id"] == "job":
				if sh.Turn == "open" {
					stop(i)
				}
				take("JobFinishes")
			case sh.Call == "running" && (canceled || sh.Turn == "cancelling" || sh.Req):
				// An ok row while a request is in flight cannot be the call
				// returning (the turn would be waiting on it): it is the
				// call finishing as Esc landed.
				if sh.Turn == "open" {
					stop(i)
				}
				take(report)
			case sh.Call == "running":
				take("CallReturns")
			default:
				take(report)
			}
		case "cancelled":
			if cancels == sh.cancels {
				if sh.Turn == "open" {
					stop(i)
				}
				if sh.Turn == "cancelling" {
					take("CancelWaitExpires")
				}
			}
			cancels++
		}
	}
	steps[len(steps)-1].State = obs(len(es))
	return steps
}

func init() { historyProjections[ecSpec] = engineCancelMidCallHistory }

// ---- helpers ----

func count(es []history.Entry, kind string) int {
	n := 0
	for _, e := range es {
		if e.Kind == kind {
			n++
		}
	}
	return n
}

func dumpEntries(es []history.Entry) string {
	var b strings.Builder
	for _, e := range es {
		switch e.Kind {
		case "engine", "meta":
			continue
		}
		d := map[string]any{}
		for k, v := range e.Data {
			switch k {
			case "text", "id", "canceled", "wake", "steer", "calls", "running", "late", "adopted", "error":
				d[k] = v
			}
		}
		j, _ := json.Marshal(d)
		fmt.Fprintf(&b, "%3d %-10s %s\n", e.Seq, e.Kind, j)
	}
	return b.String()
}
