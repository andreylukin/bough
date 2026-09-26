//go:build !windows

package mbt

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/servetest"
)

// specs/cli_flag_vs_config_override_precedence.fizz against a real
// serve: one process whose project bough.yml (its cwd's, so
// resolveConfig picks it over ~/.bough/bough.yml — presence, not
// precedence) and whose ~/.bough/bough.yml are both edited live.
//
// `bough serve` itself has no --set flag (serveArgs only knows
// --insecure-bind/--host/--run) — applyOverrides is main.go's headless/
// TUI path. So the CLI flag here is exercised the same way a person's
// shell would run it: a fresh `bough --dump-config --set ...` probe in
// the live serve's own cwd and HOME, the exact resolveConfig+load+
// applyOverrides path reload() itself uses. The one real signal is
// example-wordcount's own validation: min_length must be an int, and
// applyOverrides always writes a --set value in as a bare string
// (main.go's applyOverrides: `rows[i].Config[key] = value`, value being
// the raw "id.key=value" text). So a --set on min_length makes the row
// FAIL every time, with the CLI's own value quoted in the error ("got
// 7"), whatever the file underneath says — the file's own min_length is
// a normal YAML int and would mount cleanly on its own. A defeated CLI
// flag shows up as either no failure at all (the file's int mounted) or
// a failure naming the file's value instead.
//
// loadedProjectVal/loadedHomeVal mirror projectVal/homeVal once
// HotReload has waited past the debounce: there is nothing to read
// back independently (see the spec's own comment on HotReload, which
// treats both as moving together with whatever is on disk), so, like
// project_env_precedence_live_reload's fileVer/rowLoaded, they are
// bookkeeping, not a second real read. What IS real, on every action,
// is the effective check: it is what would catch a genuine precedence
// bug, at every reachable state, not just at HotReload.
const (
	cflagCliVal = "7" // the --set value; always wins per the spec

	cflagProjectMin0 = 10 // valid ints: mount clean if ever actually read
	cflagProjectMin1 = 20
	cflagHomeMin0    = 30
	cflagHomeMin1    = 40
)

// cflagSettle clears fsnotify's 300ms debounce plus one reconcile pass.
const cflagSettle = 700 * time.Millisecond

func cflagProjectMinFor(gen int) int {
	if gen == 0 {
		return cflagProjectMin0
	}
	return cflagProjectMin1
}

func cflagHomeMinFor(gen int) int {
	if gen == 0 {
		return cflagHomeMin0
	}
	return cflagHomeMin1
}

func cflagProjectYAML(min int) string {
	return fmt.Sprintf("- id: llm\n  plugin: llm-control\n- id: wc\n  plugin: example-wordcount\n  config:\n    min_length: %d\n", min)
}

func cflagHomeYAML(min int) string {
	return fmt.Sprintf("- id: wc\n  plugin: example-wordcount\n  config:\n    min_length: %d\n", min)
}

type cflagAdapter struct {
	t    *testing.T
	s    *servetest.Server
	bin  string
	gate gate

	projectVal, homeVal             int // 0 or 1, the spec's own counters
	loadedProjectVal, loadedHomeVal int // bookkeeping; see the type comment

	// dropCLIOverride is TestCliFlagVsConfigOverridePrecedenceCatchesWrongAdapter's
	// deliberate bug: the probe never applies the --set flag, so the
	// file's own (valid) value would win for real.
	dropCLIOverride bool
}

// newCflagAdapter starts one serve for the whole run: its project
// bough.yml (in its own cwd) and its ~/.bough/bough.yml are both
// rewritten fresh by every Init. `bough serve` itself has no --set flag
// (serveArgs only knows --insecure-bind/--host/--run) — the daemon just
// needs to boot on these files; the precedence check is entirely the
// separate `bough --dump-config --set ...` probe() below, which is the
// same binary run the way a person or main.go's own headless/TUI path
// runs it.
func newCflagAdapter(t *testing.T) *cflagAdapter {
	bin := servetest.Binary(t)
	s := servetest.Start(t, servetest.Options{
		Config: cflagHomeYAML(cflagHomeMin0),
		Files:  map[string]string{"bough.yml": cflagProjectYAML(cflagProjectMin0)},
	})
	return &cflagAdapter{t: t, s: s, bin: bin}
}

func (a *cflagAdapter) writeProject() error {
	return os.WriteFile(filepath.Join(a.s.Home, "bough.yml"), []byte(cflagProjectYAML(cflagProjectMinFor(a.projectVal))), 0o644)
}

func (a *cflagAdapter) writeHome() error {
	return os.WriteFile(filepath.Join(a.s.Home, ".bough", "bough.yml"), []byte(cflagHomeYAML(cflagHomeMinFor(a.homeVal))), 0o644)
}

// Init resets both files to generation 0 and the adapter's own
// bookkeeping, then checks precedence at this, the very first state:
// startup already ran load()+applyOverrides once, before any edit.
func (a *cflagAdapter) Init() error {
	a.projectVal, a.homeVal, a.loadedProjectVal, a.loadedHomeVal = 0, 0, 0, 0
	if err := a.writeProject(); err != nil {
		return err
	}
	if err := a.writeHome(); err != nil {
		return err
	}
	time.Sleep(cflagSettle)
	a.gate.reset()
	return a.checkEffective()
}

func (a *cflagAdapter) Cleanup() error { return nil }

func (a *cflagAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Row", Index: 0}: a}, nil
}

