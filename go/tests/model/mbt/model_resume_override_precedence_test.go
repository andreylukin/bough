//go:build !windows

package mbt

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/testbin"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/model_resume_override_precedence.fizz: a resumed session's
// effective model versus bough.yml's, when the three inputs
// (explicitSet, resumedOverride, configVal) change relative to Resume
// and later hot reloads. There is no turn in this flow, so the adapter
// drives a real `bough --headless` process directly (not through
// servetest/serve, which has no HTTP seam for a spawn's own --set
// flags or for a session's history before its first child ever ran):
// resumedModelSets and applyOverrides are cmd/bough's own startup
// logic, exercised the same way whether serve spawns the child or a
// person types `bough --resume <id>`.
//
// The real "effective" model is read off the live process with
// /model list (plugins/commands/model.go: `describeRow` prints
// r.Config["model"] straight from ctx.Desired()), which is exactly the
// row applyOverrides last set. Each of the three inputs' int values
// doubles as its own model id string ("0", "1", "2" for configVal,
// "7" for a recorded override, "9" for an explicit --set), so a single
// probe tells the adapter which one is winning.

// rmpModelOf renders a spec int as the model id logged for it.
func rmpModelOf(n int) string { return strconv.Itoa(n) }

// rmpWriteConfig is the bough.yml this flow edits: one llm row, model
// set to configVal's id.
func rmpWriteConfig(path string, configVal int) error {
	yml := fmt.Sprintf("- id: llm\n  plugin: llm-echo\n  config:\n    model: %q\n", rmpModelOf(configVal))
	return os.WriteFile(path, []byte(yml), 0o644)
}

// rmpBuf is a concurrency-safe stdout+stderr accumulator (the process
// writes on its own goroutine, the adapter polls on the runner's).
type rmpBuf struct {
	mu  sync.Mutex
	buf strings.Builder
}

func (b *rmpBuf) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Write(p)
}

func (b *rmpBuf) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.String()
}

// rmpProc is one running `bough --headless` process.
type rmpProc struct {
	cmd    *exec.Cmd
	stdin  *os.File
	out    *rmpBuf
	exited chan error
}

// rmpEnv replaces HOME so the process never reaches the real ~/.bough.
func rmpEnv(home string) []string {
	env := []string{"HOME=" + home}
	for _, kv := range os.Environ() {
		if !strings.HasPrefix(kv, "HOME=") && !strings.HasPrefix(kv, "BOUGH_ROOT=") &&
			!strings.HasPrefix(kv, "BOUGH_UPSTREAM=") {
			env = append(env, kv)
		}
	}
	return env
}

// rmpLaunch starts bin as `bough --headless` in cwd (which holds
// bough.yml) with HOME=home and the given extra args (e.g. --resume,
// an explicit --set), stdin held open so a later config edit's hot
// reload has something alive to land in.
func rmpLaunch(bin, cwd, home string, args []string) (*rmpProc, error) {
	full := append([]string{"--config", "bough.yml", "--set", "llm.plugin=llm-echo", "--set", "loop.plugin=loop"}, args...)
	full = append(full, "--headless")
	cmd := exec.Command(bin, full...)
	cmd.Dir = cwd
	cmd.Env = rmpEnv(home)
	out := &rmpBuf{}
	cmd.Stdout, cmd.Stderr = out, out
	r, w, err := os.Pipe()
	if err != nil {
		return nil, err
	}
	cmd.Stdin = r
	if err := cmd.Start(); err != nil {
		r.Close()
		w.Close()
		return nil, err
	}
	r.Close()
	p := &rmpProc{cmd: cmd, stdin: w, out: out, exited: make(chan error, 1)}
	go func() { p.exited <- cmd.Wait() }()
	return p, nil
}

func (p *rmpProc) send(line string) error {
	_, err := fmt.Fprintln(p.stdin, line)
	return err
}

