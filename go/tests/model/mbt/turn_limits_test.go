//go:build !windows

package mbt

import (
	"encoding/json"
	"errors"
	"fmt"
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

// specs/turn_limits.fizz against a real serve on the unreal engine: the
// step budget, call_timeout, a bash call's own timeout and the Stop
// hook's one continue. The loop row runs at max_steps 2 with a 3 s
// call_timeout; each model request takes one llm-control turn, held
// ("block") so the adapter decides what the model answers and when.
//
// The real coordinator asks the model again as soon as something is
// unread (a call's result, a nudge; at most the harness's 1 s grace for
// calls a response just started later), and the spec splits that into
// the unread-making step and a separate Request. The adapter queues the
// held turn that request will take BEFORE the step that makes it, and
// checks it was taken at Request. Where the real system cannot follow
// the walk, the walk goes on in the spec alone (detached, counted in
// the log) until the next Prompt, which starts a fresh session:
//
//   - another result landing between an unread-making step and Request:
//     the real request already went out without it;
//   - a timeout (call_timeout, or b's own) while the other call runs on
//     a deadline that passes no later: all are 3 s, so a call started
//     with it or before it ends first, or with it;
//   - ReplyNotMatchingSchema: stop-schema comes only from `--schema`,
//     and serve starts its children with no way to pass it;
//   - a call finishing or timing out while a budget stop cancels: the
//     real stop cancels every running call at once;
//   - a call that hits call_timeout before the walk's step for it (the
//     machine was slower than the 3 s deadline).
//
// Nothing can wake an idle session: heartbeats fire only while calls
// run, no call runs between the spec's turns, and a job notice opens a
// turn like a prompt, not through the Gate. So Wake changes nothing
// real (a lost wake is exactly that), and WakeOpens stands in a prompt
// for the wake: the woken turn is then driven like any other, with wake
// taken from the spec.
//
// A budget stop runs ahead: the Gate refuses the request as soon as the
// real coordinator makes it, and the cancel, the note and the done
// follow at once. The adapter reports the spec's in-between states
// until BudgetDone, where it checks the real close.
//
// status, steps (held turns taken), thinking (a taken turn not yet
// released), a, b (the calls' history rows), hooks (nudges), dones and
// why are read off the server; the rest is the spec's own record.

const (
	tlMaxSteps    = 2
	tlStopRetries = 1
	// tlCallTimeout is the loop row's call_timeout and the timeout the
	// model gives bash as b's own: short, because CallTimeout waits it
	// out, and long enough that no walk's steps outlast it.
	tlCallTimeout = 3 * time.Second
	tlHookText    = "keep going"
	tlHookMarker  = "HOOK-CONTINUE"
)

// tlState is the spec's Session role, field for field.
type tlState struct {
	status                  string
	steps                   int
	thinking, unread        bool
	a, b                    string
	own, cancelling, hooked bool
	hooks, tries, dones     int
	why                     string
	wake, waking, lost      bool
	aID, bID                string // the history projection's call ids; not spec state
}

func (s tlState) fields() map[string]any {
	return map[string]any{
		"status": s.status, "steps": s.steps, "thinking": s.thinking, "unread": s.unread,
		"a": s.a, "b": s.b, "own": s.own, "cancelling": s.cancelling, "hooked": s.hooked,
		"hooks": s.hooks, "tries": s.tries, "dones": s.dones, "why": s.why,
		"wake": s.wake, "waking": s.waking, "lost": s.lost,
	}
}

// pending is the state in which the real coordinator is already asking
// the model while the spec has yet to take Request.
func (s tlState) pending() bool {
	return s.status == "running" && !s.thinking && !s.cancelling && s.unread && !s.waking
}

// tlAction is one spec action: its require and its effect.
type tlAction struct {
	en func(tlState) bool
	do func(*tlState)
}

// tlSpec is specs/turn_limits.fizz in Go, action for action. The
// adapter's record and the history projection both step it; the graph
// check of every projected trace keeps it honest.
var tlSpec = map[string]tlAction{
	"Prompt": {
		en: func(s tlState) bool { return s.status != "running" },
		do: func(s *tlState) {
			*s = tlState{status: "running", steps: 1, thinking: true}
		},
	},
	"Wake": {
		en: func(s tlState) bool { return s.status != "running" && !s.thinking && !s.lost },
		do: func(s *tlState) {
			if s.steps >= tlMaxSteps {
				s.lost = true
			} else {
				s.steps++
				s.thinking, s.waking = true, true
			}
		},
	},
	"WakeOpens": {
		en: func(s tlState) bool { return s.waking },
		do: func(s *tlState) {
			*s = tlState{status: "running", steps: 1, thinking: s.thinking, wake: true}
		},
	},
	"ToolLoop": {
		en: func(s tlState) bool { return s.thinking && !s.waking && s.a != "running" },
		do: func(s *tlState) {
			s.thinking = false
			s.a = "running"
			if s.b != "running" {
				s.b, s.own = "running", false
			}
		},
	},
	"BashWithTimeout": {
		en: func(s tlState) bool { return s.thinking && !s.waking && s.b != "running" },
		do: func(s *tlState) { s.thinking, s.b, s.own = false, "running", true },
	},
	"FinishA": {
		en: func(s tlState) bool { return s.a == "running" },
		do: func(s *tlState) { s.a = "ok"; s.unread = s.unread || !s.cancelling },
	},
	"FinishB": {
		en: func(s tlState) bool { return s.b == "running" },
		do: func(s *tlState) { s.b = "ok"; s.unread = s.unread || !s.cancelling },
	},
	"CallTimeout": {
		en: func(s tlState) bool { return s.a == "running" },
		do: func(s *tlState) { s.a = "timeout"; s.unread = s.unread || !s.cancelling },
	},
	"CallTimeoutB": {
		en: func(s tlState) bool { return s.b == "running" && !s.own },
		do: func(s *tlState) { s.b = "timeout"; s.unread = s.unread || !s.cancelling },
	},
	"BashLimit": {
		en: func(s tlState) bool { return s.b == "running" && s.own },
		do: func(s *tlState) { s.b = "limit"; s.unread = s.unread || !s.cancelling },
	},
	"Request": {
		en: func(s tlState) bool {
			return s.status == "running" && !s.thinking && !s.cancelling && s.unread
		},
		do: func(s *tlState) {
			s.unread = false
			if s.steps >= tlMaxSteps {
				s.cancelling = true
			} else {
				s.steps++
				s.thinking = true
			}
		},
	},
	"CancelA": {
		en: func(s tlState) bool { return s.cancelling && s.a == "running" },
		do: func(s *tlState) { s.a = "cancelled" },
	},
	"CancelB": {
		en: func(s tlState) bool { return s.cancelling && s.b == "running" },
		do: func(s *tlState) { s.b = "cancelled" },
	},
	"BudgetDone": {
		en: func(s tlState) bool { return s.cancelling && s.a != "running" && s.b != "running" },
		do: func(s *tlState) {
			s.cancelling, s.unread = false, false
			s.status, s.why = "done", "max_steps"
			s.dones++
		},
	},
	"Reply": {
		en: func(s tlState) bool { return s.thinking && !s.waking },
		do: func(s *tlState) {
			s.thinking = false
			if s.a != "running" && s.b != "running" && !s.unread {
				s.status = "done"
				s.dones++
			}
		},
	},
	"StopHookContinues": {
		en: func(s tlState) bool {
			return s.thinking && !s.waking && !s.hooked && s.a != "running" && s.b != "running" && !s.unread
		},
		do: func(s *tlState) {
			s.thinking, s.hooked, s.unread = false, true, true
			s.hooks++
		},
	},
	"ReplyNotMatchingSchema": {
		en: func(s tlState) bool {
			return s.thinking && !s.waking && s.a != "running" && s.b != "running" && !s.unread
		},
		do: func(s *tlState) {
			s.thinking = false
			if s.tries < tlStopRetries {
				s.tries++
				s.unread = true
			} else {
				s.status, s.why = "error", "schema"
				s.dones++
			}
		},
	},
}

// tlConfig is the loop row the flow runs: max_steps 2, one schema
// retry, a call_timeout short enough to fire, and a turn_settle long
// enough that no call is ever adopted as a job mid-walk.
var tlConfig = controlConfig + fmt.Sprintf(`- id: loop
  plugin: engine-unreal
  config:
    max_steps: %d
    stop_retries: %d
    call_timeout: %s
    turn_settle: 10m
`, tlMaxSteps, tlStopRetries, tlCallTimeout)

// tlStopHook continues a turn whose closing reply carries the marker,
// once: the engine asks the Stop hook only while stopUsed is false.
const tlStopHook = `if (String(event.reply || "").indexOf("` + tlHookMarker + `") >= 0) return {block: "` + tlHookText + `"};`

type tlAdapter struct {
	t     *testing.T
	s     *servetest.Server
	dir   string // llm-control's queue
	gates string // a call runs until its gate file appears
	gate  gate

	sh  tlState
	id  string
	ids []string
	n   int

	attached bool
	ahead    bool   // a budget stop the real system has run ahead of the spec
	held     string // the taken, unreleased turn: the real "thinking"
	pend     string // queued for the request the coordinator makes on its own
	names    []string
	first    int // the first turn number of the current bough turn
	since    int64
	calls    []string
	callA    string
	callB    string
	startA   time.Time
	startB   time.Time

	stats map[string]int

	// replyHooks is TestTurnLimitsCatchesWrongAdapter's bug: every
	// reply carries the Stop hook's marker, so a reply that should
	// close the turn is continued instead.
	replyHooks bool
}

func newTLAdapter(t *testing.T) *tlAdapter {
	s := servetest.Start(t, servetest.Options{Config: tlConfig, Files: map[string]string{
		".bough/hooks/stop/continue.js": tlStopHook,
	}})
	return &tlAdapter{t: t, s: s, dir: control.Dir(s.Home), gates: t.TempDir(), stats: map[string]int{}}
}

func (a *tlAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

// Init is a fresh idle session.
func (a *tlAdapter) Init() error {
	a.sh = tlState{status: "idle"}
	a.gate.reset()
	return a.fresh()
}

// fresh starts the walk (or its re-attached rest) on a new session.
func (a *tlAdapter) fresh() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	a.attached, a.ahead = true, false
	a.held, a.pend, a.names, a.calls, a.first, a.since = "", "", nil, nil, 0, 0
	a.callA, a.callB = "", ""
	return nil
}

// Cleanup lets go of everything the walk left running and archives its
// session, so nothing of it can take a later walk's queued turn.
func (a *tlAdapter) Cleanup() error {
	return a.abandon()
}

// abandon ends the real session: every gate opens, held turns reply,
// queued ones are withdrawn, and once the turn has closed the session
// is archived (its child killed).
func (a *tlAdapter) abandon() error {
	if a.id == "" {
		return nil
	}
	for _, c := range a.calls {
		os.WriteFile(filepath.Join(a.gates, c), nil, 0o644)
	}
	for _, n := range a.names {
		os.Remove(filepath.Join(a.dir, n+".json"))
		if exists(filepath.Join(a.dir, n+".taken")) && !exists(filepath.Join(a.dir, n+".release")) {
			if err := a.release(n, control.Turn{Text: "abandoned " + n}); err != nil {
				return err
			}
		}
	}
	// It may go on asking the model (no turn is queued, so each answer
	// is llm-control's own text) until it closes; a turn that never
	// does is still archived.
	a.waitLines("the abandoned turn to close", 10*time.Second, func(r serve.Row, _ []serve.Line) bool {
		return r.Status != serve.StatusRunning
	})
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Archive(ctx, a.id)
	a.id = ""
	return err
}

func exists(p string) bool { _, err := os.Stat(p); return err == nil }

func (a *tlAdapter) name() string {
	a.n++
	n := fmt.Sprintf("t%06d", a.n)
	a.names = append(a.names, n)
	return n
}

func (a *tlAdapter) queue(n string, turn control.Turn) error {
	b, err := json.Marshal(turn)
	if err != nil {
		return err
	}
	tmp := filepath.Join(a.dir, n+".tmp")
	if err := os.MkdirAll(a.dir, 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(tmp, b, 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, filepath.Join(a.dir, n+".json"))
}

func (a *tlAdapter) release(n string, turn control.Turn) error {
	b, err := json.Marshal(turn)
	if err != nil {
		return err
	}
	tmp := filepath.Join(a.dir, n+".release-tmp")
	if err := os.WriteFile(tmp, b, 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, filepath.Join(a.dir, n+".release"))
}

func (a *tlAdapter) waitTaken(n string) error {
	deadline := time.Now().Add(actionTimeout)
	for !exists(filepath.Join(a.dir, n+".taken")) {
		if time.Now().After(deadline) {
			_, lines, _ := a.lines()
			return fmt.Errorf("turn %s not taken after %s; transcript %v", n, actionTimeout, lineKinds(lines))
		}
		time.Sleep(10 * time.Millisecond)
	}
	return nil
}

func (a *tlAdapter) lines() (serve.Row, []serve.Line, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	return a.s.GetSession(ctx, a.id)
}

// waitLines polls the row and transcript until ok holds, for at most d.
func (a *tlAdapter) waitLines(what string, d time.Duration, ok func(serve.Row, []serve.Line) bool) ([]serve.Line, error) {
	deadline := time.Now().Add(d)
	for {
		row, lines, err := a.lines()
		if err == nil && ok(row, lines) {
			return lines, nil
		}
		if time.Now().After(deadline) {
			return lines, fmt.Errorf("waiting %s for %s: row %s, transcript %v (err %v)", d, what, row.Status, lineKinds(lines), err)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// callOutcome is a call's history row as the spec names it: "running"
// while it has none.
func callOutcome(lines []serve.Line, id string) string {
	if id == "" {
		return ""
	}
	for _, l := range lines {
		if l.Kind != "call" || l.Data["id"] != id {
			continue
		}
		return outcomeOf(l.Data)
	}
	return "running"
}

func outcomeOf(d map[string]any) string {
	errText, _ := d["error"].(string)
	switch {
	case d["canceled"] == true:
		return "cancelled"
	case strings.Contains(errText, "timed out after"):
		return "timeout"
	case strings.Contains(errText, "killed after"):
		return "limit"
	case errText == "":
		return "ok"
	}
	return "error: " + errText
}

func (a *tlAdapter) detach(why string) {
	if a.attached {
		a.attached = false
		a.stats["detached: "+why]++
	}
}

// GetState reads what the server shows and the spec's record for the
// rest (see the file comment).
func (a *tlAdapter) GetState() (map[string]any, error) {
	st := a.sh.fields()
	if !a.attached || a.ahead {
		return st, nil
	}
	row, lines, err := a.lines()
	if err != nil {
		return nil, err
	}
	ra, rb := callOutcome(lines, a.callA), callOutcome(lines, a.callB)
	// A call the slow machine let reach call_timeout before the walk's
	// step for it: the real system took a path the walk did not.
	if (a.sh.a == "running" && ra == "timeout" && time.Since(a.startA) >= tlCallTimeout) ||
		(a.sh.b == "running" && (rb == "timeout" || rb == "limit") && time.Since(a.startB) >= tlCallTimeout) {
		a.detach("a call reached its deadline before its step")
		return st, nil
	}
	st["status"] = string(row.Status)
	if a.sh.a != "" {
		st["a"] = ra
	}
	if a.sh.b != "" {
		st["b"] = rb
	}
	taken, open := 0, false
	for _, n := range a.names[a.first:] {
		if n == a.pend || !exists(filepath.Join(a.dir, n+".taken")) {
			continue
		}
		taken++
		if !exists(filepath.Join(a.dir, n+".release")) {
			open = true
		}
	}
	if !a.sh.waking {
		// While a wake is on its way (spec only) the request is not real.
		st["steps"], st["thinking"] = taken, open
	}
	dones, hooks, why := 0, 0, ""
	for _, l := range lines {
		if l.Seq <= a.since {
			continue
		}
		switch {
		case l.Kind == "done":
			dones++
		case l.Kind == "nudge" && l.Text == tlHookText:
			hooks++
		case l.Kind == "system" && strings.HasPrefix(l.Text, "step budget reached"):
			why = "max_steps"
		}
	}
	st["dones"], st["hooks"], st["why"] = dones, hooks, why
	return st, nil
}

// step runs one spec action: the gate, the real side (while attached),
// then the spec's record.
func (a *tlAdapter) step(name string, real func(before, after tlState) error) error {
	act := tlSpec[name]
	if !a.gate.pass(act.en(a.sh)) {
		return nil
	}
	before, after := a.sh, a.sh
	act.do(&after)
	if name == "Prompt" && !a.attached {
		if err := a.abandon(); err != nil {
			return err
		}
		if err := a.fresh(); err != nil {
			return err
		}
	}
	if a.attached {
		switch {
		case a.ahead && name != "Request" && name != "CancelA" && name != "CancelB" && name != "BudgetDone":
			a.detach("a call ended while the budget stop cancelled")
		case before.pending() && name != "Request":
			a.detach("a result landed after the real request went out")
		}
	}
	if a.attached && real != nil {
		// The real coordinator asks the model the moment the step
		// leaves something unread: queue what that request takes first.
		if !before.pending() && after.pending() {
			if after.steps < tlMaxSteps {
				a.pend = a.name()
				if err := a.queue(a.pend, control.Turn{Mode: "block"}); err != nil {
					return err
				}
			} else {
				a.ahead = true
			}
		}
		if err := real(before, after); err != nil {
			return fmt.Errorf("%s: %w", name, err)
		}
	}
	if a.attached {
		a.stats["attached "+name]++
	} else {
		a.stats["spec only "+name]++
	}
	a.sh = after
	return nil
}

func (a *tlAdapter) Prompt() error {
	return a.step("Prompt", func(_, _ tlState) error { return a.prompt() })
}

// prompt opens a real turn and holds its first request.
func (a *tlAdapter) prompt() error {
	_, lines, err := a.lines()
	if err != nil {
		return err
	}
	a.since = lastSeq(lines)
	a.first = len(a.names)
	a.callA, a.callB = "", ""
	n := a.name()
	if err := a.queue(n, control.Turn{Mode: "block"}); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, "turn "+n); err != nil {
		return err
	}
	if err := a.waitTaken(n); err != nil {
		return err
	}
	a.held = n
	_, err = a.waitLines("the prompt to open a turn", actionTimeout, func(r serve.Row, l []serve.Line) bool {
		return r.Status == serve.StatusRunning && inputAfter(l, a.since)
	})
	return err
}

// gateCmd runs until the gate file for id appears (bounded, so a gate
// nobody opens cannot outlive the test).
func (a *tlAdapter) gateCmd(id string) string {
	return fmt.Sprintf("n=0; while [ ! -e '%s' ] && [ $n -lt 1500 ]; do sleep 0.02; n=$((n+1)); done", filepath.Join(a.gates, id))
}

// answer releases the held turn as the model's response.
func (a *tlAdapter) answer(turn control.Turn) error {
	if a.held == "" {
		return errors.New("no model request is held")
	}
	n := a.held
	a.held = ""
	return a.release(n, turn)
}

func (a *tlAdapter) ToolLoop() error {
	return a.step("ToolLoop", func(before, _ tlState) error {
		n, now := a.held, time.Now()
		a.callA, a.startA = n+"-a", now
		a.calls = append(a.calls, a.callA)
		calls := []control.Call{{ID: a.callA, Name: "bash", Args: map[string]any{"command": a.gateCmd(a.callA)}}}
		if before.b != "running" {
			a.callB, a.startB = n+"-b", now
			a.calls = append(a.calls, a.callB)
			calls = append(calls, control.Call{ID: a.callB, Name: "bash", Args: map[string]any{"command": a.gateCmd(a.callB)}})
		}
		return a.answer(control.Turn{Calls: calls})
	})
}

func (a *tlAdapter) BashWithTimeout() error {
	return a.step("BashWithTimeout", func(_, _ tlState) error {
		a.callB, a.startB = a.held+"-B", time.Now()
		a.calls = append(a.calls, a.callB)
		return a.answer(control.Turn{Calls: []control.Call{{ID: a.callB, Name: "bash", Args: map[string]any{
			"command": a.gateCmd(a.callB), "timeout": tlCallTimeout.Seconds(),
		}}}})
	})
}

// finish opens a call's gate and waits for its row.
func (a *tlAdapter) finish(id string) error {
	if err := os.WriteFile(filepath.Join(a.gates, id), nil, 0o644); err != nil {
		return err
	}
	lines, err := a.waitLines("call "+id+" to report", actionTimeout, func(_ serve.Row, l []serve.Line) bool {
		return callOutcome(l, id) != "running"
	})
	if err != nil {
		return err
	}
	if got := callOutcome(lines, id); got == "timeout" || got == "limit" {
		a.detach("a call reached its deadline before its step")
	}
	return nil
}

// expire waits out a call's deadline.
func (a *tlAdapter) expire(id string) error {
	_, err := a.waitLines("call "+id+" to time out", tlCallTimeout+actionTimeout, func(_ serve.Row, l []serve.Line) bool {
		return callOutcome(l, id) != "running"
	})
	return err
}

func (a *tlAdapter) FinishA() error {
	return a.step("FinishA", func(_, _ tlState) error { return a.finish(a.callA) })
}

func (a *tlAdapter) FinishB() error {
	return a.step("FinishB", func(_, _ tlState) error { return a.finish(a.callB) })
}

// expireFirst waits out a call's deadline, unless the other call is
// still running on a deadline that passes no later (every call has one:
// call_timeout, or b's own timeout of the same length): then the other
// ends first, or with it, and the real system leaves the walk.
func (a *tlAdapter) expireFirst(id string, start time.Time, other bool, otherStart time.Time) error {
	if other && !otherStart.After(start) {
		a.detach("another call's deadline passes first")
		return nil
	}
	return a.expire(id)
}

func (a *tlAdapter) CallTimeout() error {
	return a.step("CallTimeout", func(before, _ tlState) error {
		return a.expireFirst(a.callA, a.startA, before.b == "running", a.startB)
	})
}

func (a *tlAdapter) CallTimeoutB() error {
	return a.step("CallTimeoutB", func(before, _ tlState) error {
		return a.expireFirst(a.callB, a.startB, before.a == "running", a.startA)
	})
}

func (a *tlAdapter) BashLimit() error {
	return a.step("BashLimit", func(before, _ tlState) error {
		return a.expireFirst(a.callB, a.startB, before.a == "running", a.startA)
	})
}

func (a *tlAdapter) Request() error {
	return a.step("Request", func(before, _ tlState) error {
		if a.ahead {
			return nil // the Gate refused it already; BudgetDone checks the close
		}
		n := a.pend
		a.pend = ""
		if err := a.waitTaken(n); err != nil {
			return err
		}
		a.held = n
		return nil
	})
}

func (a *tlAdapter) CancelA() error { return a.step("CancelA", nil) }
func (a *tlAdapter) CancelB() error { return a.step("CancelB", nil) }

func (a *tlAdapter) BudgetDone() error {
	return a.step("BudgetDone", func(_, _ tlState) error {
		_, err := a.waitLines("the budget stop to close the turn", actionTimeout, func(r serve.Row, l []serve.Line) bool {
			for _, x := range l {
				if x.Seq > a.since && x.Kind == "done" && x.Data["stop"] == "max_steps" {
					return r.Status != serve.StatusRunning
				}
			}
			return false
		})
		a.ahead = false
		return err
	})
}

func (a *tlAdapter) Reply() error {
	return a.step("Reply", func(_, after tlState) error {
		text := "reply " + a.held
		if a.replyHooks {
			text = tlHookMarker + " " + text
		}
		if err := a.answer(control.Turn{Text: text}); err != nil {
			return err
		}
		if after.status == "done" {
			_, err := a.waitLines("the reply to close the turn", actionTimeout, func(r serve.Row, _ []serve.Line) bool {
				return r.Status != serve.StatusRunning
			})
			return err
		}
		return nil
	})
}

func (a *tlAdapter) StopHookContinues() error {
	return a.step("StopHookContinues", func(_, after tlState) error {
		if err := a.answer(control.Turn{Text: tlHookMarker + " " + a.held}); err != nil {
			return err
		}
		_, err := a.waitLines("the stop hook's nudge", actionTimeout, func(_ serve.Row, l []serve.Line) bool {
			n := 0
			for _, x := range l {
				if x.Seq > a.since && x.Kind == "nudge" {
					n++
				}
			}
			return n >= after.hooks
		})
		return err
	})
}

func (a *tlAdapter) ReplyNotMatchingSchema() error {
	return a.step("ReplyNotMatchingSchema", func(_, _ tlState) error {
		a.detach("serve cannot pass --schema")
		return nil
	})
}

// Wake has nothing real to do: nothing can wake an idle session (see
// the file comment). A lost wake leaves the session as it was, which
// GetState keeps reading; a wake that goes out is WakeOpens's.
func (a *tlAdapter) Wake() error { return a.step("Wake", nil) }

// WakeOpens stands in a prompt for the wake: after openWake the turn
// is a prompt's (budgets reset, its first request counted, the same
// Gate, calls and closes), so the walk goes on against the server. What
// this cannot check is the wake itself: that it opens a turn at all,
// and wake on its done (reported from the spec).
func (a *tlAdapter) WakeOpens() error {
	return a.step("WakeOpens", func(_, _ tlState) error { return a.prompt() })
}

var tlActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Prompt":                 action((*tlAdapter).Prompt),
	"Wake":                   action((*tlAdapter).Wake),
	"WakeOpens":              action((*tlAdapter).WakeOpens),
	"ToolLoop":               action((*tlAdapter).ToolLoop),
	"BashWithTimeout":        action((*tlAdapter).BashWithTimeout),
	"FinishA":                action((*tlAdapter).FinishA),
	"FinishB":                action((*tlAdapter).FinishB),
	"CallTimeout":            action((*tlAdapter).CallTimeout),
	"CallTimeoutB":           action((*tlAdapter).CallTimeoutB),
	"BashLimit":              action((*tlAdapter).BashLimit),
	"Request":                action((*tlAdapter).Request),
	"CancelA":                action((*tlAdapter).CancelA),
	"CancelB":                action((*tlAdapter).CancelB),
	"BudgetDone":             action((*tlAdapter).BudgetDone),
	"Reply":                  action((*tlAdapter).Reply),
	"StopHookContinues":      action((*tlAdapter).StopHookContinues),
	"ReplyNotMatchingSchema": action((*tlAdapter).ReplyNotMatchingSchema),
}}

// walkTLPaths drives every walk over the checked-in graph through the
// adapter, comparing its state with the walk's after every step. A walk
// stops at its first mismatch; the rest still run, unless first.
func walkTLPaths(t *testing.T, a *tlAdapter, cover tracecheck.Cover, first bool) (mismatches []string) {
	t.Helper()
	b, err := pathsJSONCover("turn_limits", cover)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		t.Fatal(err)
	}
	for pi, p := range doc.Paths {
		var names []string
		for si, st := range p.Trace {
			name := strings.TrimPrefix(st.Action, "Session#0.")
			names = append(names, name)
			if si == 0 {
				err = a.Init()
			} else {
				f, ok := tlActions["Session"][name]
				if !ok {
					t.Fatalf("path %d: no adapter action %q", pi, name)
				}
				_, err = f(a, nil)
			}
			if err == nil {
				err = a.compare(st.State)
			}
			if err != nil {
				mismatches = append(mismatches, fmt.Sprintf("path %d step %d (%s): %v", pi, si, strings.Join(names, " "), err))
				break
			}
		}
		if err := a.Cleanup(); err != nil {
			t.Fatalf("path %d: cleanup: %v", pi, err)
		}
		if first && len(mismatches) > 0 {
			break
		}
	}
	t.Logf("turn_limits: %d walks; steps %v", len(doc.Paths), a.stats)
	return mismatches
}

func (a *tlAdapter) compare(want map[string]any) error {
	got, err := a.GetState()
	if err != nil {
		return err
	}
	var diff []string
	for k, v := range want {
		f, ok := strings.CutPrefix(k, "Session#0.")
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

// ---- history projection ----

// tlEvent is one thing a transcript shows about the spec's actions.
type tlEvent struct {
	kind    string // prompt | wake | call | reply | hooked | budget | done | other
	call    string // a call's id
	resp    string // the response that made it: its id less the slot
	slot    string // a | b | B (b with its own timeout)
	outcome string
	text    string
}

// tlEvents reads the events off a transcript. The calls' ids name the
// response that made them and their slot (the adapter's convention,
// <turn>-a, -b, -B): history records a call only when it ends, and the
// response that started it nowhere else.
func tlEvents(entries []history.Entry) (evs []tlEvent, order []string, resps map[string][]string) {
	resps = map[string][]string{}
scan:
	for i, e := range entries {
		text, _ := e.Data["text"].(string)
		switch e.Kind {
		case "input":
			if e.Data["steer"] != nil {
				evs = append(evs, tlEvent{kind: "other", text: "a steer"})
				continue
			}
			if e.Data["wake"] == true {
				evs = append(evs, tlEvent{kind: "wake"})
				continue
			}
			evs = append(evs, tlEvent{kind: "prompt"})
		case "call":
			id, _ := e.Data["id"].(string)
			k := strings.LastIndex(id, "-")
			if k < 0 {
				evs = append(evs, tlEvent{kind: "other", text: "call " + id})
				continue
			}
			ev := tlEvent{kind: "call", call: id, resp: id[:k], slot: id[k+1:], outcome: outcomeOf(e.Data)}
			evs = append(evs, ev)
		case "assistant":
			// A reply the Stop hook answered with a continue: its nudge
			// follows (after the hook's own record).
			kind := "reply"
			for _, f := range entries[i+1:] {
				if f.Kind == "hook" {
					continue
				}
				if f.Kind == "nudge" {
					kind = "hooked"
				}
				break
			}
			evs = append(evs, tlEvent{kind: kind, text: text})
		case "system":
			if strings.HasPrefix(text, "step budget reached") {
				evs = append(evs, tlEvent{kind: "budget"})
			}
		case "done":
			if e.Data["stop"] == nil {
				evs = append(evs, tlEvent{kind: "done"})
			}
		case "cancelled":
			// A user's stop is stop_interrupt's, not this spec's: here it
			// is the walk's cleanup archiving a session mid-turn. The
			// trace ends before it, and before the calls it cancelled.
			for len(evs) > 0 && evs[len(evs)-1].kind == "call" && evs[len(evs)-1].outcome == "cancelled" {
				evs = evs[:len(evs)-1]
			}
			break scan
		case "error":
			evs = append(evs, tlEvent{kind: "other", text: e.Kind + " " + text})
		}
	}
	for _, ev := range evs {
		if ev.kind != "call" {
			continue
		}
		if _, ok := resps[ev.resp]; !ok {
			order = append(order, ev.resp)
		}
		if !slicesContains(resps[ev.resp], ev.slot) {
			resps[ev.resp] = append(resps[ev.resp], ev.slot)
		}
	}
	return evs, order, resps
}

func slicesContains(s []string, v string) bool {
	for _, x := range s {
		if x == v {
			return true
		}
	}
	return false
}

// tlHistory projects a transcript to a path. The transcript shows the
// calls ending, the replies, the nudges and the closes. It never shows
// a request, nor the response that started a call: history writes a
// call's row when it ends. The real coordinator asks as soon as
// something is unread, but whether a result that landed just then was
// in that request is not written down, and a response may start its
// calls before an older call's row lands. So the projection searches:
// it steps the Go spec through the events, and at each point may also
// take a Request, or start the next response, before the event. It
// returns the first placement every observation agrees with, which the
// graph check then replays; with none, the longest agreeing prefix and
// the event that broke it, which the graph check reports.
func tlHistory(entries []history.Entry) []tracecheck.Step {
	evs, order, resps := tlEvents(entries)
	type key struct {
		i, next int
		s       tlState
	}
	dead := map[key]bool{}
	var best []string
	bestAt := -1
	var solve func(i, next int, s tlState, path []string) []string
	solve = func(i, next int, s tlState, path []string) []string {
		if i == len(evs) {
			return path
		}
		k := key{i, next, s}
		if dead[k] {
			return nil
		}
		if i > bestAt {
			bestAt, best = i, append([]string(nil), path...)
		}
		more := func(acts ...string) []string { return append(path[:len(path):len(path)], acts...) }
		ev := evs[i]
		if ev.kind == "call" && ev.call != s.aID && ev.call != s.bID {
			// Its response, if it is the next one, is started below.
		} else if acts, n, ok := tlObserve(ev, s); ok {
			if got := solve(i+1, next, n, more(acts...)); got != nil {
				return got
			}
		}
		if next < len(order) {
			if act, n, ok := tlStart(s, order[next], resps[order[next]]); ok {
				if got := solve(i, next+1, n, more(act)); got != nil {
					return got
				}
			}
		}
		if req := tlSpec["Request"]; req.en(s) {
			n := s
			req.do(&n)
			if got := solve(i, next, n, more("Request")); got != nil {
				return got
			}
		}
		dead[k] = true
		return nil
	}
	path := solve(0, 0, tlState{status: "idle"}, []string{})
	if path == nil {
		path = append(best, "no path explains "+fmt.Sprintf("%+v", evs[bestAt]))
	}
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Session#0.status": "idle"}}}
	s := tlState{status: "idle"}
	for _, name := range path {
		st := map[string]any{}
		if act, ok := tlSpec[name]; ok && act.en(s) {
			act.do(&s)
			for f, v := range s.fields() {
				st["Session#0."+f] = v
			}
		}
		steps = append(steps, tracecheck.Step{Action: "Session#0." + name, State: st})
	}
	return steps
}

// tlStart is the response resp starting its calls (slots): ToolLoop
// when it made an a call, BashWithTimeout otherwise.
func tlStart(s tlState, resp string, slots []string) (string, tlState, bool) {
	name := "BashWithTimeout"
	if slicesContains(slots, "a") {
		name = "ToolLoop"
	}
	act := tlSpec[name]
	if !act.en(s) {
		return "", s, false
	}
	startsB := s.b != "running"
	act.do(&s)
	switch {
	case name == "BashWithTimeout":
		s.bID = resp + "-B"
	case startsB:
		s.aID, s.bID = resp+"-a", resp+"-b"
	default:
		s.aID = resp + "-a"
	}
	return name, s, true
}

// tlObserve is the actions one event is, from s; ok is false when the
// event cannot happen there. A call's event is its end: its response
// has started.
func tlObserve(ev tlEvent, s tlState) (acts []string, next tlState, ok bool) {
	next = s
	do := func(name string) bool {
		act := tlSpec[name]
		if !act.en(next) {
			return false
		}
		act.do(&next)
		acts = append(acts, name)
		return true
	}
	switch ev.kind {
	case "prompt":
		return acts, next, do("Prompt")
	case "wake":
		return acts, next, do("Wake") && next.waking && do("WakeOpens")
	case "reply":
		return acts, next, do("Reply")
	case "hooked":
		return acts, next, do("StopHookContinues")
	case "budget":
		return acts, next, do("BudgetDone")
	case "done":
		return acts, next, next.status != "running"
	case "call":
		slot := "b"
		if ev.call == next.aID {
			slot = "a"
		}
		var end string
		switch ev.outcome {
		case "ok":
			end = map[string]string{"a": "FinishA", "b": "FinishB"}[slot]
		case "timeout":
			end = map[string]string{"a": "CallTimeout", "b": "CallTimeoutB"}[slot]
		case "limit":
			end = "BashLimit"
		case "cancelled":
			end = map[string]string{"a": "CancelA", "b": "CancelB"}[slot]
		default:
			return nil, s, false
		}
		return acts, next, do(end)
	}
	return nil, s, false
}

func init() { historyProjections["turn_limits"] = tlHistory }

func checkTLHistories(t *testing.T, a *tlAdapter) {
	t.Helper()
	g, err := tracecheck.Load(fizzCheck(t, "turn_limits"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		entries := sessionHistory(t, a.s.Home, id)
		if v := g.Check(tlHistory(entries)); v != nil {
			var seen []string
			for _, e := range entries {
				if e.Kind == "hook" || e.Kind == "meta" || e.Kind == "engine" {
					continue
				}
				w := e.Kind
				if e.Kind == "call" {
					w += " " + fmt.Sprint(e.Data["id"]) + " " + outcomeOf(e.Data)
				}
				seen = append(seen, w)
			}
			t.Errorf("history trace of %s is not a path in the model: %v\ntranscript: %s", id, v, strings.Join(seen, ", "))
		}
	}
}

func TestTurnLimitsPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newTLAdapter(t)
	for _, m := range walkTLPaths(t, a, envCover(), false) {
		t.Error(m)
	}
	checkTLHistories(t, a)
}

// TestTurnLimits is the runner's random walks (the exhaustive run only).
func TestTurnLimits(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newTLAdapter(t)
	err := runMBT(t, "turn_limits", a, tlActions, map[string]any{"max-seq-runs": 200, "max-actions": 12, "max-parallel-runs": 0})
	t.Logf("turn_limits: runner steps %v", a.stats)
	if err != nil {
		t.Errorf("model-based run: %v", err)
	}
	checkTLHistories(t, a)
}

// A reply that always carries the Stop hook's marker must fail the
// walks: the hook continues a turn the spec says the reply closed.
func TestTurnLimitsCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newTLAdapter(t)
	a.replyHooks = true
	ms := walkTLPaths(t, a, envCover(), true)
	if len(ms) == 0 {
		t.Fatal("walks whose every reply asks the Stop hook to continue passed; the adapter is not checking state")
	}
	t.Logf("caught: %s", ms[0])
}

// The Go spec the adapter's record and the history projection step must
// be the fizz spec: every action link of the checked-in graph, applied
// in Go to its source state, lands on its destination state, and Go
// enables nothing the graph does not. Offline; no serve.
func TestTurnLimitsGoSpecIsTheGraph(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("turn_limits")), "..", "testdata", "turn_limits"))
	if err != nil {
		t.Fatal(err)
	}
	of := func(st map[string]any) tlState {
		f := func(k string) any { return st["Session#0."+k] }
		n := func(k string) int { v, _ := f(k).(float64); return int(v) }
		b := func(k string) bool { v, _ := f(k).(bool); return v }
		s := func(k string) string { v, _ := f(k).(string); return v }
		return tlState{status: s("status"), steps: n("steps"), thinking: b("thinking"), unread: b("unread"),
			a: s("a"), b: s("b"), own: b("own"), cancelling: b("cancelling"), hooked: b("hooked"),
			hooks: n("hooks"), tries: n("tries"), dones: n("dones"), why: s("why"),
			wake: b("wake"), waking: b("waking"), lost: b("lost")}
	}
	enabled := map[int]map[string]bool{}
	links := 0
	for _, l := range g.Links {
		if l.Type != "action" || g.Nodes[l.Src].Name != "yield" {
			continue
		}
		name := strings.TrimPrefix(l.Name, "Session#0.")
		if enabled[l.Src] == nil {
			enabled[l.Src] = map[string]bool{}
		}
		enabled[l.Src][name] = true
		act, ok := tlSpec[name]
		if !ok {
			t.Fatalf("no Go action %q", name)
		}
		src, dest := of(g.Nodes[l.Src].State), of(g.Nodes[l.Dest].State)
		if !act.en(src) {
			t.Errorf("%s: enabled in the graph, not in Go, from %+v", name, src)
			continue
		}
		act.do(&src)
		if src != dest {
			t.Errorf("%s: Go reaches %+v, the graph %+v", name, src, dest)
		}
		links++
	}
	for _, n := range g.Nodes {
		if n.Name != "yield" {
			continue
		}
		for name, act := range tlSpec {
			if act.en(of(n.State)) && !enabled[n.Index][name] {
				t.Errorf("%s: enabled in Go, not in the graph, from %+v", name, of(n.State))
			}
		}
	}
	if links == 0 {
		t.Fatal("no action links in the graph")
	}
}

