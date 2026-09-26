//go:build !windows

package mbt

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/provider_key_revocation_mid_session.fizz: a provider key that is
// valid when a turn starts is revoked externally while that turn is open.
// This flow combines two independent real mechanisms in one serve: a real
// model turn on the "llm" row (llm-control, exactly as example_test.go
// drives it) is the spec's open/req/closed/dones, and plugins/testgate's
// "test-gate-provider" row (its credential lives outside its config, like
// a real provider's API key in ~/.bough/env) is the spec's rowStatus —
// RevokeKey breaks its keyfile the way an external rotation would, and
// SettleFailed forces the real kernel.Context.Remount that discovers it,
// the same seam provider_row_pending_after_key_fix_test.go drives through
// test-gate-control's `/gate-remount` and `/gate-rows`. The two halves
// never touch each other's plumbing, which is the point: the row's status
// change must not corrupt the open turn's own close, and the turn closing
// must not, by itself, move the row.
const pkrConfig = "- id: llm\n  plugin: llm-control\n" +
	"- id: provider\n  plugin: test-gate-provider\n" +
	"- id: dependent\n  plugin: test-gate-dependent\n" +
	"- id: control\n  plugin: test-gate-control\n"

type pkrAdapter struct {
	t       *testing.T
	s       *servetest.Server
	dir     string // llm-control's queue
	keyfile string
	gate    gate

	id   string // this walk's session
	ids  []string
	turn int    // turn names are unique across walks: the queue is shared
	held string // the turn in flight, "" when none

	rowStatus  string // "ready" | "pending" | "failed"
	keyRevoked bool
	open       bool
	req        string // "" | "out" | "streaming"
	closed     string // "" | "done" | "error"
	dones      int

	// requestFailsAsOK is the deliberate bug
	// TestProviderKeyRevocationMidSessionCatchesWrongAdapter injects:
	// RequestFailsRevoked releases the open turn as a success instead of
	// the 401 it is supposed to be.
	requestFailsAsOK bool
}

func newPKRAdapter(t *testing.T) *pkrAdapter {
	s := servetest.Start(t, servetest.Options{Config: pkrConfig})
	return &pkrAdapter{t: t, s: s, dir: control.Dir(s.Home), keyfile: filepath.Join(s.Home, ".bough", "test-gate-key")}
}

// Init starts each walk on a fresh session with a valid key and the
// provider row already up, the way a real session starting healthy does.
func (a *pkrAdapter) Init() error {
	if err := os.WriteFile(a.keyfile, []byte("ok\n"), 0o600); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	a.held, a.open, a.req, a.closed, a.dones = "", false, "", "", 0
	a.rowStatus, a.keyRevoked = "ready", false
	a.gate.reset()
	if _, err := a.waitRows(func(m map[string]string) bool {
		return m["provider"] == "active" && m["dependent"] == "active"
	}); err != nil {
		return fmt.Errorf("initial boot never settled: %w", err)
	}
	return nil
}

// Cleanup lets a turn the walk left running finish, so its child is not
// holding a request while the next walk queues its own turns.
func (a *pkrAdapter) Cleanup() error {
	if a.held == "" {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	_, err := waitRow(a.s, a.id, "the held turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning })
	return err
}

func (a *pkrAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Provider", Index: 0}: a}, nil
}

func (a *pkrAdapter) GetState() (map[string]any, error) {
	return map[string]any{
		"rowStatus":  a.rowStatus,
		"keyRevoked": a.keyRevoked,
		"open":       a.open,
		"req":        a.req,
		"closed":     a.closed,
		"dones":      a.dones,
	}, nil
}

// --- the open turn: a real model request through llm-control ---

func (a *pkrAdapter) StartTurn() error {
	if !a.gate.pass(!a.open && a.rowStatus != "failed") {
		return nil
	}
	a.turn++
	name := fmt.Sprintf("t%04d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, "turn "+name); err != nil {
		return err
	}
	a.held = name
	control.WaitTaken(a.t, a.dir, name, actionTimeout)
	if _, err := waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning }); err != nil {
		return err
	}
	a.open, a.req, a.closed, a.dones = true, "out", "", 0
	return nil
}

func (a *pkrAdapter) Stream() error {
	if !a.gate.pass(a.req == "out") {
		return nil
	}
	control.Stream(a.t, a.dir, a.held, "chunk "+a.held, actionTimeout)
	a.req = "streaming"
	return nil
}