// close stops the process, killing it if it does not exit on its own
// (a walk's Cleanup, or the next Init tidying up a prior walk's proc).
func (p *rmpProc) close() {
	p.stdin.Close()
	select {
	case <-p.exited:
	case <-time.After(5 * time.Second):
		p.cmd.Process.Kill()
		<-p.exited
	}
}

// rmpModelLine matches /model list's current-row line: "model: llm-echo
// · <id>" (describeRow omits "· id" only when the row has no model
// configured, which this flow's config never leaves unset).
var rmpModelLine = regexp.MustCompile(`model: llm-echo · (\d+)\n`)

// rmpAdapter is both the fmbt.Model and the spec's one Session role.
type rmpAdapter struct {
	t   *testing.T
	bin string
	gate

	home, cwd, configPath, histPath, id string
	proc                                *rmpProc

	configVal, explicitSet, resumedOverride int
	resumed                                 bool
	effective                               int

	// skipExplicitSet is TestModelResumeOverridePrecedenceCatchesWrongAdapter's
	// deliberate bug: Resume drops the explicit --set it owes the spawn,
	// so a config or a recorded override can win instead.
	skipExplicitSet bool
}

func newRmpAdapter(t *testing.T) *rmpAdapter {
	bin, err := testbin.Path()
	if err != nil {
		t.Fatal(err)
	}
	return &rmpAdapter{t: t, bin: bin}
}

// Init starts each walk on a fresh HOME/cwd and a pre-minted (but not
// yet spawned) session id: RecordedOverridePresent and Resume decide
// whether its history file ever gets a model entry before the first
// spawn reads it.
func (a *rmpAdapter) Init() error {
	if a.proc != nil {
		a.proc.close()
		a.proc = nil
	}
	base := a.t.TempDir()
	a.home, a.cwd = filepath.Join(base, "home"), filepath.Join(base, "cwd")
	if err := os.MkdirAll(a.home, 0o755); err != nil {
		return err
	}
	if err := os.MkdirAll(a.cwd, 0o755); err != nil {
		return err
	}
	a.configPath = filepath.Join(a.cwd, "bough.yml")
	a.configVal, a.explicitSet, a.resumedOverride, a.resumed, a.effective = 0, 0, 0, false, -1
	if err := rmpWriteConfig(a.configPath, a.configVal); err != nil {
		return err
	}
	a.id = history.NewID()
	a.histPath = filepath.Join(a.home, ".bough", "history", a.id+".jsonl")
	if err := os.MkdirAll(filepath.Dir(a.histPath), 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(a.histPath, nil, 0o644); err != nil {
		return err
	}
	a.gate.reset()
	return nil
}

func (a *rmpAdapter) Cleanup() error {
	if a.proc != nil {
		a.proc.close()
		a.proc = nil
	}
	return nil
}

func (a *rmpAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

func (a *rmpAdapter) GetState() (map[string]any, error) {
	return map[string]any{
		"configVal": a.configVal, "explicitSet": a.explicitSet,
		"resumedOverride": a.resumedOverride, "resumed": a.resumed, "effective": a.effective,
	}, nil
}

// queryEffective sends one /model list and reads back the row's
// current model id — the real system's answer, not a value the
// adapter derives itself.
func (a *rmpAdapter) queryEffective() (int, error) {
	before := strings.Count(a.proc.out.String(), "usage: /model")
	if err := a.proc.send("/model list"); err != nil {
		return 0, err
	}
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		out := a.proc.out.String()
		if strings.Count(out, "usage: /model") > before {
			ms := rmpModelLine.FindAllStringSubmatch(out, -1)
			if len(ms) == 0 {
				return 0, fmt.Errorf("model-resume: /model list answered with no model id:\n%s", out)
			}
			return strconv.Atoi(ms[len(ms)-1][1])
		}
		time.Sleep(20 * time.Millisecond)
	}
	return 0, fmt.Errorf("model-resume: /model list timed out:\n%s", a.proc.out.String())
}

// settle polls until two reads 400ms apart agree, so a hot reload's
// 300ms debounce has had time to land before the adapter trusts what
// it saw — genuinely waiting for the real system to settle, not
// looping until it matches what the spec expects.
func (a *rmpAdapter) settle() (int, error) {
	prev, stable := -999, 0
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		v, err := a.queryEffective()
		if err != nil {
			return 0, err
		}
		if v == prev {
			stable++
		} else {
			stable = 1
		}
		prev = v
		if stable >= 2 {
			return v, nil
		}
		time.Sleep(400 * time.Millisecond)
	}
	return prev, nil
}

