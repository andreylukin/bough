//go:build !windows

package mbt

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
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

// specs/provider_failure_retry.fizz against a real serve: one web
// session on the engine whose model requests go through the real
// Messages API adapter (llm-control's "api" turns), so a 529 is retried
// by the adapter's own loop, a context overflow is a real 400 the Gate
// makes sticky, a refusal and a max_tokens stop are the provider's stop
// reasons.
//
// Every model request is one api turn held by llm-control, one HTTP
// attempt at a time: the adapter answers the held attempt (AnswerWith)
// to end it, streams into it (Stream), and lets the retry's wait go
// (Retry). The spec's req, attempt and stream are the adapter's record
// of that, checked against what the server does at every step: the
// attempt the row reports reaching it, the delta and delta-reset events
// on the page's stream. call is the adapter's record of its one bash
// call (running while its gate file is missing), checked against the
// call's running row and its recorded end. overflow and paid are the adapter's too; paid is
// how many context-overflow 400s llm-control actually served: while the
// model has overflowed, every request the adapter expects the Gate to
// answer is queued anyway as a turn that would answer 400 again, so a
// Gate that forgot the overflow is caught paying for it.
//
// status and live are the row's; open, erred, closed, dones, stray, turn and cut
// are read off the transcript (history is what the page renders and
// StatusOf reads) at the end of each step. One exception: a Prompt on a
// model whose overflow is sticky is answered by the Gate at once, so the
// spec's step between the input and the Gate's answer cannot be held;
// Prompt checks the input is recorded and reports the state as of that
// input, and StickyOverflow checks what the Gate recorded after it.
type pfrAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	id     string
	ids    []string
	events chan serve.Event
	stream *servetest.Stream

	n      int    // names are unique across walks: one queue per serve
	held   string // the api turn of the request in flight, "" when none
	gateF  string // the bash call's gate file, "" when none
	callID string
	sentry string // a turn queued for a request the Gate should answer
	model  int    // the session's model is control-<model>, 0: "control"

	snap   []history.Entry // the transcript as of the last step
	status string

	// The adapter's record of the spec's fields it drives.
	req, call     string
	attempt, strm int
	overflow      bool
	paid          int

	ran map[string]int

	// transientAsFatal is TestProviderFailureRetryCatchesWrongAdapter's
	// bug: a transient failure is answered as a fatal one.
	transientAsFatal bool
}

func newPFRAdapter(t *testing.T) *pfrAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &pfrAdapter{t: t, s: s, dir: control.Dir(s.Home)}
	t.Cleanup(func() {
		if a.gateF != "" {
			os.WriteFile(a.gateF, nil, 0o644)
		}
		if a.stream != nil {
			a.stream.Close()
		}
	})
	return a
}

func (a *pfrAdapter) on(action string, enabled bool) bool {
	if !a.gate.pass(enabled) {
		return false
	}
	if a.ran == nil {
		a.ran = map[string]int{}
	}
	a.ran[action]++
	return true
}

func (a *pfrAdapter) name(prefix string) string {
	a.n++
	return fmt.Sprintf("%s%05d", prefix, a.n)
}

func (a *pfrAdapter) Init() error {
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
	ch := make(chan serve.Event, 1024)
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
	a.held, a.gateF, a.callID, a.sentry, a.model = "", "", "", "", 0
	a.snap, a.status = nil, string(row.Status)
	a.req, a.call, a.attempt, a.strm, a.overflow, a.paid = "none", "none", 1, 0, false, 0
	a.gate.reset()
	return nil
}

// Cleanup ends what the walk left running by killing the session's
// child (nobody reads this session again), so its transcript stops on a
// path of the model instead of on whatever a released call would start,
// then lets an orphaned call's loop exit and empties the queue.
func (a *pfrAdapter) Cleanup() error {
	var err error
	if a.held != "" || a.gateF != "" || a.sentry != "" {
		err = a.kill()
	}
	if a.gateF != "" {
		os.WriteFile(a.gateF, nil, 0o644)
	}
	left, _ := filepath.Glob(filepath.Join(a.dir, "*.json"))
	for _, f := range left {
		os.Remove(f)
	}
	a.held, a.gateF, a.sentry = "", "", ""
	return err
}

