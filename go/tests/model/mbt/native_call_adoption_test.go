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
	"os/exec"
	"path/filepath"
	"slices"
	"strconv"
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

// specs/native_call_adoption.fizz against a real serve: an engine call
// that outlives turn_settle is adopted as a job, its turn closes with
// done{running: 1}, and the call then ends on its own, is killed, or
// loses its child.
//
// The model is llm-control; every model request takes a queued "block"
// turn and is held until the adapter answers it. The long call is a
// bash call waiting on a gate file, so the adapter decides when it ends.
// turn_settle is ncaSettle: short enough to walk, long enough that the
// person's steps before it (Steer, CallEnds) land inside the window.
//
// The spec splits what the product does in one go: a kill is sent
// (KillJob) and then acted on (KillLands), an idle end is recorded
// (CallEnds) and then wakes the model (WakeRequest), a steer is sent
// (Steer) and then taken (SteerLands). The product does the second half
// at once, so the adapter keeps the spec's fields as of the step it
// took, and each step waits for the record that proves the product did
// that step. A walk that asks for another step in between gets
// fmbt.ErrNotImplemented when the product already took the second
// half (a prompt after the wake opened, anything but KillLands after a
// kill landed): the product took the other branch, and the walk's check
// ends there, counted as notTaken.
//
// What the product decides is read off it: the job's record, its
// finishes and the end row from history, Row.jobs (listed) from the
// row, the page's Work word and footer count (work, settled) from the
// transcript a CatchUp fetches.
type ncaAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	walk  int
	turns int // llm-control turn names, unique across walks
	id    string
	cwd   string
	ids   []string

	gateF    string // the long call's gate: it exits once this exists
	startedF string // the long call writes this as it starts
	callID   string
	jobID    int
	endSeq   int64 // the recorded end row's seq, 0 before it
	steerTxt string

	// The spec's role, field for field.
	status      string
	turn        string
	inflight    bool
	steerQ      bool
	steers      int
	call        string
	job         string
	jobGen      int
	finishes    int
	killReq     bool
	killed      bool
	wakePending bool
	wakes       int
	child       string
	gen         int
	footRunning int
	settled     int
	endRow      string
	turnDones   int
	behind      bool
	work        string
	listed      bool
	prompts     int

	stats map[string]int

	// endAsKill is the deliberate bug the wrong-adapter test injects:
	// CallEnds of an adopted call kills the job instead of letting its
	// command exit.
	endAsKill bool
}

// ncaSettle is the engine's turn_settle for the walks.
const ncaSettle = 4 * time.Second

const ncaConfig = controlConfig + "- id: loop\n  plugin: engine-unreal\n  config:\n    turn_settle: 4s\n"

func newNCAAdapter(t *testing.T) *ncaAdapter {
	s := servetest.Start(t, servetest.Options{Config: ncaConfig})
	a := &ncaAdapter{t: t, s: s, dir: control.Dir(s.Home), stats: map[string]int{}}
	t.Cleanup(func() {
		// No long call outlives the test, whatever step it stopped on.
		files, _ := filepath.Glob(filepath.Join(s.Root, "gate-*"))
		_ = files
		for i := 1; i <= a.walk; i++ {
			os.WriteFile(filepath.Join(s.Root, fmt.Sprintf("gate-%d", i)), nil, 0o644)
		}
	})
	return a
}

func (a *ncaAdapter) Init() error {
	a.walk++
	a.gate.reset()
	a.topUp()
	a.cwd = a.s.Dir(a.t, fmt.Sprintf("w%d", a.walk))
	a.gateF = filepath.Join(a.s.Root, fmt.Sprintf("gate-%d", a.walk))
	a.startedF = filepath.Join(a.s.Root, fmt.Sprintf("started-%d", a.walk))
	a.callID, a.jobID, a.endSeq, a.steerTxt = "", 0, 0, ""
	a.status, a.turn, a.inflight, a.steerQ, a.steers = "idle", "none", false, false, 0
	a.call, a.job, a.jobGen, a.finishes = "none", "none", 0, 0
	a.killReq, a.killed, a.wakePending, a.wakes = false, false, false, 0
	a.child, a.gen, a.footRunning, a.settled = "alive", 0, 0, 0
	a.endRow, a.turnDones, a.behind, a.work, a.listed, a.prompts = "none", 0, false, "none", false, 0
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.cwd, "")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	_, err = waitRow(a.s, a.id, "the idle session's child", func(r serve.Row) bool { return r.Live })
	return err
}