// FinishTurnCleanly: the open turn's own request went out and came back
// before the revocation took effect.
func (a *pkrAdapter) FinishTurnCleanly() error {
	if !a.gate.pass(a.open && (a.req == "out" || a.req == "streaming") && !a.keyRevoked) {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	row, err := waitRow(a.s, a.id, "the turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning })
	if err != nil {
		return err
	}
	a.open, a.req, a.dones = false, "", a.dones+1
	if row.Status == serve.StatusDone {
		a.closed = "done"
	} else {
		a.closed = "error"
	}
	return nil
}

// RequestFailsRevoked: the open turn's own request is the one that
// discovers the revoked key (a 401 mid-turn), so its close is not
// deferred or left ambiguous by the row's own transition — it happens in
// this same step.
func (a *pkrAdapter) RequestFailsRevoked() error {
	if !a.gate.pass(a.open && (a.req == "out" || a.req == "streaming") && a.keyRevoked) {
		return nil
	}
	if a.requestFailsAsOK {
		// The deliberate wrong wiring under test: releases the turn as if
		// the key were still good.
		control.Release(a.t, a.dir, a.held)
	} else {
		control.ReleaseWith(a.t, a.dir, a.held, control.Turn{Mode: "error", Error: "ANTHROPIC_API_KEY was rejected (HTTP 401)"})
	}
	a.held = ""
	row, err := waitRow(a.s, a.id, "the turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning })
	if err != nil {
		return err
	}
	a.open, a.req, a.dones = false, "", a.dones+1
	if row.Status == serve.StatusError {
		a.closed = "error"
	} else {
		a.closed = "done"
	}
	a.rowStatus = "pending"
	return nil
}

// --- the provider row: plugins/testgate, the real credential-outside-config seam ---

// RevokeKey is the external fact: the person edits ~/.bough/env, an org
// rotates the key, the provider suspends it. It never touches the
// session or the row directly — only the file the row's Apply reads.
func (a *pkrAdapter) RevokeKey() error {
	if !a.gate.pass(a.rowStatus == "ready" && !a.keyRevoked) {
		return nil
	}
	if err := os.Remove(a.keyfile); err != nil && !os.IsNotExist(err) {
		return err
	}
	a.keyRevoked = true
	return nil
}

// HealthCheckFails: no turn is open, so nothing discovers the revocation
// until something else tries to use the row. This never touches turn
// state, because there is none to touch.
func (a *pkrAdapter) HealthCheckFails() error {
	if !a.gate.pass(!a.open && a.rowStatus == "ready" && a.keyRevoked) {
		return nil
	}
	a.rowStatus = "pending"
	return nil
}

// SettleFailed is the real kernel.Context.Remount that discovers the
// broken key: unlike the fizz spec's step of its own, the real Remount
// unmounts and settles in one call, so there is no separately observable
// "pending" instant to poll for — SettleFailed is where the real remount
// actually happens, and RequestFailsRevoked/HealthCheckFails only record
// the model's own claim that the row is on its way there. Guarded with
// !a.open (stricter than the spec's bare require) so this never races a
// turn that started again while the row sat pending.
func (a *pkrAdapter) SettleFailed() error {
	if !a.gate.pass(a.rowStatus == "pending" && !a.open) {
		return nil
	}
	out, err := a.command("/gate-remount provider")
	if err != nil {
		return err
	}
	if out != "remounted provider" {
		return fmt.Errorf("gate-remount provider: %q", out)
	}
	rows, err := a.rows()
	if err != nil {
		return err
	}
	if rows["provider"] != "failed" {
		return fmt.Errorf("provider row did not fail on a revoked key: %v", rows)
	}
	a.rowStatus = "failed"
	return nil
}

// ReconcileSameSpec: a Failed row stays Failed through a same-spec
// Reconcile — provider_row_pending_after_key_fix_test.go's territory, not
// this flow's, but reachable here too and must change nothing.
func (a *pkrAdapter) ReconcileSameSpec() error {
	if !a.gate.pass(a.rowStatus == "failed") {
		return nil
	}
	_, err := a.command("/gate-reconcile")
	return err
}

// command and rows are plugins/testgate's control surface, exactly as
// provider_row_pending_after_key_fix_test.go's providerAdapter drives it.

func (a *pkrAdapter) command(text string) (string, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, text); err != nil {
		return "", err
	}
	deadline := time.Now().Add(actionTimeout)
	for {
		gctx, gcancel := actionCtx()
		_, entries, err := a.s.GetSession(gctx, a.id)
		gcancel()
		if err != nil {
			return "", err
		}
		if n := len(entries); n >= 2 && entries[n-1].Kind == "system" &&
			entries[n-2].Kind == "command" && entries[n-2].Text == text {
			return entries[n-1].Text, nil
		}
		if time.Now().After(deadline) {
			return "", fmt.Errorf("%s: no reply after %s", text, actionTimeout)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func (a *pkrAdapter) rows() (map[string]string, error) {
	out, err := a.command("/gate-rows")
	if err != nil {
		return nil, err
	}
	var m map[string]string
	if err := json.Unmarshal([]byte(out), &m); err != nil {
		return nil, fmt.Errorf("gate-rows: bad JSON %q: %w", out, err)
	}
	return m, nil
}

func (a *pkrAdapter) waitRows(ok func(map[string]string) bool) (map[string]string, error) {
	deadline := time.Now().Add(actionTimeout)
	for {
		m, err := a.rows()
		if err != nil {
			return nil, err
		}
		if ok(m) {
			return m, nil
		}
		if time.Now().After(deadline) {
			return m, fmt.Errorf("row states never settled: %v", m)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

var pkrActions = map[string]map[string]fmbt.ActionFunc{"Provider": {
	"StartTurn":           action((*pkrAdapter).StartTurn),
	"Stream":              action((*pkrAdapter).Stream),
	"RevokeKey":           action((*pkrAdapter).RevokeKey),
	"RequestFailsRevoked": action((*pkrAdapter).RequestFailsRevoked),
	"FinishTurnCleanly":   action((*pkrAdapter).FinishTurnCleanly),
	"HealthCheckFails":    action((*pkrAdapter).HealthCheckFails),
	"SettleFailed":        action((*pkrAdapter).SettleFailed),
	"ReconcileSameSpec":   action((*pkrAdapter).ReconcileSameSpec),
}}

// pkrOptions: every step is at most a real llm-control turn or a couple
// of synchronous headless commands, so a short walk suffices.
func pkrOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

// pkrHistory reads the abstract trace off a transcript. Stream, RevokeKey
// and HealthCheckFails leave nothing in the session's own history (like
// View/Leave in example_test.go) — HealthCheckFails, unlike those, does
// change rowStatus, so when a "/gate-remount provider" command is seen
// with rowStatus still "ready" a HealthCheckFails step is inserted ahead
// of it, the way pfrHistory's fragment() reconstructs an implied
// StreamFragment.
func pkrHistory(entries []history.Entry) []tracecheck.Step {
	status := func(s string) map[string]any { return map[string]any{"Provider#0.rowStatus": s} }
	steps := []tracecheck.Step{{Action: "Init", State: status("ready")}}
	cur := "ready"
	open := false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			text, _ := e.Data["text"].(string)
			if len(text) < 5 || text[:5] != "turn " {
				continue
			}
			open = true
			steps = append(steps, tracecheck.Step{Action: "Provider#0.StartTurn", State: status(cur)})
		case "error":
			if !open {
				continue
			}
			open = false
			cur = "pending"
			steps = append(steps, tracecheck.Step{Action: "Provider#0.RequestFailsRevoked", State: status(cur)})
		case "done":
			if !open {
				continue
			}
			open = false
			steps = append(steps, tracecheck.Step{Action: "Provider#0.FinishTurnCleanly", State: status(cur)})
		case "command":
			text, _ := e.Data["text"].(string)
			switch text {
			case "/gate-remount provider":
				if cur == "ready" {
					cur = "pending"
					steps = append(steps, tracecheck.Step{Action: "Provider#0.HealthCheckFails", State: status(cur)})
				}
				cur = "failed"
				steps = append(steps, tracecheck.Step{Action: "Provider#0.SettleFailed", State: status(cur)})
			case "/gate-reconcile":
				steps = append(steps, tracecheck.Step{Action: "Provider#0.ReconcileSameSpec", State: status(cur)})
			}
		}
	}
	return steps
}

func init() { historyProjections["provider_key_revocation_mid_session"] = pkrHistory }

func TestProviderKeyRevocationMidSession(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPKRAdapter(t)
	if err := runMBT(t, "provider_key_revocation_mid_session", a, pkrActions, pkrOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "provider_key_revocation_mid_session"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), pkrHistory)
	}
}

// The run above proves nothing unless a server that breaks the model
// fails it. This adapter releases RequestFailsRevoked as a success, the
// kind of wrong wiring a flow's adapter can have, and the run must say
// so.
func TestProviderKeyRevocationMidSessionCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPKRAdapter(t)
	a.requestFailsAsOK = true
	if err := runMBT(t, "provider_key_revocation_mid_session", a, pkrActions, pkrOptions()); err == nil {
		t.Fatal("a run whose RequestFailsRevoked ends the turn as done passed; the runner is not checking state")
	}
}