// kill is the archive kill, undone: the child exits, the session stays.
func (a *pfrAdapter) kill() error {
	a.drain()
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
		if err := a.await("the child's exit", func(ev serve.Event) bool { return ev.Kind == "exit" }); err != nil {
			return err
		}
	}
	_, err = a.s.Unarchive(ctx, a.id)
	return err
}

func (a *pfrAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

// GetState reads live off the row now; the rest is the step's.
func (a *pfrAdapter) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return nil, err
	}
	h := pfrHistoryState(a.snap)
	return map[string]any{
		"status": a.status, "open": h.open, "erred": h.erred, "closed": h.closed, "dones": h.dones,
		"stray": h.stray, "turn": h.turn, "req": a.req, "attempt": a.attempt, "stream": a.strm,
		"call": a.call, "overflow": a.overflow, "paid": a.paid, "cut": h.cut, "live": row.Live,
	}, nil
}

// pfrHist is what the transcript says, as the spec names it.
type pfrHist struct {
	open, erred, stray bool
	closed, turn, cut  string
	dones              int
}

// pfrCutNote is the start of the system entry a max_tokens stop records.
const pfrCutNote = "reply cut off at max_tokens"

// pfrHistoryState reads the spec's transcript fields the way StatusOf
// does: an input opens a turn and clears errInTurn, a done or cancelled
// closes it, and the done the engine writes after its cancelled is the
// same close.
func pfrHistoryState(es []history.Entry) pfrHist {
	var h pfrHist
	for _, e := range es {
		switch e.Kind {
		case "input":
			h.open, h.erred, h.closed, h.dones, h.cut = true, false, "", 0, ""
			h.turn = "user"
			if e.Data["wake"] == true {
				h.turn = "wake"
			}
		case "error":
			h.erred = true
			if !h.open {
				h.stray = true
			}
		case "system":
			if s, _ := e.Data["text"].(string); strings.HasPrefix(s, pfrCutNote) && h.open {
				h.cut = "cut"
			}
		case "done", "cancelled":
			if e.Kind == "done" && !h.open && h.closed == "cancelled" {
				continue
			}
			h.open = false
			h.closed = e.Kind
			h.dones++
		}
	}
	return h
}

func (a *pfrAdapter) entries() ([]history.Entry, error) {
	return history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
}