// Cleanup archives the walk's session (its child is killed and reaped),
// lets its long call exit and answers whatever request is still held,
// so the next walk's requests take its own turns.
func (a *ncaAdapter) Cleanup() error {
	if a.gateF != "" {
		os.WriteFile(a.gateF, nil, 0o644)
	}
	if a.id == "" {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Archive(ctx, a.id)
	for _, name := range a.held() {
		control.Release(a.t, a.dir, name)
	}
	a.topUp()
	return err
}

func (a *ncaAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

func (a *ncaAdapter) GetState() (map[string]any, error) {
	return map[string]any{
		"status": a.status, "turn": a.turn, "inflight": a.inflight,
		"steer_q": a.steerQ, "steers": a.steers, "call": a.call,
		"job": a.job, "job_gen": a.jobGen, "finishes": a.finishes,
		"kill_req": a.killReq, "killed": a.killed, "wake_pending": a.wakePending,
		"wakes": a.wakes, "child": a.child, "gen": a.gen,
		"foot_running": a.footRunning, "settled": a.settled, "end_row": a.endRow,
		"turn_dones": a.turnDones, "behind": a.behind, "work": a.work,
		"listed": a.listed, "still": a.footRunning - a.settled, "prompts": a.prompts,
	}, nil
}

// ---- the person ----

func (a *ncaAdapter) Prompt() error {
	if !a.gate.pass(a.prompts == 0 && a.turn == "none") {
		return nil
	}
	if err := a.prompt(fmt.Sprintf("walk %d: run the long build", a.walk)); err != nil {
		return err
	}
	a.prompts = 1
	return a.opened("user")
}

// armed is the spec's: the settle timer runs.
func (a *ncaAdapter) armed() bool {
	return a.turn == "user" && !a.inflight && !a.steerQ && a.call == "fg"
}

// Steer is Enter while the turn is live: sent, not waited for.
func (a *ncaAdapter) Steer() error {
	if !a.gate.pass(a.armed() && a.steers < 1) {
		return nil
	}
	if err := a.notKilled(); err != nil {
		return err
	}
	a.steerTxt = fmt.Sprintf("walk %d: also check the logs", a.walk)
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, a.steerTxt); err != nil {
		return err
	}
	a.steers++
	a.steerQ = true
	return nil
}

func (a *ncaAdapter) PromptWhileAdopted() error {
	if !a.gate.pass(a.turn == "none" && a.child == "alive" && (a.job == "running" || a.wakePending) && a.prompts < 2) {
		return nil
	}
	if err := a.notKilled(); err != nil {
		return err
	}
	if a.wakePending {
		// The coordinator asks the model the moment the end is recorded;
		// a prompt only beats it when the wake is not out yet.
		if es, _ := a.entries(); slices.ContainsFunc(es, isWake) {
			return fmbt.ErrNotImplemented
		}
	}
	if err := a.prompt(fmt.Sprintf("walk %d: meanwhile, a question", a.walk)); err != nil {
		return err
	}
	a.prompts++
	a.wakePending = false
	return a.opened("user")
}

// KillJob is the strip's stop: POST .../jobs/N/kill. The child acts on
// it at once, so the step waits for that (the job's finish) and keeps
// the spec's view of it as sent, not yet acted on, until KillLands.
func (a *ncaAdapter) KillJob() error {
	if !a.gate.pass(a.listed && !a.killReq) {
		return nil
	}
	if err := a.killJob(); err != nil {
		return err
	}
	if _, err := a.waitEntries("the kill's finish", func(es []history.Entry) bool {
		st, _ := jobState(es, a.callID)
		return st != "running"
	}); err != nil {
		return err
	}
	a.killReq = true
	return nil
}

func (a *ncaAdapter) PromptAfterExit() error {
	if !a.gate.pass(a.child == "dead" && a.turn == "none") {
		return nil
	}
	if err := a.prompt(fmt.Sprintf("walk %d: are you still there?", a.walk)); err != nil {
		return err
	}
	if a.call == "lost" {
		// The resumed child's catch-up records the lost call interrupted.
		if _, err := a.waitEntries("the lost call's interrupted end", func(es []history.Entry) bool {
			return endRowName(endRowOf(es, a.callID)) == "interrupted"
		}); err != nil {
			return err
		}
	}
	a.child = "alive"
	a.gen++
	a.prompts++
	return a.opened("user")
}

// ---- the model ----

// CallsBash answers the first request with the long bash call and
// waits for its command to start.
func (a *ncaAdapter) CallsBash() error {
	if !a.gate.pass(a.turn == "user" && a.inflight && a.call == "none" && a.prompts == 1) {
		return nil
	}
	held := a.held()
	if len(held) != 1 {
		return fmt.Errorf("CallsBash: want one request in flight, have %v", held)
	}
	cmd := fmt.Sprintf("touch %s; while [ ! -e %s ]; do sleep 0.05; done; echo built", a.startedF, a.gateF)
	control.ReleaseWith(a.t, a.dir, held[0], control.Turn{Bash: cmd})
	a.callID = "control_" + held[0]
	a.topUp()
	if err := a.waitFile(a.startedF, "the long call to start"); err != nil {
		return err
	}
	a.inflight = false
	a.call = "fg"
	a.behind = true
	return nil
}

// Reply answers the request in flight with text. With the call still
// foreground the turn stays open (the settle re-arms); with a steer
// waiting the model is asked again; otherwise every request the turn
// makes is answered until it closes (an adopted end that reached the
// model inside the turn asks it once more).
func (a *ncaAdapter) Reply() error {
	if !a.gate.pass(a.turn != "none" && a.inflight) {
		return nil
	}
	if err := a.notKilled(); err != nil {
		return err
	}
	since := a.lastSeq()
	switch {
	case a.turn == "user" && a.call == "fg":
		if err := a.releaseHeld(); err != nil {
			return err
		}
		if _, err := a.waitEntries("the reply's assistant entry", func(es []history.Entry) bool {
			return slices.ContainsFunc(es, func(e history.Entry) bool { return e.Seq > since && e.Kind == "assistant" })
		}); err != nil {
			return err
		}
		if err := a.quiet(); err != nil {
			return err
		}
		a.inflight = false
	case a.steerQ:
		if err := a.releaseHeld(); err != nil {
			return err
		}
		if err := a.steerLanded(); err != nil {
			return err
		}
		a.steerQ = false
	default:
		row, err := a.untilDone(since)
		if err != nil {
			return err
		}
		if err := a.closed(row, 0); err != nil {
			return err
		}
	}
	a.behind = true
	return nil
}

// ---- the system ----

func (a *ncaAdapter) SteerLands() error {
	if !a.gate.pass(a.steerQ) {
		return nil
	}
	if err := a.steerLanded(); err != nil {
		return err
	}
	a.steerQ = false
	a.inflight = true
	a.behind = true
	return nil
}

// SettleFires waits out turn_settle: the call is adopted as a job and
// the turn closes with done{running: 1}. A steer sent in the window
// lands instead.
func (a *ncaAdapter) SettleFires() error {
	if !a.gate.pass(a.turn == "user" && !a.inflight && a.call == "fg") {
		return nil
	}
	if a.steerQ {
		return a.SteerLands()
	}
	since := a.lastSeq()
	var es []history.Entry
	var err error
	ctx, cancel := context.WithTimeout(context.Background(), ncaSettle+actionTimeout)
	defer cancel()
	es, err = a.waitEntriesCtx(ctx, "the call's adoption and its turn's done", func(es []history.Entry) bool {
		st, _ := jobState(es, a.callID)
		return st != "none" && slices.ContainsFunc(es, func(e history.Entry) bool { return e.Seq > since && e.Kind == "done" })
	})
	if err != nil {
		return err
	}
	a.jobID = jobOf(es, a.callID)
	row, err := waitRow(a.s, a.id, "the adopting turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning })
	if err != nil {
		return err
	}
	a.call = "adopted"
	a.jobGen = a.gen
	return a.closed(row, 1)
}

// CallEnds opens the gate: the command exits. A foreground call's
// result goes to the model in its turn; an adopted one's end row and
// the job's finish are recorded.
func (a *ncaAdapter) CallEnds() error {
	if !a.gate.pass((a.call == "fg" || a.call == "adopted") && a.child == "alive") {
		return nil
	}
	if err := a.notKilled(); err != nil {
		return err
	}
	if a.call == "fg" {
		os.WriteFile(a.gateF, nil, 0o644)
		es, err := a.waitEntries("the call's end row", func(es []history.Entry) bool { return endRowOf(es, a.callID) != nil })
		if err != nil {
			return err
		}
		if err := a.waitHeld("the model to be asked with the result"); err != nil {
			return err
		}
		a.call = "ended"
		a.endRow = endRowName(endRowOf(es, a.callID))
		a.inflight = true
		a.behind = true
		return nil
	}
	if a.endAsKill {
		if err := a.killJob(); err != nil {
			return err
		}
	} else {
		os.WriteFile(a.gateF, nil, 0o644)
	}
	return a.endAdopted()
}

// KillLands: the child acted on /jobkill N (KillJob waited for it).
func (a *ncaAdapter) KillLands() error {
	if !a.gate.pass(a.killReq) {
		return nil
	}
	if a.job != "running" {
		a.killReq = false
		return nil
	}
	a.killReq = false
	a.killed = true
	return a.endAdopted()
}

// WakeRequest: the coordinator asks the model about the ended call on
// its own, a wake turn.
func (a *ncaAdapter) WakeRequest() error {
	if !a.gate.pass(a.wakePending && a.turn == "none" && a.child == "alive") {
		return nil
	}
	after := a.endSeq
	if _, err := a.waitEntries("the wake input", func(es []history.Entry) bool {
		return slices.ContainsFunc(es, func(e history.Entry) bool { return e.Seq > after && isWake(e) })
	}); err != nil {
		return err
	}
	a.wakePending = false
	a.wakes++
	return a.opened("wake")
}

// ChildExits SIGKILLs the session's child (a crash; a serve restart
// kills children the same way) and waits for serve to see it gone.
func (a *ncaAdapter) ChildExits() error {
	if !a.gate.pass(a.child == "alive" && a.gen == 0 && a.turn == "none" && a.job == "running" && !a.wakePending) {
		return nil
	}
	if err := a.notKilled(); err != nil {
		return err
	}
	if err := a.killChild(); err != nil {
		return err
	}
	// Its orphaned command is nobody's now; let it go.
	os.WriteFile(a.gateF, nil, 0o644)
	es, err := a.entries()
	if err != nil {
		return err
	}
	a.child = "dead"
	a.call = "lost"
	a.job, a.finishes = jobState(es, a.callID)
	a.killReq = false
	a.behind = true
	a.status = "idle"
	return a.readListed()
}

// ---- the page ----

// CatchUp is the page's GET landing: Work's word for the job and the
// footer's count of reported calls, read off the transcript as
// app.tsx's groupTurns reads them.
func (a *ncaAdapter) CatchUp() error {
	if !a.gate.pass(a.behind) {
		return nil
	}
	if err := a.notKilled(); err != nil {
		return err
	}
	es, err := a.entries()
	if err != nil {
		return err
	}
	a.work, _ = jobState(es, a.callID)
	a.settled = footerSettled(es)
	a.behind = false
	return nil
}

// ---- helpers ----

// notKilled ends the walk's check when a kill already landed and the
// walk asks for any step but KillLands.
func (a *ncaAdapter) notKilled() error {
	if a.killReq {
		return fmbt.ErrNotImplemented
	}
	return nil
}

// prompt sends text the way the composer does and waits for its turn:
// the input recorded, the model's request held, the row running.
func (a *ncaAdapter) prompt(text string) error {
	a.topUp()
	since := a.lastSeq()
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, text); err != nil {
		return err
	}
	if _, err := a.waitEntries("the prompt's input", func(es []history.Entry) bool {
		return slices.ContainsFunc(es, func(e history.Entry) bool {
			return e.Seq > since && e.Kind == "input" && strings.TrimSpace(entryText(e)) == text
		})
	}); err != nil {
		return err
	}
	return a.waitHeld("the prompt's model request")
}

// opened is the spec's open(kind), with the row read off the server.
func (a *ncaAdapter) opened(kind string) error {
	row, err := waitRow(a.s, a.id, "the turn to run", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
	if err != nil {
		return err
	}
	es, err := a.entries()
	if err != nil {
		return err
	}
	a.endRow = endRowName(endRowOf(es, a.callID))
	a.turn = kind
	a.inflight = true
	a.turnDones = 0
	a.behind = true
	a.status = ncaStatus(row)
	a.listed = hasJob(row, a.jobID)
	return nil
}

// closed is the spec's close(running): the done's running, and how many
// dones the turn has, read off the history.
func (a *ncaAdapter) closed(row serve.Row, _ int) error {
	es, err := a.entries()
	if err != nil {
		return err
	}
	a.endRow = endRowName(endRowOf(es, a.callID))
	a.turn = "none"
	a.inflight = len(a.held()) > 0
	a.turnDones = turnDones(es)
	a.footRunning = footRunning(es)
	a.job, a.finishes = jobState(es, a.callID)
	a.behind = true
	a.status = ncaStatus(row)
	a.listed = hasJob(row, a.jobID)
	return nil
}

// endAdopted waits for the adopted call's end row and the job's finish
// (the spec's end_adopted).
func (a *ncaAdapter) endAdopted() error {
	es, err := a.waitEntries("the adopted call's end row and finish", func(es []history.Entry) bool {
		st, _ := jobState(es, a.callID)
		return endRowOf(es, a.callID) != nil && st != "running"
	})
	if err != nil {
		return err
	}
	end := endRowOf(es, a.callID)
	a.endSeq = end.Seq
	a.call = "ended"
	a.endRow = endRowName(end)
	a.job, a.finishes = jobState(es, a.callID)
	a.behind = true
	if a.turn != "none" {
		a.inflight = true
	} else {
		a.wakePending = true
	}
	return a.readListed()
}

func (a *ncaAdapter) steerLanded() error {
	text := a.steerTxt
	if _, err := a.waitEntries("the steer's input", func(es []history.Entry) bool {
		return slices.ContainsFunc(es, func(e history.Entry) bool {
			return e.Kind == "input" && e.Data["steer"] == true && strings.Contains(entryText(e), text)
		})
	}); err != nil {
		return err
	}
	return a.waitHeld("the model to be asked with the steer")
}

// untilDone answers every request of the open turn with text until its
// done is recorded after since.
func (a *ncaAdapter) untilDone(since int64) (serve.Row, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	for {
		for _, name := range a.held() {
			control.ReleaseWith(a.t, a.dir, name, control.Turn{Text: "reply " + name})
		}
		a.topUp()
		es, _ := a.entries()
		if slices.ContainsFunc(es, func(e history.Entry) bool { return e.Seq > since && e.Kind == "done" }) {
			return waitRow(a.s, a.id, "the turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning })
		}
		select {
		case <-ctx.Done():
			return serve.Row{}, fmt.Errorf("waiting for the turn's done: transcript %s", ncaKinds(es))
		case <-time.After(50 * time.Millisecond):
		}
	}
}

func (a *ncaAdapter) releaseHeld() error {
	held := a.held()
	if len(held) == 0 {
		return errors.New("no model request in flight to answer")
	}
	for _, name := range held {
		control.ReleaseWith(a.t, a.dir, name, control.Turn{Text: "reply " + name})
	}
	a.topUp()
	return nil
}

// quiet checks nothing asks the model again for a moment: the turn
// waits on its call alone.
func (a *ncaAdapter) quiet() error {
	time.Sleep(300 * time.Millisecond)
	if h := a.held(); len(h) > 0 {
		return fmt.Errorf("the model was asked again (%v) while the turn should wait on its call", h)
	}
	return nil
}

func (a *ncaAdapter) readListed() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return err
	}
	a.listed = hasJob(row, a.jobID)
	return nil
}

func (a *ncaAdapter) killJob() error {
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodPost,
		a.s.URL+"/api/sessions/"+url.PathEscape(a.id)+"/jobs/"+strconv.Itoa(a.jobID)+"/kill", bytes.NewReader(nil))
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode/100 != 2 {
		return fmt.Errorf("kill job %d: %s", a.jobID, resp.Status)
	}
	return nil
}