// tlTranscript builds history entries from words: prompt, reply, hooked
// (a reply the Stop hook continued), budget, done, and <call id>:<ok |
// timeout | limit | cancelled>.
func tlTranscript(words string) []history.Entry {
	var es []history.Entry
	add := func(kind string, data map[string]any) { es = append(es, history.Entry{Kind: kind, Data: data}) }
	for _, w := range strings.Fields(words) {
		switch w {
		case "prompt":
			add("input", map[string]any{"text": "go"})
		case "reply":
			add("assistant", map[string]any{"text": "reply"})
		case "hooked":
			add("assistant", map[string]any{"text": tlHookMarker})
			add("hook", map[string]any{"event": "stop"})
			add("nudge", map[string]any{"text": tlHookText})
		case "budget":
			add("system", map[string]any{"text": "step budget reached (max_steps 2); stopped"})
			add("done", map[string]any{"stop": "max_steps"})
		case "done":
			add("done", map[string]any{})
		default:
			id, out, _ := strings.Cut(w, ":")
			d := map[string]any{"id": id, "exit": 0}
			switch out {
			case "timeout":
				d["error"] = "bash: timed out after 3s"
			case "limit":
				d["error"] = "bash: killed after 3s"
			case "cancelled":
				d["canceled"], d["error"] = true, "cancelled by the user"
			}
			add("call", d)
		}
	}
	return es
}