// waitHist polls the transcript until ok holds, and keeps it as the
// step's snapshot.
func (a *pfrAdapter) waitHist(what string, ok func([]history.Entry) bool) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		es, err := a.entries()
		if err == nil && ok(es) {
			a.snap = es
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for %s: not in the transcript after %s:\n%s", what, actionTimeout, pfrDump(es))
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// pfrDump is a transcript one entry a line, for a failure message.
func pfrDump(es []history.Entry) string {
	var b strings.Builder
	for _, e := range es {
		if e.Kind == "engine" || e.Kind == "meta" {
			continue
		}
		d := map[string]any{}
		for k, v := range e.Data {
			if k != "checkpoint" && k != "engine_turn" && k != "input_id" {
				d[k] = v
			}
		}
		j, _ := json.Marshal(d)
		if len(j) > 300 {
			j = append(j[:300], "…"...)
		}
		fmt.Fprintf(&b, "  %d %s %s\n", e.Seq, e.Kind, j)
	}
	return b.String()
}

// await applies events until one satisfies ok.
func (a *pfrAdapter) await(what string, ok func(serve.Event) bool) error {
	timer := time.NewTimer(actionTimeout)
	defer timer.Stop()
	var seen []string
	for {
		select {
		case ev, open := <-a.events:
			if !open {
				return fmt.Errorf("waiting for %s: the event stream ended", what)
			}
			if ok(ev) {
				return nil
			}
			seen = append(seen, ev.Kind+" "+ev.Text)
		case <-timer.C:
			return fmt.Errorf("waiting for %s: no such event after %s; saw %q", what, actionTimeout, seen)
		}
	}
}

// drain drops the events already in, so the next await sees this
// step's own.
func (a *pfrAdapter) drain() {
	for {
		select {
		case _, open := <-a.events:
			if !open {
				return
			}
		default:
			return
		}
	}
}

func (a *pfrAdapter) row(what string, ok func(serve.Row) bool) error {
	row, err := waitRow(a.s, a.id, what, ok)
	a.status = string(row.Status)
	return err
}

// settleRow waits for the row to leave running and snapshots the
// transcript after it.
func (a *pfrAdapter) settleRow() error {
	if err := a.row("the turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning }); err != nil {
		return err
	}
	es, err := a.entries()
	a.snap = es
	return err
}

// --- the person ---

// Prompt sends a prompt. Its request is an api turn held at its first
// attempt; on a model whose overflow is sticky the Gate answers it, so
// the turn queued for it is a sentry that must stay untaken.
func (a *pfrAdapter) Prompt() error {
	if !a.on("Prompt", a.req == "none" && !pfrHistoryState(a.snap).open) {
		return nil
	}
	return a.input()
}

func (a *pfrAdapter) Retry() error {
	if !a.on("Retry", a.status == "error") {
		return nil
	}
	return a.input()
}

func (a *pfrAdapter) input() error {
	a.drain()
	before := len(a.snap)
	es, err := a.entries()
	if err == nil {
		before = len(es)
	}
	name := a.name("t")
	if a.overflow {
		a.sentry = name
		control.Queue(a.t, a.dir, name, control.Turn{Mode: "api", Answer: &control.Answer{Kind: "overflow"}})
	} else {
		a.held = name
		control.Queue(a.t, a.dir, name, control.Turn{Mode: "api"})
	}
	ctx, cancel := actionCtx()
	defer cancel()
	text := "prompt " + name
	if err := a.s.Prompt(ctx, a.id, text); err != nil {
		return err
	}
	var at int
	if err := a.waitHist("the input", func(es []history.Entry) bool {
		for i := before; i < len(es); i++ {
			if es[i].Kind == "input" && es[i].Data["text"] == text {
				at = i
				return true
			}
		}
		return false
	}); err != nil {
		return err
	}
	switch a.call {
	case "parked":
		a.call = "job"
	case "ended", "returned":
		a.call = "none"
	}
	a.req, a.attempt, a.strm = "out", 1, 0
	if a.overflow {
		// The state as of the input: the Gate's answer is StickyOverflow.
		a.snap = a.snap[:at+1]
		st, _ := serve.StatusOf(a.snap, true)
		a.status = string(st)
		return nil
	}
	control.WaitAttempt(a.t, a.dir, a.held, 1, actionTimeout)
	return a.row("running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
}

// SwitchModel is the picker's POST /model with a model the session has
// not overflowed on. It reaches the child as a /model line, recorded.
func (a *pfrAdapter) SwitchModel() error {
	if !a.on("SwitchModel", !pfrHistoryState(a.snap).open && a.overflow) {
		return nil
	}
	a.model++
	model := fmt.Sprintf("control-%d", a.model)
	if err := a.post("model", map[string]string{"model": model}); err != nil {
		return err
	}
	if err := a.waitHist("the model switch", func(es []history.Entry) bool {
		for _, e := range es {
			if e.Kind != "model" {
				continue
			}
			b, _ := json.Marshal(e.Data["sets"])
			if strings.Contains(string(b), "model="+model) {
				return true
			}
		}
		return false
	}); err != nil {
		return err
	}
	a.overflow, a.paid = false, 0
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	a.status = string(row.Status)
	return err
}

func (a *pfrAdapter) post(verb string, body any) error {
	b, err := json.Marshal(body)
	if err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.s.URL+"/api/sessions/"+a.id+"/"+verb, bytes.NewReader(b))
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	resp.Body.Close()
	if resp.StatusCode/100 != 2 {
		return fmt.Errorf("POST %s: %d", verb, resp.StatusCode)
	}
	return nil
}

// StopDuringRetry is the composer's Stop while the adapter waits to
// retry: a SIGINT to the child, which closes the turn cancelled (a
// running call cancelled first) and exits, taking any job with it.
func (a *pfrAdapter) StopDuringRetry() error {
	if !a.on("StopDuringRetry", a.req == "retry_wait") {
		return nil
	}
	a.drain()
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Interrupt(ctx, a.id); err != nil {
		return err
	}
	if err := a.waitHist("the cancel", func(es []history.Entry) bool {
		h := pfrHistoryState(es)
		return !h.open && h.closed == "cancelled"
	}); err != nil {
		return err
	}
	if a.call == "running" {
		id := a.callID
		if err := a.waitHist("the cancelled call's end", func(es []history.Entry) bool { return callEnded(es, id) }); err != nil {
			return err
		}
	}
	if err := a.await("the child's exit", func(ev serve.Event) bool { return ev.Kind == "exit" }); err != nil {
		return err
	}
	if a.call == "running" || a.call == "job" {
		a.call, a.gateF = "none", ""
	}
	a.held = ""
	a.req, a.attempt, a.strm = "none", 1, 0
	return a.settleRow()
}

// callEnded: the transcript has the call's recorded end.
func callEnded(es []history.Entry, id string) bool {
	for _, e := range es {
		if e.Kind == "call" && e.Data["id"] == id {
			return true
		}
	}
	return false
}

// --- the provider ---

func (a *pfrAdapter) StreamFragment() error {
	if !a.on("StreamFragment", (a.req == "out" || a.req == "streaming") && !a.overflow) {
		return nil
	}
	a.drain()
	text := "fragment " + a.name("f") + " "
	control.Stream(a.t, a.dir, a.held, text, actionTimeout)
	if err := a.await("the streamed fragment", func(ev serve.Event) bool {
		return ev.Kind == "assistant-delta" && strings.Contains(ev.Text, text)
	}); err != nil {
		return err
	}
	a.req, a.strm = "streaming", a.attempt
	return nil
}

func (a *pfrAdapter) TransientError() error {
	if !a.on("TransientError", (a.req == "out" || a.req == "streaming") && !a.overflow && a.attempt < 2) {
		return nil
	}
	if a.transientAsFatal {
		return a.fail(control.Answer{Kind: "fatal"})
	}
	a.drain()
	shown := a.strm != 0
	control.AnswerWith(a.t, a.dir, a.held, control.Answer{Kind: "transient"})
	control.WaitRetryWait(a.t, a.dir, a.held, actionTimeout)
	if shown {
		if err := a.await("the delta-reset", func(ev serve.Event) bool { return ev.Kind == "delta-reset" }); err != nil {
			return err
		}
	}
	a.attempt++
	a.strm, a.req = 0, "retry_wait"
	return a.row("still running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
}

func (a *pfrAdapter) RetryFires() error {
	if !a.on("RetryFires", a.req == "retry_wait") {
		return nil
	}
	control.Retry(a.t, a.dir, a.held)
	control.WaitAttempt(a.t, a.dir, a.held, a.attempt, actionTimeout)
	a.req = "out"
	return nil
}

func (a *pfrAdapter) RetriesExhausted() error {
	if !a.on("RetriesExhausted", (a.req == "out" || a.req == "streaming") && !a.overflow && a.attempt == 2) {
		return nil
	}
	return a.fail(control.Answer{Kind: "transient"})
}

func (a *pfrAdapter) FatalError() error {
	if !a.on("FatalError", (a.req == "out" || a.req == "streaming") && !a.overflow) {
		return nil
	}
	return a.fail(control.Answer{Kind: "fatal"})
}

func (a *pfrAdapter) ContextOverflow() error {
	if !a.on("ContextOverflow", a.req == "out" && !a.overflow) {
		return nil
	}
	a.overflow = true
	a.paid++
	return a.fail(control.Answer{Kind: "overflow"})
}

// StickyOverflow is the Gate answering the prompt's request itself. The
// sentry queued for it must still be there; if the provider was asked
// after all, it answered 400 again and that is paid.
func (a *pfrAdapter) StickyOverflow() error {
	if !a.on("StickyOverflow", a.req == "out" && a.overflow) {
		return nil
	}
	return a.failed()
}

// fail ends the held attempt with answer and waits for the turn to
// close on its error.
func (a *pfrAdapter) fail(answer control.Answer) error {
	control.AnswerWith(a.t, a.dir, a.held, answer)
	a.held = ""
	return a.failed()
}

func (a *pfrAdapter) failed() error {
	if err := a.waitHist("the error and the done", func(es []history.Entry) bool {
		h := pfrHistoryState(es)
		return h.erred && !h.open && h.closed != ""
	}); err != nil {
		return err
	}
	a.checkSentry()
	if a.call == "running" {
		a.call = "parked"
	}
	a.req, a.attempt, a.strm = "none", 1, 0
	return a.settleRow()
}

// checkSentry counts a sentry the provider was asked for after all as
// a 400 paid, and takes an untaken one out of the queue.
func (a *pfrAdapter) checkSentry() {
	if a.sentry == "" {
		return
	}
	if _, err := os.Stat(filepath.Join(a.dir, a.sentry+".taken")); err == nil {
		a.paid++
	} else {
		os.Remove(filepath.Join(a.dir, a.sentry+".json"))
	}
	a.sentry = ""
}

// CallStart answers with two calls: a bash call that runs until its
// gate file appears, and a call of a tool that does not exist, which
// fails at once. Its result is what sends the next request (another api
// turn) while the bash call runs: the engine asks the model again when
// a result or an input arrives, never for a call that is still running.
func (a *pfrAdapter) CallStart() error {
	if !a.on("CallStart", a.req == "streaming" && a.call == "none") {
		return nil
	}
	a.drain()
	next := a.name("t")
	a.gateF = filepath.Join(a.s.Root, next+".gate")
	a.callID = "toolu_" + next + "_run"
	control.Queue(a.t, a.dir, next, control.Turn{Mode: "api"})
	cmd := fmt.Sprintf("while [ ! -e %s ]; do sleep 0.05; done; echo gate open", a.gateF)
	control.AnswerWith(a.t, a.dir, a.held, control.Answer{Kind: "ok", Calls: []control.Call{
		{ID: a.callID, Name: "bash", Args: map[string]any{"command": cmd}},
		{ID: "toolu_" + next + "_quick", Name: "no_such_tool"},
	}})
	id := a.callID
	if err := a.await("the call's running row", func(ev serve.Event) bool {
		return ev.Kind == "call" && ev.Extra["phase"] == "start" && ev.Extra["id"] == id
	}); err != nil {
		return err
	}
	a.held = next
	control.WaitAttempt(a.t, a.dir, next, 1, actionTimeout)
	a.call, a.req, a.attempt, a.strm = "running", "out", 1, 0
	return nil
}

func (a *pfrAdapter) Finish() error {
	if !a.on("Finish", a.req == "streaming" && a.call != "running") {
		return nil
	}
	return a.end(control.Answer{Kind: "ok", Text: "finished " + a.held})
}

func (a *pfrAdapter) Refusal() error {
	if !a.on("Refusal", (a.req == "out" || a.req == "streaming") && !a.overflow && a.call != "running") {
		return nil
	}
	return a.end(control.Answer{Kind: "refused"})
}

func (a *pfrAdapter) MaxTokensStop() error {
	if !a.on("MaxTokensStop", a.req == "streaming" && a.call != "running") {
		return nil
	}
	return a.end(control.Answer{Kind: "max_tokens", Text: "and then"})
}

// end answers the held attempt with a reply that closes the turn.
func (a *pfrAdapter) end(answer control.Answer) error {
	control.AnswerWith(a.t, a.dir, a.held, answer)
	a.held = ""
	if err := a.waitHist("the turn's done", func(es []history.Entry) bool {
		h := pfrHistoryState(es)
		return !h.open && h.closed != ""
	}); err != nil {
		return err
	}
	a.req, a.attempt, a.strm = "none", 1, 0
	return a.settleRow()
}

// --- the system ---

func (a *pfrAdapter) openGate() error {
	err := os.WriteFile(a.gateF, nil, 0o644)
	a.gateF = ""
	return err
}

func (a *pfrAdapter) CallEnds() error {
	if !a.on("CallEnds", pfrHistoryState(a.snap).open && a.call == "running") {
		return nil
	}
	if err := a.openGate(); err != nil {
		return err
	}
	id := a.callID
	if err := a.waitHist("the call's end", func(es []history.Entry) bool { return callEnded(es, id) }); err != nil {
		return err
	}
	a.call = "returned"
	return nil
}

// CallEndsWhileParked: the parked call's end is recorded and starts
// nothing: no request, no wake turn.
func (a *pfrAdapter) CallEndsWhileParked() error {
	if !a.on("CallEndsWhileParked", a.call == "parked") {
		return nil
	}
	if err := a.openGate(); err != nil {
		return err
	}
	id := a.callID
	if err := a.waitHist("the parked call's end", func(es []history.Entry) bool { return callEnded(es, id) }); err != nil {
		return err
	}
	// A wake would be an input within a moment of the end.
	time.Sleep(500 * time.Millisecond)
	es, err := a.entries()
	if err != nil {
		return err
	}
	a.snap = es
	a.call = "ended"
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	a.status = string(row.Status)
	return err
}

// WakeRequestFails: the unparked job ends while idle and the request
// its end starts fails before the provider (the overflow is sticky:
// the Gate answers, and the sentry must stay untaken) or at it (a 400).
func (a *pfrAdapter) WakeRequestFails() error {
	if !a.on("WakeRequestFails", !pfrHistoryState(a.snap).open && a.call == "job") {
		return nil
	}
	name := a.name("t")
	if a.overflow {
		a.sentry = name
		control.Queue(a.t, a.dir, name, control.Turn{Mode: "api", Answer: &control.Answer{Kind: "overflow"}})
	} else {
		control.Queue(a.t, a.dir, name, control.Turn{Mode: "api", Answer: &control.Answer{Kind: "fatal"}})
	}
	before := len(a.snap)
	if err := a.openGate(); err != nil {
		return err
	}
	if err := a.waitHist("the wake's error and done", func(es []history.Entry) bool {
		wake := false
		for i := before; i < len(es); i++ {
			if es[i].Kind == "input" && es[i].Data["wake"] == true {
				wake = true
			}
		}
		h := pfrHistoryState(es)
		return (wake || h.stray) && h.erred && !h.open
	}); err != nil {
		return err
	}
	a.checkSentry()
	a.call = "none"
	return a.settleRow()
}

// ChildRestart kills the session's child between turns (the archive
// kill, undone); the next prompt or /model starts a fresh process, which
// must still know the model's overflow.
func (a *pfrAdapter) ChildRestart() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return err
	}
	if !a.on("ChildRestart", !pfrHistoryState(a.snap).open && a.call == "none" && row.Live) {
		return nil
	}
	if err := a.kill(); err != nil {
		return err
	}
	row, _, err = a.s.GetSession(ctx, a.id)
	a.status = string(row.Status)
	return err
}

var pfrActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Prompt":              action((*pfrAdapter).Prompt),
	"Retry":               action((*pfrAdapter).Retry),
	"SwitchModel":         action((*pfrAdapter).SwitchModel),
	"StopDuringRetry":     action((*pfrAdapter).StopDuringRetry),
	"StreamFragment":      action((*pfrAdapter).StreamFragment),
	"TransientError":      action((*pfrAdapter).TransientError),
	"RetryFires":          action((*pfrAdapter).RetryFires),
	"RetriesExhausted":    action((*pfrAdapter).RetriesExhausted),
	"FatalError":          action((*pfrAdapter).FatalError),
	"ContextOverflow":     action((*pfrAdapter).ContextOverflow),
	"StickyOverflow":      action((*pfrAdapter).StickyOverflow),
	"CallStart":           action((*pfrAdapter).CallStart),
	"Finish":              action((*pfrAdapter).Finish),
	"Refusal":             action((*pfrAdapter).Refusal),
	"MaxTokensStop":       action((*pfrAdapter).MaxTokensStop),
	"CallEnds":            action((*pfrAdapter).CallEnds),
	"CallEndsWhileParked": action((*pfrAdapter).CallEndsWhileParked),
	"WakeRequestFails":    action((*pfrAdapter).WakeRequestFails),
	"ChildRestart":        action((*pfrAdapter).ChildRestart),
}}