// killChild SIGKILLs the child whose cwd is the walk's own (a created
// session's command line does not carry its id).
func (a *ncaAdapter) killChild() error {
	out, _ := exec.Command("lsof", "-a", "-d", "cwd", "-c", "bough", "-Fpn").Output()
	pid, killed := 0, false
	for _, l := range strings.Split(string(out), "\n") {
		switch {
		case strings.HasPrefix(l, "p"):
			pid, _ = strconv.Atoi(l[1:])
		case strings.HasPrefix(l, "n") && l[1:] == a.cwd && pid > 0:
			if syscall.Kill(pid, syscall.SIGKILL) == nil {
				killed = true
			}
		}
	}
	if !killed {
		return fmt.Errorf("no bough child in %s to kill", a.cwd)
	}
	_, err := waitRow(a.s, a.id, "the child to be gone", func(r serve.Row) bool { return !r.Live })
	return err
}

func (a *ncaAdapter) entries() ([]history.Entry, error) {
	return history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
}

func (a *ncaAdapter) lastSeq() int64 {
	es, _ := a.entries()
	var n int64
	for _, e := range es {
		n = max(n, e.Seq)
	}
	return n
}

func (a *ncaAdapter) waitEntries(what string, ok func([]history.Entry) bool) ([]history.Entry, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	return a.waitEntriesCtx(ctx, what, ok)
}

