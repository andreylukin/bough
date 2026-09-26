// Package testgate is test-only infrastructure for model-based tests
// that need to observe and drive the kernel's row lifecycle (Pending /
// Active / Failed, Reconcile vs Remount) through a real `bough serve`,
// the way go/tests/model/README.md's flows do. It is compiled into the
// binary like any other plugin, but every row it defines is inert
// unless a test's bough.yml mounts it — nothing here runs in a normal
// session.
//
// "test-gate-provider" models a provider row whose credential lives
// outside its config (like init-js's ~/.bough/init.js, or an API key in
// ~/.bough/env): Apply reads a file's contents directly rather than its
// config, so writing that file never changes the row's spec and a plain
// Reconcile is a no-op on a Failed instance of it — only an explicit
// Remount re-runs Apply. "test-gate-dependent" injects the service the
// provider provides once it is valid, so a walk can watch a dependent
// come up in the same settle pass the provider does.
// "test-gate-control" registers the two commands (`/gate-rows`,
// `/gate-remount`) a headless session can run synchronously (never
// reaching the loop/LLM — see hlDispatch) to read row states and force
// a Remount, since neither is otherwise reachable over serve's HTTP API.
package testgate

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
)

func init() {
	kernel.Register("test-gate-provider", func() kernel.Plugin { return providerPlugin{} })
	kernel.Register("test-gate-dependent", func() kernel.Plugin { return dependentPlugin{} })
	kernel.Register("test-gate-control", func() kernel.Plugin { return controlPlugin{} })
}

// serviceKey is what a valid provider row provides and a dependent row
// injects.
const serviceKey = "test-gate-key"

// wantOK is the file content Apply treats as a valid credential.
const wantOK = "ok"

type providerPlugin struct{}

func (providerPlugin) Name() string     { return "test-gate-provider" }
func (providerPlugin) Inject() []string { return nil }

// Apply reads cfg["keyfile"] (default $HOME/.bough/test-gate-key) fresh
// every time it runs, exactly like init-js reading init.js: the file is
// not the row's config, so editing it alone never makes Reconcile treat
// this row as changed.
func (providerPlugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	path, err := keyfilePath(cfg)
	if err != nil {
		return err
	}
	b, err := os.ReadFile(path)
	if err != nil || strings.TrimSpace(string(b)) != wantOK {
		return fmt.Errorf("test-gate-provider: %s does not hold a valid key", path)
	}
	ctx.Provide(serviceKey, true)
	return nil
}

func keyfilePath(cfg map[string]any) (string, error) {
	if p, ok := cfg["keyfile"].(string); ok && p != "" {
		return p, nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("test-gate-provider: %w", err)
	}
	return filepath.Join(home, ".bough", "test-gate-key"), nil
}

// dependentPlugin mounts once the provider's key has landed, the way
// loop/llm-small mount once their provider does.
type dependentPlugin struct{}

func (dependentPlugin) Name() string                                { return "test-gate-dependent" }
func (dependentPlugin) Inject() []string                            { return []string{serviceKey} }
func (dependentPlugin) Apply(*kernel.Context, map[string]any) error { return nil }

// controlPlugin registers the two commands a walk drives through a
// session's headless stdin (never the model): `/gate-rows` reports every
// row's live state as JSON, `/gate-remount <id>` forces that row (and
// its dependents) through kernel.Context.Remount.
type controlPlugin struct{}

func (controlPlugin) Name() string     { return "test-gate-control" }
func (controlPlugin) Inject() []string { return []string{"commands"} }

func (controlPlugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	reg, err := kernel.Get[*commands.Registry](ctx, "commands")
	if err != nil {
		return err
	}
	// config-set is the seam /connect and /model use: Reconcile with the
	// candidate rows built from the config file plus every override
	// applied so far. Re-asserting the provider row's own plugin name
	// changes nothing, so this reproduces exactly the "same spec"
	// Reconcile the flow is about.
	set, err := kernel.Get[func(...string) error](ctx, "config-set")
	if err != nil {
		return fmt.Errorf("test-gate-control: %w", err)
	}
	if err := reg.Register(commands.CommandInfo{Name: "gate-reconcile", Summary: "test-gate: same-spec Reconcile via config-set"}, func(string) (string, error) {
		if err := set("provider.plugin=test-gate-provider"); err != nil {
			return "", err
		}
		return "reconciled", nil
	}); err != nil {
		return fmt.Errorf("test-gate-control: %w", err)
	}
	ctx.Effect(func() { reg.Unregister("gate-reconcile") })
	// gate-fixkey does nothing to the tree itself — a walk writes the
	// key file directly, the way an env-file edit never touches a
	// session — but it gives that step a mark in the session's own
	// transcript, so a trace built from the transcript later can tell
	// the key was fixed before a Remount that needed it.
	if err := reg.Register(commands.CommandInfo{Name: "gate-fixkey", Summary: "test-gate: marks the transcript, changes nothing"}, func(string) (string, error) {
		return "ok", nil
	}); err != nil {
		return fmt.Errorf("test-gate-control: %w", err)
	}
	ctx.Effect(func() { reg.Unregister("gate-fixkey") })
	if err := reg.Register(commands.CommandInfo{Name: "gate-rows", Summary: "test-gate: row states as JSON"}, func(string) (string, error) {
		rows := ctx.Rows()
		out := make(map[string]string, len(rows))
		for _, r := range rows {
			out[r.ID] = string(r.State)
		}
		b, err := json.Marshal(out)
		if err != nil {
			return "", err
		}
		return string(b), nil
	}); err != nil {
		return fmt.Errorf("test-gate-control: %w", err)
	}
	ctx.Effect(func() { reg.Unregister("gate-rows") })

	if err := reg.Register(commands.CommandInfo{Name: "gate-remount", Usage: "<row-id> [...]", Summary: "test-gate: kernel.Remount(id)"}, func(args string) (string, error) {
		// Only the first word is the id: a caller that needs a distinct
		// command line for a setup step (so it does not read, in the
		// transcript, like one more of the walk's own Remount actions)
		// can pass trailing words and still get "remounted <id>" back.
		id, _, _ := strings.Cut(strings.TrimSpace(args), " ")
		if id == "" {
			return "", fmt.Errorf("gate-remount: want a row id")
		}
		if err := ctx.Remount(id); err != nil {
			return "", err
		}
		return "remounted " + id, nil
	}); err != nil {
		return fmt.Errorf("test-gate-control: %w", err)
	}
	ctx.Effect(func() { reg.Unregister("gate-remount") })
	return nil
}