// walkPFR drives the adapter down every walk in raw (pathsJSON's shape)
// and compares the whole role state after Init and after every step.
func walkPFR(a *pfrAdapter, raw []byte) error {
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		return err
	}
	acts := pfrActions["Session"]
	for pi, p := range doc.Paths {
		var names []string
		for _, st := range p.Trace {
			names = append(names, strings.TrimPrefix(st.Action, "Session#0."))
		}
		fail := func(i int, format string, args ...any) error {
			a.Cleanup()
			lo := max(0, i-6)
			return fmt.Errorf("walk %d, step %d (%s) after %v: %s", pi, i, names[i], names[lo:i], fmt.Sprintf(format, args...))
		}
		for i, st := range p.Trace {
			if i == 0 {
				if err := a.Init(); err != nil {
					return fail(i, "%v", err)
				}
			} else {
				f, ok := acts[names[i]]
				if !ok {
					return fail(i, "no such action")
				}
				if _, err := f(a, nil); err != nil {
					return fail(i, "%v", err)
				}
			}
			got, _ := a.GetState()
			for k, v := range st.State {
				field, ok := strings.CutPrefix(k, "Session#0.")
				if !ok {
					continue
				}
				if !pfrSame(got[field], v) {
					return fail(i, "%s: model %v, server %v (whole state %v)", field, v, got[field], got)
				}
			}
		}
		if err := a.Cleanup(); err != nil {
			return fmt.Errorf("walk %d cleanup: %w", pi, err)
		}
	}
	return nil
}

