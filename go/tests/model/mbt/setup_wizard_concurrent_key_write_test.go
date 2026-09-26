//go:build !windows

package mbt

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/connect"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/setup_wizard_concurrent_key_write.fizz against a real serve: the
// welcome wizard's key save (POST /api/setup/key, provider A) racing a
// concurrent `bough setup`-style save (connect.WriteKey called directly,
// provider B), both against the same ~/.bough/env. Both go through
// connect.WriteKey (writeMu, then a cross-process flock), which the
// spec's require clauses model as the one lock field: an Acquire is only
// ever enabled while lock == "none", so the walker itself can never
// schedule the two writers' read..write spans to overlap, which is
// exactly the guarantee the lock gives the real code. What the adapter
// checks against reality is that a writer's Read snapshot and its
// Write's actual effect on the file agree, and that the file itself
// (read fresh, never from adapter bookkeeping) always has both lines
// once both writers report done.
//
// zwqA/zwqB name the two provider keys the writers save.
const (
	zwqA = "anthropic"
	zwqB = "openai"
)

type zwqAdapter struct {
	t    *testing.T
	gate gate

	s *servetest.Server

	lock         string // "none", "welcome", "cli"
	welcomePhase string // "idle", "reading", "writing", "done"
	welcomeSeenB bool
	cliPhase     string // "idle", "reading", "writing", "done"
	cliSeenA     bool
	reloadedA    bool
	reloadedB    bool

	// bugDropOther is TestSetupWizardConcurrentKeyWriteCatchesWrongAdapter's
	// deliberate bug: CliWrite writes its own line only, the naive
	// read-modify-write the lock exists to rule out.
	bugDropOther bool
}

func newZwqAdapter(t *testing.T) *zwqAdapter {
	return &zwqAdapter{t: t}
}

func (a *zwqAdapter) Init() error {
	if a.s != nil {
		a.s.Close()
	}
	a.s = servetest.Start(a.t, servetest.Options{Config: controlConfig})
	a.lock = "none"
	a.welcomePhase, a.welcomeSeenB = "idle", false
	a.cliPhase, a.cliSeenA = "idle", false
	a.reloadedA, a.reloadedB = false, false
	a.gate.reset()
	return nil
}

func (a *zwqAdapter) Cleanup() error { return nil }

func (a *zwqAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Env", Index: 0}: a}, nil
}

func (a *zwqAdapter) envFile() string { return filepath.Join(a.s.Home, ".bough", "env") }

// fileState reads ~/.bough/env fresh off disk: whether A's and B's lines
// are present. This is never taken from adapter bookkeeping, so it is
// the one place a dropped line would show up.
func (a *zwqAdapter) fileState() (hasA, hasB bool, err error) {
	b, err := os.ReadFile(a.envFile())
	if os.IsNotExist(err) {
		return false, false, nil
	}
	if err != nil {
		return false, false, err
	}
	for line := range strings.SplitSeq(string(b), "\n") {
		k, _, ok := strings.Cut(strings.TrimPrefix(strings.TrimSpace(line), "export "), "=")
		if !ok {
			continue
		}
		switch strings.TrimSpace(k) {
		case "ANTHROPIC_API_KEY":
			hasA = true
		case "OPENAI_API_KEY":
			hasB = true
		}
	}
	return hasA, hasB, nil
}

func (a *zwqAdapter) GetState() (map[string]any, error) {
	hasA, hasB, err := a.fileState()
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"lock":         a.lock,
		"fileHasA":     hasA,
		"fileHasB":     hasB,
		"welcomePhase": a.welcomePhase,
		"welcomeSeenB": a.welcomeSeenB,
		"cliPhase":     a.cliPhase,
		"cliSeenA":     a.cliSeenA,
		"reloadedA":    a.reloadedA,
		"reloadedB":    a.reloadedB,
	}, nil
}

// --- the welcome wizard's save (provider A) -----------------------------

func (a *zwqAdapter) WelcomeAcquire() error {
	if !a.gate.pass(a.welcomePhase == "idle" && a.lock == "none") {
		return nil
	}
	a.lock, a.welcomePhase = "welcome", "reading"
	return nil
}

func (a *zwqAdapter) WelcomeRead() error {
	if !a.gate.pass(a.welcomePhase == "reading") {
		return nil
	}
	_, hasB, err := a.fileState()
	if err != nil {
		return err
	}
	a.welcomeSeenB, a.welcomePhase = hasB, "writing"
	return nil
}

// WelcomeWrite is the wizard's POST landing: the real API call, which
// runs connect.WriteKey under the hood the same way welcome.tsx's key
// prompt does.
func (a *zwqAdapter) WelcomeWrite() error {
	if !a.gate.pass(a.welcomePhase == "writing") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.API(ctx, http.MethodPost, "/api/setup/key", map[string]string{"provider": zwqA, "key": "sk-ant-wizard"}, nil); err != nil {
		return err
	}
	a.welcomePhase, a.lock = "done", "none"
	return nil
}

// --- the concurrent `bough setup`-style save (provider B) ---------------

func (a *zwqAdapter) CliAcquire() error {
	if !a.gate.pass(a.cliPhase == "idle" && a.lock == "none") {
		return nil
	}
	a.lock, a.cliPhase = "cli", "reading"
	return nil
}