func (a *ncaAdapter) waitEntriesCtx(ctx context.Context, what string, ok func([]history.Entry) bool) ([]history.Entry, error) {
	var es []history.Entry
	for {
		if got, err := a.entries(); err == nil {
			es = got
			if ok(es) {
				return es, nil
			}
		}
		select {
		case <-ctx.Done():
			return es, fmt.Errorf("waiting for %s: transcript %s", what, ncaKinds(es))
		case <-time.After(50 * time.Millisecond):
		}
	}
}

func (a *ncaAdapter) waitHeld(what string) error {
	ctx, cancel := actionCtx()
	defer cancel()
	for len(a.held()) == 0 {
		select {
		case <-ctx.Done():
			es, _ := a.entries()
			return fmt.Errorf("waiting for %s: transcript %s", what, ncaKinds(es))
		case <-time.After(20 * time.Millisecond):
		}
	}
	return nil
}

func (a *ncaAdapter) waitFile(p, what string) error {
	ctx, cancel := actionCtx()
	defer cancel()
	for {
		if _, err := os.Stat(p); err == nil {
			return nil
		}
		select {
		case <-ctx.Done():
			es, _ := a.entries()
			return fmt.Errorf("waiting for %s: transcript %s", what, ncaKinds(es))
		case <-time.After(20 * time.Millisecond):
		}
	}
}