// pfrSame compares a field through JSON: the walks' numbers are
// float64, the adapter's ints.
func pfrSame(a, b any) bool {
	x, _ := json.Marshal(a)
	y, _ := json.Marshal(b)
	return bytes.Equal(x, y)
}

func TestProviderFailureRetryPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	raw, err := pathsJSON("provider_failure_retry")
	if err != nil {
		t.Fatal(err)
	}
	a := newPFRAdapter(t)
	err = walkPFR(a, raw)
	t.Logf("actions run: %v", a.ran)
	if err != nil {
		t.Fatal(err)
	}
	checkPFRHistories(t, a)
}

// The runner's random walks, in the exhaustive run only (runMBT).
func TestProviderFailureRetry(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPFRAdapter(t)
	err := runMBT(t, "provider_failure_retry", a, pfrActions, map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0})
	t.Logf("actions run past the gate: %v", a.ran)
	if err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkPFRHistories(t, a)
}

// A transient failure answered as a fatal one must fail the walk: it
// shows on the TransientError transitions only, so this walks every
// transition.
func TestProviderFailureRetryCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	raw, err := pathsJSONCover("provider_failure_retry", tracecheck.CoverTransitions)
	if err != nil {
		t.Fatal(err)
	}
	a := newPFRAdapter(t)
	a.transientAsFatal = true
	err = walkPFR(a, raw)
	if err == nil {
		t.Fatal("walks whose transient failures were answered as fatal passed; the walk is not checking state")
	}
	t.Logf("caught: %v", err)
}