func (a *zwqAdapter) CliRead() error {
	if !a.gate.pass(a.cliPhase == "reading") {
		return nil
	}
	hasA, _, err := a.fileState()
	if err != nil {
		return err
	}
	a.cliSeenA, a.cliPhase = hasA, "writing"
	return nil
}

// CliWrite is the same connect.WriteKey a second bough process reaches
// through /connect or `bough setup`, called directly here: the two
// writers share the file, not a process.
func (a *zwqAdapter) CliWrite() error {
	if !a.gate.pass(a.cliPhase == "writing") {
		return nil
	}
	var err error
	if a.bugDropOther {
		err = os.WriteFile(a.envFile(), []byte("OPENAI_API_KEY=sk-oa-cli\n"), 0o600)
	} else {
		err = connect.WriteKey(a.envFile(), "OPENAI_API_KEY", "sk-oa-cli")
	}
	if err != nil {
		return err
	}
	a.cliPhase, a.lock = "done", "none"
	return nil
}

// --- a hot env-file reload, unsynchronized with either writer -----------

func (a *zwqAdapter) Reload() error {
	a.gate.pass(true)
	hasA, hasB, err := a.fileState()
	if err != nil {
		return err
	}
	a.reloadedA, a.reloadedB = hasA, hasB
	return nil
}

var setupWizardConcurrentKeyWriteActions = map[string]map[string]fmbt.ActionFunc{"Env": {
	"WelcomeAcquire": action((*zwqAdapter).WelcomeAcquire),
	"WelcomeRead":    action((*zwqAdapter).WelcomeRead),
	"WelcomeWrite":   action((*zwqAdapter).WelcomeWrite),
	"CliAcquire":     action((*zwqAdapter).CliAcquire),
	"CliRead":        action((*zwqAdapter).CliRead),
	"CliWrite":       action((*zwqAdapter).CliWrite),
	"Reload":         action((*zwqAdapter).Reload),
}}

func setupWizardConcurrentKeyWriteOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 10, "max-parallel-runs": 0}
}

// TestSetupWizardConcurrentKeyWrite is the runner's random walks (part
// of the exhaustive MODEL_COVER=transitions run; see runMBT).
func TestSetupWizardConcurrentKeyWrite(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newZwqAdapter(t)
	if err := runMBT(t, "setup_wizard_concurrent_key_write", a, setupWizardConcurrentKeyWriteActions, setupWizardConcurrentKeyWriteOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

func zwqPaths(t *testing.T, cover tracecheck.Cover) []swPath {
	t.Helper()
	raw, err := pathsJSONCover("setup_wizard_concurrent_key_write", cover)
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Paths []swPath `json:"paths"`
	}
	if err := json.Unmarshal(raw, &f); err != nil {
		t.Fatal(err)
	}
	return f.Paths
}

// walk drives one path through a fresh serve and compares the adapter's
// state with the spec's at every node. Every action on a path is
// enabled, so a closed gate is the adapter misreading a require.
func (a *zwqAdapter) walk(p swPath) error {
	for i, step := range p.Trace {
		name := strings.TrimPrefix(step.Action, "Env#0.")
		var err error
		switch {
		case i == 0:
			err = a.Init()
		default:
			f, ok := setupWizardConcurrentKeyWriteActions["Env"][name]
			if !ok {
				err = fmt.Errorf("no adapter action for %q", step.Action)
				break
			}
			_, err = f(a, nil)
		}
		if err == nil && a.gate.off {
			err = fmt.Errorf("the adapter's gate closed on an action the spec enables")
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, step.Action, err)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, step.Action, err)
		}
		for k, want := range step.State {
			field, ok := strings.CutPrefix(k, "Env#0.")
			if !ok {
				continue
			}
			if got[field] != want {
				return fmt.Errorf("step %d (%s): state mismatch for field %s: expected %v, actual %v", i, step.Action, field, want, got[field])
			}
		}
	}
	return nil
}

// TestSetupWizardConcurrentKeyWritePaths walks every path against a real
// serve.
func TestSetupWizardConcurrentKeyWritePaths(t *testing.T) {
	t.Parallel()
	for i, p := range zwqPaths(t, envCover()) {
		t.Run(fmt.Sprintf("path%03d_to_%d", i, p.Target), func(t *testing.T) {
			t.Parallel()
			a := newZwqAdapter(t)
			if err := a.walk(p); err != nil {
				t.Fatal(err)
			}
		})
	}
}

// A CliWrite that drops A's line instead of preserving it (the naive
// read-modify-write the lock exists to rule out) must fail a walk: once
// both writers are done, both lines must survive.
func TestSetupWizardConcurrentKeyWriteCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	for _, p := range zwqPaths(t, tracecheck.CoverTransitions) {
		last := p.Trace[len(p.Trace)-1]
		if last.State["Env#0.fileHasA"] != true || last.State["Env#0.fileHasB"] != true {
			continue
		}
		a := newZwqAdapter(t)
		a.bugDropOther = true
		err := a.walk(p)
		if err != nil {
			t.Logf("caught, as it should be: %v", err)
			return
		}
	}
	t.Fatal("no path finishes with both lines present; the walks are not checking the drop")
}