// topUp keeps two block turns queued, so every model request is held
// until the adapter answers it.
func (a *ncaAdapter) topUp() {
	names, _ := filepath.Glob(filepath.Join(a.dir, "*.json"))
	for n := len(names); n < 2; n++ {
		a.turns++
		control.Queue(a.t, a.dir, fmt.Sprintf("q%08d", a.turns), control.Turn{Mode: "block", Text: "answered"})
	}
}

// held lists the requests in flight: taken and not released.
func (a *ncaAdapter) held() []string {
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

func ncaStatus(r serve.Row) string {
	if r.Status == serve.StatusRunning {
		return "running"
	}
	return "idle"
}

func hasJob(r serve.Row, id int) bool {
	return id != 0 && slices.ContainsFunc(r.Jobs, func(j serve.Job) bool { return j.ID == id })
}

func entryText(e history.Entry) string { s, _ := e.Data["text"].(string); return s }

func isWake(e history.Entry) bool { return e.Kind == "input" && e.Data["wake"] == true }

func typedJobEntry(e history.Entry) bool {
	_, ok := e.Data["event"].(string)
	return e.Kind == "job" && ok
}

// jobState is the adopted call's job as history records it: none,
// running, finished or stopped, and how many finishes it has.
func jobState(es []history.Entry, call string) (string, int) {
	st, n := "none", 0
	if call == "" {
		return st, n
	}
	for _, e := range es {
		if !typedJobEntry(e) || e.Data["call"] != call {
			continue
		}
		switch e.Data["event"] {
		case "started":
			st = "running"
		case "finished":
			n++
			if e.Data["stopped"] == true {
				st = "stopped"
			} else {
				st = "finished"
			}
		}
	}
	return st, n
}

func jobOf(es []history.Entry, call string) int {
	for _, e := range es {
		if typedJobEntry(e) && e.Data["call"] == call && e.Data["event"] == "started" {
			n, _ := e.Data["id"].(float64)
			return int(n)
		}
	}
	return 0
}

// endRowOf is the call's recorded end row, nil before it.
func endRowOf(es []history.Entry, call string) *history.Entry {
	for i, e := range es {
		if e.Kind == "call" && call != "" && e.Data["id"] == call {
			return &es[i]
		}
	}
	return nil
}

func endRowName(e *history.Entry) string {
	switch {
	case e == nil:
		return "none"
	case interrupted(*e):
		return "interrupted"
	case e.Data["adopted"] == true:
		return "adopted"
	}
	return "fg"
}

// interrupted is the end row a resumed child records for a call its
// predecessor died under.
func interrupted(e history.Entry) bool {
	s, _ := e.Data["error"].(string)
	return strings.HasPrefix(s, "interrupted")
}

// turnDones counts the dones since the last input that opened a turn
// (a steer does not).
func turnDones(es []history.Entry) int {
	n := 0
	for _, e := range es {
		switch {
		case e.Kind == "input" && e.Data["steer"] != true:
			n = 0
		case e.Kind == "done":
			n++
		}
	}
	return n
}

// footRunning is done.running of the last turn that left calls running.
func footRunning(es []history.Entry) int {
	n := 0
	for _, e := range es {
		if e.Kind == "done" {
			if r, _ := e.Data["running"].(float64); r > 0 {
				n = int(r)
			}
		}
	}
	return n
}

// footerSettled is groupTurns' settled for the last turn whose done left
// calls running: of the calls its job lines named, those whose end row
// or job finish has been recorded since.
func footerSettled(es []history.Entry) int {
	var ids map[string]bool
	ended := map[string]bool{}
	var body []history.Entry
	for _, e := range es {
		if e.Kind == "input" && e.Data["steer"] != true {
			body = nil
		}
		if ids != nil {
			id := ""
			switch {
			case e.Kind == "call":
				id, _ = e.Data["id"].(string)
			case typedJobEntry(e) && e.Data["event"] == "finished":
				id, _ = e.Data["call"].(string)
			}
			if ids[id] {
				ended[id] = true
			}
		}
		body = append(body, e)
		if r, _ := e.Data["running"].(float64); e.Kind == "done" && r > 0 {
			ids, ended = map[string]bool{}, map[string]bool{}
			for _, b := range body {
				if c, ok := b.Data["call"].(string); ok && typedJobEntry(b) && b.Data["event"] == "started" {
					ids[c] = true
				}
			}
		}
	}
	return len(ended)
}

func ncaKinds(es []history.Entry) string {
	var b strings.Builder
	for _, e := range es {
		switch e.Kind {
		case "meta", "usage", "title", "summary":
			continue
		}
		fmt.Fprintf(&b, "%d:%s", e.Seq, e.Kind)
		switch {
		case e.Kind == "input":
			fmt.Fprintf(&b, "(steer=%v wake=%v)", e.Data["steer"] == true, e.Data["wake"] == true)
		case e.Kind == "job":
			fmt.Fprintf(&b, "(%v %v stopped=%v)", e.Data["event"], e.Data["id"], e.Data["stopped"] == true)
		case e.Kind == "call":
			d := map[string]any{}
			for k, v := range e.Data {
				if k != "output" && k != "text" {
					d[k] = v
				}
			}
			fmt.Fprintf(&b, "(%v)", d)
		case e.Kind == "done":
			fmt.Fprintf(&b, "(running=%v)", e.Data["running"])
		}
		b.WriteString(" ")
	}
	return b.String()
}

// ncaActions counts what each step did: done, skipped (gated off) or
// notTaken (the product took the other branch).
func ncaActions(a *ncaAdapter) map[string]map[string]fmbt.ActionFunc {
	acts := map[string]map[string]fmbt.ActionFunc{"Session": {}, "": {}}
	for role, m := range ncaActionTable {
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

var ncaActionTable = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Prompt":             action((*ncaAdapter).Prompt),
	"Steer":              action((*ncaAdapter).Steer),
	"PromptWhileAdopted": action((*ncaAdapter).PromptWhileAdopted),
	"KillJob":            action((*ncaAdapter).KillJob),
	"PromptAfterExit":    action((*ncaAdapter).PromptAfterExit),
	"CallsBash":          action((*ncaAdapter).CallsBash),
	"Reply":              action((*ncaAdapter).Reply),
	"SteerLands":         action((*ncaAdapter).SteerLands),
	"SettleFires":        action((*ncaAdapter).SettleFires),
	"CallEnds":           action((*ncaAdapter).CallEnds),
	"KillLands":          action((*ncaAdapter).KillLands),
	"WakeRequest":        action((*ncaAdapter).WakeRequest),
	"ChildExits":         action((*ncaAdapter).ChildExits),
	"CatchUp":            action((*ncaAdapter).CatchUp),
}, "": {
	"end": func(any, []fmbt.Arg) (any, error) { return nil, nil },
}}

func ncaOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 10, "max-parallel-runs": 0}
}

