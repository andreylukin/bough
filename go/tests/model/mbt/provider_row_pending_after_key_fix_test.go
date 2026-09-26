//go:build !windows

package mbt

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/provider_row_pending_after_key_fix.fizz: a provider row started
// with a bad or missing key goes Failed and, per kernel/reconcile.go,
// stays Failed until its plugin, config or disabled bit changes — never
// on its own. This flow drives that through plugins/testgate, which is
// test-only infrastructure built for exactly this: "provider" reads a
// key file outside its config (like init-js reading init.js, or an API
// key in ~/.bough/env), "dependent" injects the service it provides,
// and "control" registers three commands a headless session runs
// synchronously (never reaching the loop/LLM) so the walk can read row
// states and force kernel.Context.Remount without any HTTP surface for
// it existing.

// providerConfig mounts the three test-gate rows plus the default llm
// row every session needs to start.
const providerConfig = "- id: llm\n  plugin: llm-echo\n" +
	"- id: provider\n  plugin: test-gate-provider\n" +
	"- id: dependent\n  plugin: test-gate-dependent\n" +
	"- id: control\n  plugin: test-gate-control\n"

type providerAdapter struct {
	t        *testing.T
	s        *servetest.Server
	keyfile  string
	gate     gate
	id       string // this walk's session, one child process = one kernel tree
	keyValid bool
	ready    bool // status == "ready": nothing in the spec ever takes it back to failed
	remounts int
	ids      []string

	// releaseAsOK is the deliberate bug TestProviderRowPendingAfterKeyFixCatchesWrongAdapter
	// injects: Remount reports success without the row having become ready.
	releaseAsOK bool
}

func newProviderAdapter(t *testing.T) *providerAdapter {
	s := servetest.Start(t, servetest.Options{Config: providerConfig})
	return &providerAdapter{t: t, s: s, keyfile: filepath.Join(s.Home, ".bough", "test-gate-key")}
}

// Init starts each walk on a fresh session: the key file is gone (a
// fresh session process reads it at Apply, so the provider row starts
// Failed) and a new session is a fresh kernel tree, so nothing about a
// previous walk's key or remounts carries over.
// Init needs the spec's starting shape — provider Failed, dependent
// Pending, nothing remounted — but kernel.Context.Mount (the initial
// mount every new session process does at boot) is strict: an Apply
// error there fails the whole process, unlike Reconcile/Remount, which
// tolerate a row going Failed and leave the rest running. So a session
// cannot boot straight into the state the walk starts from; Init gets
// there the way a real one would — start valid, then break the key and
// force a Remount that now fails — before the walk's own actions run.
func (a *providerAdapter) Init() error {
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
	a.keyValid, a.ready, a.remounts = false, false, 0
	a.gate.reset()
	a.ids = append(a.ids, row.ID)
	if _, err := a.waitRows(func(m map[string]string) bool {
		return m["provider"] == "active" && m["dependent"] == "active"
	}); err != nil {
		return fmt.Errorf("initial boot never settled: %w", err)
	}
	if err := os.Remove(a.keyfile); err != nil {
		return err
	}
	// A distinct command line ("setup", ignored by gate-remount past the
	// id) so this bootstrap step never reads, in the transcript, as one
	// more of the walk's own Remount actions — providerHistory keys off
	// the exact text "/gate-remount provider".
	if out, err := a.command("/gate-remount provider setup"); err != nil || out != "remounted provider" {
		return fmt.Errorf("breaking the key: %q, %w", out, err)
	}
	rows, err := a.waitRows(func(m map[string]string) bool {
		return m["provider"] == "failed" && m["dependent"] == "pending"
	})
	if err != nil {
		return fmt.Errorf("initial row states: %w (%v)", err, rows)
	}
	return nil
}

// Cleanup: every action here is a synchronous command or a plain file
// write, nothing is ever left held between steps.
func (a *providerAdapter) Cleanup() error { return nil }

func (a *providerAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Provider", Index: 0}: a}, nil
}

func (a *providerAdapter) GetState() (map[string]any, error) {
	rows, err := a.rows()
	if err != nil {
		return nil, err
	}
	status := "failed"
	if rows["provider"] == "active" {
		status = "ready"
	}
	return map[string]any{
		"status":           status,
		"keyValid":         a.keyValid,
		"dependentMounted": rows["dependent"] == "active",
		"remounts":         a.remounts,
	}, nil
}

// ReconcileSameSpec runs `/gate-reconcile`: config-set with the
// provider row's own current plugin, the seam /connect and /model use.
// It never changes the row's spec, so it must never change its state.
func (a *providerAdapter) ReconcileSameSpec() error {
	if !a.gate.pass(true) {
		return nil
	}
	_, err := a.command("/gate-reconcile")
	return err
}