// GetState is the Row role's state. cliVal/effective are the spec's own
// fixed sentinel (1): the real read is checkEffective, run from every
// action, not from here — a state read must stay cheap, and this one
// would otherwise spawn a process on every single step.
func (a *cflagAdapter) GetState() (map[string]any, error) {
	return map[string]any{
		"cliVal":           1,
		"projectVal":       a.projectVal,
		"homeVal":          a.homeVal,
		"loadedProjectVal": a.loadedProjectVal,
		"loadedHomeVal":    a.loadedHomeVal,
		"effective":        1,
	}, nil
}

// probe is a fresh `bough --dump-config`, in the live serve's own cwd
// and HOME: the exact resolveConfig+load+applyOverrides path reload()
// itself runs, over whatever is on disk right now. It returns the
// printed line for the "wc" row.
func (a *cflagAdapter) probe() (string, error) {
	args := []string{"--dump-config"}
	if !a.dropCLIOverride {
		args = append(args, "--set", "wc.min_length="+cflagCliVal)
	}
	cmd := a.s.Command(a.bin, nil, args...)
	// Command sets Stdout/Stderr to the server's own log buffer;
	// CombinedOutput refuses to run with either already set, so this
	// probe (a separate one-shot process, not the serve daemon) needs
	// its own.
	cmd.Stdout, cmd.Stderr = nil, nil
	out, err := cmd.CombinedOutput()
	if err != nil {
		return "", fmt.Errorf("dump-config: %w\n%s", err, out)
	}
	for _, line := range strings.Split(string(out), "\n") {
		if f := strings.Fields(line); len(f) > 0 && f[0] == "wc" {
			return line, nil
		}
	}
	return "", fmt.Errorf("dump-config: no %q row in output:\n%s", "wc", out)
}

// checkEffective is the spec's CLIAlwaysWins, for real: the --set value
// always lands in the row's config as a bare string (applyOverrides),
// so min_length always fails validation naming that string, whatever
// generation either file is on. A file's own (valid int) min_length
// would instead mount clean, or fail naming the file's own value — both
// are precedence lost, and both are caught here.
func (a *cflagAdapter) checkEffective() error {
	line, err := a.probe()
	if err != nil {
		return err
	}
	fields := strings.Fields(line)
	if len(fields) < 3 || fields[2] != "failed" || !strings.Contains(line, "got "+cflagCliVal) {
		return fmt.Errorf("cli flag lost precedence: wc row = %q, want it failed on %q", line, "got "+cflagCliVal)
	}
	return nil
}

// EditProjectConfig moves the resolved file (this cwd's bough.yml, the
// one resolveConfig picked at startup) to its next generation.
func (a *cflagAdapter) EditProjectConfig() error {
	if !a.gate.pass(a.projectVal < 1) {
		return nil
	}
	a.projectVal++
	if err := a.writeProject(); err != nil {
		return err
	}
	time.Sleep(cflagSettle)
	return a.checkEffective()
}

// EditHomeConfig moves ~/.bough/bough.yml to its next generation. It is
// never the resolved file here (the project one is always present), so
// this edit is genuinely inert on the running process — exercised all
// the same, to prove it stays inert rather than assumed to.
func (a *cflagAdapter) EditHomeConfig() error {
	if !a.gate.pass(a.homeVal < 1) {
		return nil
	}
	a.homeVal++
	if err := a.writeHome(); err != nil {
		return err
	}
	time.Sleep(cflagSettle)
	return a.checkEffective()
}

// HotReload is the fsnotify-debounced reload firing on the live serve:
// past the debounce, it must still be answering (a bad reload keeps the
// last good tree, per main.go's reload(), rather than crashing it), and
// precedence must still hold.
func (a *cflagAdapter) HotReload() error {
	if !a.gate.pass(a.loadedProjectVal != a.projectVal || a.loadedHomeVal != a.homeVal) {
		return nil
	}
	time.Sleep(cflagSettle)
	ctx, cancel := actionCtx()
	_, err := a.s.Build(ctx)
	cancel()
	if err != nil {
		return fmt.Errorf("serve unreachable after reload: %w", err)
	}
	if err := a.checkEffective(); err != nil {
		return err
	}
	a.loadedProjectVal, a.loadedHomeVal = a.projectVal, a.homeVal
	return nil
}

// Idle is always enabled: sitting still is not a deadlock, and
// precedence must hold there too.
func (a *cflagAdapter) Idle() error {
	a.gate.pass(true)
	return a.checkEffective()
}

var cflagActions = map[string]map[string]fmbt.ActionFunc{"Row": {
	"EditProjectConfig": action((*cflagAdapter).EditProjectConfig),
	"EditHomeConfig":    action((*cflagAdapter).EditHomeConfig),
	"HotReload":         action((*cflagAdapter).HotReload),
	"Idle":              action((*cflagAdapter).Idle),
}}

// Every action here spawns a probe process against a real serve, so
// walks are kept short, the same way chrOptions does for its own
// per-edit settle+observe cost.
func cflagOptions() map[string]any {
	return map[string]any{"max-seq-runs": 30, "max-actions": 6, "max-parallel-runs": 0}
}

func TestCliFlagVsConfigOverridePrecedence(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newCflagAdapter(t)
	if err := runMBT(t, "cli_flag_vs_config_override_precedence", a, cflagActions, cflagOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// The run above proves nothing unless a server that breaks the model
// fails it. This adapter's probe never applies the --set flag, so
// whichever file resolveConfig picked would win for real — the kind of
// regression applyOverrides running before, not after, the file load
// would cause — and the run must say so.
func TestCliFlagVsConfigOverridePrecedenceCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newCflagAdapter(t)
	a.dropCLIOverride = true
	if err := runMBT(t, "cli_flag_vs_config_override_precedence", a, cflagActions, cflagOptions()); err == nil {
		t.Fatal("a run whose probe never applies --set passed; the runner is not checking state")
	}
}