// ncaHistory reads the abstract trace off a transcript. What leaves no
// record of its own is put where the record implies it: the bash call
// before the first thing that needs it, a steer's send beside its
// landing, a kill's send beside its finish, and CatchUp not at all (the
// state it checks leaves the page's fields out). A job finish stopped
// with no end row before it is the child's exit.
func ncaHistory(entries []history.Entry) []tracecheck.Step {
	st := map[string]any{"turn": "none", "call": "none", "job": "none", "wakes": 0}
	var steps []tracecheck.Step
	emit := func(action string, set map[string]any) {
		for k, v := range set {
			st[k] = v
		}
		q := map[string]any{}
		for k, v := range st {
			q["Session#0."+k] = v
		}
		name := action
		if action != "Init" {
			name = "Session#0." + action
		}
		steps = append(steps, tracecheck.Step{Action: name, State: q})
	}
	bashed, replyDue, exited, adoptedEnd := false, false, false, false
	wakes := 0
	bash := func() {
		if !bashed {
			bashed = true
			emit("CallsBash", map[string]any{"call": "fg"})
		}
	}
	for _, e := range entries {
		switch {
		case e.Kind == "input" && e.Data["steer"] == true:
			bash()
			// A steer is recorded when serve gets it, which can be after the
			// foreground call ended although it was sent before (the page's
			// send was still on its way): it was sent in the settle window.
			if n := len(steps); n > 0 && steps[n-1].Action == "Session#0.CallEnds" && st["call"] == "ended" {
				steps = steps[:n-1]
				emit("Steer", map[string]any{"call": "fg"})
				emit("CallEnds", map[string]any{"call": "ended"})
			} else {
				emit("Steer", nil)
			}
			emit("SteerLands", nil)
			replyDue = true
		case isWake(e):
			wakes++
			emit("WakeRequest", map[string]any{"turn": "wake", "wakes": wakes})
		case e.Kind == "input":
			switch {
			case len(steps) == 0:
				emit("Init", nil)
				emit("Prompt", map[string]any{"turn": "user"})
			case exited:
				exited = false
				emit("PromptAfterExit", map[string]any{"turn": "user"})
			default:
				emit("PromptWhileAdopted", map[string]any{"turn": "user"})
			}
		case e.Kind == "assistant" && replyDue && st["call"] == "fg":
			replyDue = false
			emit("Reply", nil)
		case e.Kind == "call" && e.Data["tool"] == "bash" && interrupted(e):
			// The lost call's end, part of PromptAfterExit.
		case e.Kind == "call" && e.Data["tool"] == "bash":
			bash()
			if e.Data["adopted"] == true {
				adoptedEnd = true
			} else {
				emit("CallEnds", map[string]any{"call": "ended"})
			}
		case typedJobEntry(e) && e.Data["event"] == "started":
			bash()
			emit("SettleFires", map[string]any{"turn": "none", "call": "adopted", "job": "running"})
		case typedJobEntry(e) && e.Data["event"] == "finished":
			switch {
			case e.Data["stopped"] != true:
				emit("CallEnds", map[string]any{"call": "ended", "job": "finished"})
			case adoptedEnd:
				emit("KillJob", nil)
				emit("KillLands", map[string]any{"call": "ended", "job": "stopped"})
			default:
				exited = true
				emit("ChildExits", map[string]any{"call": "lost", "job": "stopped"})
			}
		case e.Kind == "done":
			if r, _ := e.Data["running"].(float64); r > 0 {
				continue // the adopting close, SettleFires above
			}
			if st["turn"] != "none" {
				emit("Reply", map[string]any{"turn": "none"})
			}
		}
	}
	if len(steps) == 0 {
		emit("Init", nil)
	}
	return steps
}