// FixKey writes a valid key to the file the provider row reads outside
// its config — the /connect or hand-edit-of-~/.bough/env equivalent.
// It touches nothing in the session at all, the way editing an env file
// leaves no mark on the session either.
func (a *providerAdapter) FixKey() error {
	if !a.gate.pass(!a.ready && !a.keyValid) {
		return nil
	}
	if err := os.WriteFile(a.keyfile, []byte("ok\n"), 0o600); err != nil {
		return err
	}
	// `/gate-fixkey` changes nothing; it only marks the transcript so
	// providerHistory can place this step, the way the real write to
	// ~/.bough/env cannot.
	if _, err := a.command("/gate-fixkey"); err != nil {
		return err
	}
	a.keyValid = true
	return nil
}

// Remount runs `/gate-remount provider`: kernel.Context.Remount, the
// only thing that re-runs Apply on a same-spec Failed row.
func (a *providerAdapter) Remount() error {
	if !a.gate.pass(!a.ready && a.keyValid) {
		return nil
	}
	// releaseAsOK is the deliberate wrong wiring
	// TestProviderRowPendingAfterKeyFixCatchesWrongAdapter injects:
	// claim the row came up without ever asking the server to remount
	// it, so the real row is left Failed while the adapter reports
	// ready.
	if a.releaseAsOK {
		a.remounts++
		a.ready = true
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
	if rows["provider"] == "active" {
		a.remounts++
		a.ready = true
	}
	return nil
}

// command sends text as a headless line (dispatched as a command,
// never reaching the loop/LLM — plugins/ui/headless.go hlLineIn) and
// returns the "system" reply it produced.
func (a *providerAdapter) command(text string) (string, error) {
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

// rows runs `/gate-rows` and decodes the JSON it prints: every desired
// row's id to its live kernel.State.
func (a *providerAdapter) rows() (map[string]string, error) {
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

func (a *providerAdapter) waitRows(ok func(map[string]string) bool) (map[string]string, error) {
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

var providerActions = map[string]map[string]fmbt.ActionFunc{"Provider": {
	"ReconcileSameSpec": action((*providerAdapter).ReconcileSameSpec),
	"FixKey":            action((*providerAdapter).FixKey),
	"Remount":           action((*providerAdapter).Remount),
}}

// providerOptions: every step is one or two HTTP round trips against a
// real serve with no model turn, so a short walk suffices.
func providerOptions() map[string]any {
	return map[string]any{"max-seq-runs": 60, "max-actions": 6, "max-parallel-runs": 0}
}

// providerHistory reads the abstract trace off the "command"/"system"
// pairs hlDispatch appends: the ReconcileSameSpec and Remount actions
// each leave one such pair (with their fixed command text), FixKey and
// the walk's own `/gate-rows` state probes leave none the projection
// recognizes and are skipped, exactly like View/Leave in the worked
// example.
func providerHistory(entries []history.Entry) []tracecheck.Step {
	status := func(s string) map[string]any { return map[string]any{"Provider#0.status": s} }
	steps := []tracecheck.Step{{Action: "Init", State: status("failed")}}
	cur := "failed"
	for _, e := range entries {
		if e.Kind != "command" {
			continue
		}
		text, _ := e.Data["text"].(string)
		switch {
		case text == "/gate-reconcile":
			steps = append(steps, tracecheck.Step{Action: "Provider#0.ReconcileSameSpec", State: status(cur)})
		case text == "/gate-fixkey":
			steps = append(steps, tracecheck.Step{Action: "Provider#0.FixKey", State: status(cur)})
		case text == "/gate-remount provider":
			cur = "ready"
			steps = append(steps, tracecheck.Step{Action: "Provider#0.Remount", State: status(cur)})
		}
	}
	return steps
}

func init() { historyProjections["provider_row_pending_after_key_fix"] = providerHistory }

func TestProviderRowPendingAfterKeyFix(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newProviderAdapter(t)
	if err := runMBT(t, "provider_row_pending_after_key_fix", a, providerActions, providerOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "provider_row_pending_after_key_fix"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), providerHistory)
	}
}

// The run above proves nothing unless a server that breaks the model
// fails it. This adapter counts a Remount as successful without
// checking the row actually came up, the kind of wrong wiring a flow's
// adapter can have, and the run must say so.
func TestProviderRowPendingAfterKeyFixCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newProviderAdapter(t)
	a.releaseAsOK = true
	if err := runMBT(t, "provider_row_pending_after_key_fix", a, providerActions, providerOptions()); err == nil {
		t.Fatal("a run whose Remount counts without checking the row passed; the runner is not checking state")
	}
}