// The projection finds where the unwritten requests and responses went,
// including the orders real runs showed: a response that started its
// call before an older call's row landed, and a result that landed
// while the request it missed was out. A transcript the model cannot
// produce stays one the graph check refuses.
func TestTurnLimitsHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("turn_limits")), "..", "testdata", "turn_limits"))
	if err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		words string
		ok    bool
	}{
		{"prompt reply done", true},
		{"prompt hooked reply done", true},
		{"prompt t1-b:ok t1-a:ok reply done", true},
		// t2 started a alone: t1-b was still running, though its row
		// lands after t2's request.
		{"prompt t1-a:ok t1-b:ok t2-a:ok budget", true},
		// t1-a's timeout landed while the request for t1-b was out.
		{"prompt t1-b:ok t1-a:timeout t2-B:cancelled budget", true},
		{"prompt t1-a:ok t1-b:ok t2-B:limit reply budget", false},
		{"prompt t1-B:limit reply done", true},
		// A second reply with nothing unread: no request asked for it.
		{"prompt reply reply done", false},
		// The Stop hook continues once a turn.
		{"prompt hooked hooked reply done", false},
		// A budget stop with no request over the budget.
		{"prompt budget", false},
	} {
		v := g.Check(tlHistory(tlTranscript(tc.words)))
		if (v == nil) != tc.ok {
			t.Errorf("%q: path %v, want %v (%v)", tc.words, v == nil, tc.ok, v)
		}
	}
}