func init() { historyProjections["native_call_adoption"] = ncaHistory }

// TestNativeCallAdoption lets fizzbee-mbt walk the spec at random; it
// is part of the exhaustive run (runMBT skips otherwise).
func TestNativeCallAdoption(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newNCAAdapter(t)
	if err := runMBT(t, "native_call_adoption", a, ncaActions(a), ncaOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("walk steps: %v", a.stats)
	checkNCAHistories(t, a)
}

// TestNativeCallAdoptionPaths walks the graph's walks step by step
// against a real serve, comparing the whole role after every step.
// Every walk runs, so one run reports every divergence.
func TestNativeCallAdoptionPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	b, err := pathsJSON("native_call_adoption")
	if err != nil {
		t.Fatal(err)
	}
	walks := ncaWalks(t, b)
	const shards = 4
	for sh := range shards {
		t.Run(fmt.Sprintf("shard%d", sh), func(t *testing.T) {
			t.Parallel()
			a := newNCAAdapter(t)
			acts := ncaActions(a)
			for i := sh; i < len(walks); i += shards {
				if err := a.walkTrace(acts, walks[i]); err != nil {
					t.Errorf("walk %d %s: %v", i, traceNames(walks[i]), err)
				}
			}
			t.Logf("shard %d: %v", sh, a.stats)
			checkNCAHistories(t, a)
		})
	}
}

func ncaWalks(t *testing.T, b []byte) [][]tracecheck.Step {
	t.Helper()
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		t.Fatal(err)
	}
	if len(doc.Paths) == 0 {
		t.Fatal("no walks over testdata/native_call_adoption")
	}
	var out [][]tracecheck.Step
	for _, p := range doc.Paths {
		out = append(out, p.Trace)
	}
	return out
}