// EditConfig: bough.yml edited by hand or by another tool, at any
// time. A pure file write: it never itself moves `effective`, the same
// way the product only recomputes it at a resolveSession or a
// reconcile.
func (a *rmpAdapter) EditConfig() error {
	if !a.gate.pass(a.configVal < 2) {
		return nil
	}
	a.configVal++
	return rmpWriteConfig(a.configPath, a.configVal)
}

// SetExplicitFlag: an explicit --set on the resume's command line,
// fixed before the process exists.
func (a *rmpAdapter) SetExplicitFlag() error {
	if !a.gate.pass(!a.resumed && a.explicitSet == 0) {
		return nil
	}
	a.explicitSet = 9
	return nil
}

// RecordedOverridePresent: the session's history already has a
// recorded /model switch, written directly the way resumedModelSets
// reads it — before the session's first-ever child, resolveSession
// reads whatever is on disk once.
func (a *rmpAdapter) RecordedOverridePresent() error {
	if !a.gate.pass(!a.resumed && a.resumedOverride == 0) {
		return nil
	}
	a.resumedOverride = 7
	_, err := history.AppendFile(a.histPath, "model", map[string]any{
		"sets": []any{"llm.model=" + rmpModelOf(a.resumedOverride)},
	})
	return err
}

// Resume: the process's first load. cmd/bough's own resolveSession +
// resumedModelSets + applyOverrides run here, for real.
func (a *rmpAdapter) Resume() error {
	if !a.gate.pass(!a.resumed) {
		return nil
	}
	a.resumed = true
	args := []string{"--resume", a.id + ".jsonl"}
	if a.explicitSet != 0 && !a.skipExplicitSet {
		args = append(args, "--set", "llm.model="+rmpModelOf(a.explicitSet))
	}
	proc, err := rmpLaunch(a.bin, a.cwd, a.home, args)
	if err != nil {
		return err
	}
	a.proc = proc
	v, err := a.settle()
	if err != nil {
		return err
	}
	a.effective = v
	return nil
}

// HotReload: a later config reload (watchConfig's debounce firing
// runtimeSet). Nothing to trigger by hand — fsnotify is already
// watching bough.yml — so this is where the adapter waits for
// whatever it has caused to land and reads it back.
func (a *rmpAdapter) HotReload() error {
	if !a.gate.pass(a.resumed) {
		return nil
	}
	v, err := a.settle()
	if err != nil {
		return err
	}
	a.effective = v
	return nil
}

var modelResumeOverridePrecedenceActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"EditConfig":              action((*rmpAdapter).EditConfig),
	"SetExplicitFlag":         action((*rmpAdapter).SetExplicitFlag),
	"RecordedOverridePresent": action((*rmpAdapter).RecordedOverridePresent),
	"Resume":                  action((*rmpAdapter).Resume),
	"HotReload":               action((*rmpAdapter).HotReload),
}}

// Every enabled step spawns or talks to a real process, so walks are
// modest: 60 of up to 6 actions gives SetExplicitFlag (needed for
// TestModelResumeOverridePrecedenceCatchesWrongAdapter) good odds of
// landing before a Resume, without the run running long.
func modelResumeOverridePrecedenceOptions() map[string]any {
	return map[string]any{"max-seq-runs": 60, "max-actions": 6, "max-parallel-runs": 0}
}