func checkPFRHistories(t *testing.T, a *pfrAdapter) {
	t.Helper()
	g, err := tracecheck.Load(fizzCheck(t, "provider_failure_retry"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		es := sessionHistory(t, a.s.Home, id)
		checkHistory(t, g, es, pfrHistory)
		if t.Failed() {
			t.Logf("transcript %s:\n%s", id, pfrDump(es))
			return
		}
	}
}

// pfrHistory reads the abstract trace off a transcript. What the
// transcript does not record is put back where the recorded step needs
// it: a reply or a call implies a fragment streamed before it, an
// overloaded error the retry and its exhaustion, a cancel the retry
// wait it stopped. Retry leaves the same input Prompt does, and a child
// restart (a self-loop) nothing at all.
//
// A reply's calls are recorded when they end, so CallStart is read off
// the call of that reply that failed at once, and the call that ran on
// by its end: inside the turn (CallEnds), or adopted when the turn
// failed (parked until an input, a job after one).
func pfrHistory(es []history.Entry) []tracecheck.Step {
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Session#0.status": "idle"}}}
	add := func(action string, state map[string]any) {
		steps = append(steps, tracecheck.Step{Action: "Session#0." + action, State: state})
	}
	st := func(s string) map[string]any { return map[string]any{"Session#0.status": s} }
	var (
		open, wake, streamed, cut, overflow bool
		call                                = "none"
		errText                             string
		erred, overflowErr                  bool
	)
	fragment := func() {
		if !streamed {
			add("StreamFragment", nil)
			streamed = true
		}
	}
	for _, e := range es {
		switch e.Kind {
		case "model":
			if !open && overflow {
				add("SwitchModel", nil)
				overflow = false
			}
		case "input":
			if e.Data["wake"] == true {
				add("WakeRequestFails", st("error"))
				call = "none"
				open, wake = true, true
				continue
			}
			switch call {
			case "parked":
				call = "job"
			case "ended", "returned":
				call = "none"
			}
			add("Prompt", st("running"))
			open, wake, streamed, cut, erred, overflowErr, errText = true, false, false, false, false, false, ""
		case "assistant":
			if open {
				fragment()
			}
		case "call":
			callErr, _ := e.Data["error"].(string)
			switch {
			case e.Data["adopted"] == true:
				// A job's end: muted while parked, else the wake after it.
				if !open && call == "parked" {
					add("CallEndsWhileParked", nil)
					call = "ended"
				}
			case !open || wake:
			case e.Data["canceled"] == true:
				// Cancelled by the Stop that follows.
			case strings.HasPrefix(callErr, "interrupted:"):
				// A call a Stop's exit took, reported by the next child.
			case callErr != "" && call == "none":
				fragment()
				add("CallStart", nil)
				call, streamed = "running", false
			case e.Data["adopted"] != true && call == "running":
				add("CallEnds", nil)
				call = "returned"
			}
		case "system":
			if s, _ := e.Data["text"].(string); open && strings.HasPrefix(s, pfrCutNote) {
				cut = true
			}
		case "error":
			if open {
				erred = true
				errText, _ = e.Data["text"].(string)
				_, overflowErr = e.Data["overflow"].(string)
			}
		case "cancelled":
			if open && !wake {
				add("TransientError", nil)
				add("StopDuringRetry", st("stopped"))
				if call == "running" || call == "job" {
					call = "none"
				}
				open = false
			}
		case "done":
			if !open {
				continue
			}
			open = false
			if wake {
				wake = false
				continue
			}
			switch {
			case erred && strings.Contains(errText, "declined"):
				add("Refusal", st("error"))
			case erred && overflowErr && overflow:
				add("StickyOverflow", st("error"))
			case erred && overflowErr:
				add("ContextOverflow", st("error"))
				overflow = true
			case erred && strings.Contains(errText, "Overloaded"):
				add("TransientError", nil)
				add("RetryFires", nil)
				add("RetriesExhausted", st("error"))
			case erred:
				add("FatalError", st("error"))
			case cut:
				fragment()
				add("MaxTokensStop", map[string]any{"Session#0.status": "done", "Session#0.cut": "cut"})
			default:
				fragment()
				add("Finish", st("done"))
			}
			if erred && call == "running" {
				call = "parked"
			}
		}
	}
	return steps
}

func init() { historyProjections["provider_failure_retry"] = pfrHistory }