func traceNames(tr []tracecheck.Step) string {
	var names []string
	for _, s := range tr[1:] {
		names = append(names, strings.TrimPrefix(s.Action, "Session#0."))
	}
	return "[" + strings.Join(names, " ") + "]"
}

// walkTrace runs one walk from Init, stopping at the first step that
// errs, is refused by the adapter's require, or leaves another state.
// A step the product took another branch of ends the walk unfailed.
func (a *ncaAdapter) walkTrace(acts map[string]map[string]fmbt.ActionFunc, trace []tracecheck.Step) (err error) {
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	for i, st := range trace {
		name := strings.TrimPrefix(st.Action, "Session#0.")
		if i == 0 {
			err = a.Init()
		} else {
			f := acts["Session"][name]
			if f == nil {
				f = acts[""][name]
			}
			if f == nil {
				return fmt.Errorf("step %d: no adapter action for %q", i, st.Action)
			}
			_, err = f(a, nil)
			if errors.Is(err, fmbt.ErrNotImplemented) {
				return nil
			}
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, name, err)
		}
		if a.gate.off {
			return fmt.Errorf("step %d (%s): the adapter refused a step the spec enables (its require disagrees)", i, name)
		}
		got, _ := a.GetState()
		want := roleState(st.State)
		var diff []string
		for k, v := range want {
			if fmt.Sprint(jsonRound(got[k])) != fmt.Sprint(jsonRound(v)) {
				diff = append(diff, fmt.Sprintf("%s: got %v, the spec says %v", k, got[k], v))
			}
		}
		if len(diff) > 0 {
			slices.Sort(diff)
			es, _ := a.entries()
			return fmt.Errorf("step %d (%s): %s\ntranscript %s", i, name, strings.Join(diff, "; "), ncaKinds(es))
		}
	}
	return nil
}

func checkNCAHistories(t *testing.T, a *ncaAdapter) {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("native_call_adoption")), "..", "testdata", "native_call_adoption"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), ncaHistory)
	}
	t.Logf("trace-checked %d sessions' histories", len(a.ids))
}

// The projection is only a check if a transcript the model forbids is
// refused: a second wake for the one adopted call.
func TestNativeCallAdoptionHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("native_call_adoption")), "..", "testdata", "native_call_adoption"))
	if err != nil {
		t.Fatal(err)
	}
	input := history.Entry{Kind: "input", Data: map[string]any{"text": "go"}}
	started := history.Entry{Kind: "job", Data: map[string]any{"id": 1.0, "event": "started", "call": "c"}}
	done1 := history.Entry{Kind: "done", Data: map[string]any{"running": 1.0}}
	end := history.Entry{Kind: "call", Data: map[string]any{"tool": "bash", "id": "c", "adopted": true}}
	fin := history.Entry{Kind: "job", Data: map[string]any{"id": 1.0, "event": "finished", "call": "c"}}
	wake := history.Entry{Kind: "input", Data: map[string]any{"wake": true}}
	done := history.Entry{Kind: "done"}
	stopped := history.Entry{Kind: "job", Data: map[string]any{"id": 1.0, "event": "finished", "call": "c", "stopped": true}}
	fgEnd := history.Entry{Kind: "call", Data: map[string]any{"tool": "bash", "id": "c"}}
	steer := history.Entry{Kind: "input", Data: map[string]any{"text": "also", "steer": true}}
	lost := history.Entry{Kind: "call", Data: map[string]any{"tool": "bash", "id": "c", "error": "interrupted: bough restarted before this call finished; it was not re-run"}}
	for name, es := range map[string][]history.Entry{
		"ends and wakes": {input, started, done1, end, fin, wake, done},
		"killed":         {input, started, done1, end, stopped, wake, done},
		"child exits":    {input, started, done1, stopped, input, lost, done},
		// A steer sent in the settle window that serve records only after
		// the call ended (the browser walk holds its POST until SteerLands).
		"steer lands after the end": {input, fgEnd, steer, done},
	} {
		if v := g.Check(ncaHistory(es)); v != nil {
			t.Errorf("%s: %v", name, v)
		}
	}
	if v := g.Check(ncaHistory([]history.Entry{input, started, done1, end, fin, wake, done, wake, done})); v == nil {
		t.Error("a second wake for the one adopted call passed the trace check")
	} else {
		t.Logf("refused as expected: %v", v)
	}
}

// A CallEnds that kills the adopted job instead of letting it exit
// records stopped where the spec says finished; the walks must say so.
func TestNativeCallAdoptionCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	b, err := pathsJSONCover("native_call_adoption", tracecheck.CoverTransitions)
	if err != nil {
		t.Fatal(err)
	}
	a := newNCAAdapter(t)
	a.endAsKill = true
	acts := ncaActions(a)
	for i, tr := range ncaWalks(t, b) {
		if !slices.ContainsFunc(tr, func(s tracecheck.Step) bool {
			return s.Action == "Session#0.CallEnds" && s.State["Session#0.job"] == "finished"
		}) {
			continue
		}
		if err := a.walkTrace(acts, tr); err != nil {
			t.Logf("caught as expected on walk %d: %v", i, err)
			return
		}
	}
	t.Fatal("walks whose CallEnds kills the job passed; the paths are not checking state")
}