func TestModelResumeOverridePrecedence(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newRmpAdapter(t)
	if err := runMBT(t, "model_resume_override_precedence", a, modelResumeOverridePrecedenceActions, modelResumeOverridePrecedenceOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// rmpFixtures is testdata/model_resume_override_precedence: the spec's
// checked-in graph.
func rmpFixtures() string {
	return filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "model_resume_override_precedence")
}

// rmpPaths are the generated paths that cover the spec's graph
// (envCover(): every state by default, every transition under
// MODEL_COVER=transitions).
func rmpPaths(t *testing.T, cover tracecheck.Cover) []swPath {
	t.Helper()
	raw, err := pathsJSONCover("model_resume_override_precedence", cover)
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

// rmpEqual compares a Go value with one decoded from the graph, where
// every number is a float64.
func rmpEqual(got, want any) bool {
	if n, ok := got.(int); ok {
		f, ok := want.(float64)
		return ok && float64(n) == f
	}
	return got == want
}

// walk drives one deterministic path through a real process and
// compares the adapter's state with the spec's at every node. Every
// action on a path is enabled, so a closed gate is the adapter
// misreading a require.
func (a *rmpAdapter) walk(p swPath) error {
	for i, step := range p.Trace {
		name := strings.TrimPrefix(step.Action, "Session#0.")
		var err error
		if i == 0 {
			err = a.Init()
		} else if f, ok := modelResumeOverridePrecedenceActions["Session"][name]; ok {
			_, err = f(a, nil)
		} else {
			err = fmt.Errorf("no adapter action for %q", step.Action)
		}
		if err == nil && a.gate.off {
			err = errors.New("the adapter's gate closed on an action the spec enables")
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, step.Action, err)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, step.Action, err)
		}
		for k, want := range step.State {
			field, ok := strings.CutPrefix(k, "Session#0.")
			if !ok {
				continue
			}
			if !rmpEqual(got[field], want) {
				return fmt.Errorf("step %d (%s): state mismatch for field %s: expected %v, actual %v", i, step.Action, field, want, got[field])
			}
		}
	}
	return nil
}

// TestModelResumeOverridePrecedencePaths walks every path against a
// real process: unlike the random run above (skipped unless
// MODEL_COVER=transitions), this is what a plain `go test` exercises.
func TestModelResumeOverridePrecedencePaths(t *testing.T) {
	t.Parallel()
	if _, err := tracecheck.Load(rmpFixtures()); err != nil {
		t.Fatal(err)
	}
	for i, p := range rmpPaths(t, envCover()) {
		p := p
		t.Run(fmt.Sprintf("path%03d_to_%d", i, p.Target), func(t *testing.T) {
			t.Parallel()
			a := newRmpAdapter(t)
			defer a.Cleanup()
			if err := a.walk(p); err != nil {
				t.Fatal(err)
			}
		})
	}
}

// TestModelResumeOverridePrecedenceCatchesWrongAdapter proves the walk
// above catches a real wiring bug: among the transitions
// MODEL_COVER=transitions' paths are guaranteed to take, one sets the
// explicit flag and later resumes with it still the only override.
// Dropping the --set that flag owes there must make effective mismatch
// what the spec expects (the explicit value), because the real process
// then runs whatever the recorded override or bough.yml says instead.
func TestModelResumeOverridePrecedenceCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	for _, p := range rmpPaths(t, tracecheck.CoverTransitions) {
		sawExplicit := false
		for i, step := range p.Trace {
			if step.Action == "Session#0.SetExplicitFlag" {
				sawExplicit = true
			}
			if step.Action == "Session#0.Resume" && sawExplicit {
				a := newRmpAdapter(t)
				a.skipExplicitSet = true
				err := a.walk(swPath{Trace: p.Trace[:i+1]})
				a.Cleanup()
				if err == nil {
					t.Fatal("a run whose Resume drops the explicit --set passed; the runner is not checking state")
				}
				t.Logf("caught, as it should be: %v", err)
				return
			}
		}
	}
	t.Fatal("no path sets the explicit flag before a Resume")
}
