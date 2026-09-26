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

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/rules_path_scope_gate.fizz against a real serve in code mode
// (the loop row is plugin: loop: the post-result hook that shows a
// scoped rule only runs on code blocks). Each walk is a fresh session in
// a fresh cwd; the rule files are real files in it, added and removed by
// the adapter the way a person editing them would.
//
//   - scoped / prefix are read off the disk. shown is read off the
//     transcript: a result carrying the "[rule: ..." injection.
//   - TouchMatching is a block that cats src/a.go.
//   - A "call in flight" is a background bash job (tools.bash(cmd,
//     limit) with job_grace 0s): the gate is decided when the call
//     starts, and the job then outlives its block, so later rule edits
//     and touches happen while it runs. It holds until the adapter
//     writes go-N; its finish wakes the agent, which is one more queued
//     model turn.
//   - StartBash checks the outcome against the gate it was decided under:
//     a refusal names the rule and starts nothing; otherwise the job
//     really starts.

const rpsConfig = controlConfig +
	"- id: tools\n  plugin: tools-basic\n  config:\n    job_grace: 0s\n" +
	"- id: loop\n  plugin: loop\n"

const rpsScoped = "---\npaths: \"src/**/*.go\"\n---\nSCOPED-BODY-MARK\n"
const rpsForbid = "prefix_rule(pattern = [\"sh\"], decision = \"forbidden\")\n"
const rpsHold = "touch start-$1; while [ ! -f go-$1 ]; do sleep 0.05; done; touch ran-$1\n"

type rpsAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	id, cwd string
	ids     []string
	walk    int
	turn    int // turn names are unique across walks
	call    int // the current job's number
	bash    string
	gated   string // the prefix state the last call was decided under

	// ignoreRule is the wrong-adapter test's bug: the forbidden rule is
	// never written, so the gate never refuses.
	ignoreRule bool
}

func newRpsAdapter(t *testing.T) *rpsAdapter {
	s := servetest.Start(t, servetest.Options{Config: rpsConfig})
	return &rpsAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

func (a *rpsAdapter) path(p ...string) string { return filepath.Join(append([]string{a.cwd}, p...)...) }

func (a *rpsAdapter) write(p, body string) error {
	if err := os.MkdirAll(filepath.Dir(a.path(p)), 0o755); err != nil {
		return err
	}
	return os.WriteFile(a.path(p), []byte(body), 0o644)
}

func (a *rpsAdapter) exists(p string) bool { _, err := os.Stat(a.path(p)); return err == nil }

func (a *rpsAdapter) Init() error {
	a.walk++
	a.cwd = a.s.Dir(a.t, fmt.Sprintf("w%d", a.walk))
	if err := a.write("src/a.go", "package a\n"); err != nil {
		return err
	}
	if err := a.write("hold.sh", rpsHold); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.cwd, "")
	if err != nil {
		return err
	}
	a.id, a.bash, a.gated = row.ID, "idle", "none"
	a.ids = append(a.ids, row.ID)
	a.gate.reset()
	return nil
}

// Cleanup lets a job the walk left holding finish.
func (a *rpsAdapter) Cleanup() error {
	if a.bash == "inflight" {
		return a.write(fmt.Sprintf("go-%d", a.call), "")
	}
	return nil
}

func (a *rpsAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Rules", Index: 0}: a}, nil
}

func (a *rpsAdapter) results() []history.Entry {
	entries, _ := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
	var out []history.Entry
	for _, e := range entries {
		if e.Kind == "result" {
			out = append(out, e)
		}
	}
	return out
}

func text(e history.Entry) string { s, _ := e.Data["text"].(string); return s }

func (a *rpsAdapter) shown() bool {
	for _, e := range a.results() {
		if strings.Contains(text(e), "[rule: ") {
			return true
		}
	}
	return false
}

func (a *rpsAdapter) GetState() (map[string]any, error) {
	prefix := "none"
	if a.exists(".codex/rules/x.rules") || (a.ignoreRule && a.gated == "forbidden") {
		prefix = "forbidden"
	}
	scoped := "absent"
	if a.exists(".claude/rules/scoped.md") {
		scoped = "present"
	}
	return map[string]any{"scoped": scoped, "shown": a.shown(), "prefix": prefix, "bash": a.bash, "gate": a.gated}, nil
}

// run is one model turn: a block, then the closing reply once its
// result is in. It returns the block's result text.
func (a *rpsAdapter) run(js string) (string, error) {
	a.turn++
	marker := fmt.Sprintf("k%04d", a.turn)
	first, second := marker+"a", marker+"b"
	block := "```js\ntry { " + js + " } catch (e) { 'not run: ' + e } // " + marker + "\n```"
	control.Queue(a.t, a.dir, first, control.Turn{Mode: "ok", Text: block})
	control.Queue(a.t, a.dir, second, control.Turn{Mode: "ok", Text: "done " + marker})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, "turn "+marker); err != nil {
		return "", err
	}
	control.WaitTaken(a.t, a.dir, second, actionTimeout)
	if _, err := waitRow(a.s, a.id, "the turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning }); err != nil {
		return "", err
	}
	for _, e := range a.results() {
		if c, _ := e.Data["code"].(string); strings.Contains(c, marker) {
			return text(e), nil
		}
	}
	return "", fmt.Errorf("rules_path_scope_gate: no result for %s", marker)
}

func (a *rpsAdapter) AddScoped() error {
	if !a.gate.pass(!a.exists(".claude/rules/scoped.md")) {
		return nil
	}
	return a.write(".claude/rules/scoped.md", rpsScoped)
}

func (a *rpsAdapter) RemoveScoped() error {
	if !a.gate.pass(a.exists(".claude/rules/scoped.md")) {
		return nil
	}
	return os.Remove(a.path(".claude/rules/scoped.md"))
}

func (a *rpsAdapter) TouchMatching() error {
	if !a.gate.pass(a.exists(".claude/rules/scoped.md") && !a.shown()) {
		return nil
	}
	got, err := a.run(`tools.bash("cat src/a.go")`)
	if err != nil {
		return err
	}
	if !strings.Contains(got, "SCOPED-BODY-MARK") {
		return fmt.Errorf("rules_path_scope_gate: touching src/a.go did not show the scoped rule: %q", got)
	}
	return nil
}

func (a *rpsAdapter) AddForbidden() error {
	if !a.gate.pass(!a.exists(".codex/rules/x.rules")) {
		return nil
	}
	if a.ignoreRule {
		return nil
	}
	return a.write(".codex/rules/x.rules", rpsForbid)
}

func (a *rpsAdapter) RemoveForbidden() error {
	if !a.gate.pass(a.exists(".codex/rules/x.rules")) {
		return nil
	}
	return os.Remove(a.path(".codex/rules/x.rules"))
}

func (a *rpsAdapter) StartBash() error {
	if !a.gate.pass(a.bash != "inflight") {
		return nil
	}
	a.gated = "none"
	if a.exists(".codex/rules/x.rules") {
		a.gated = "forbidden"
	}
	a.call++
	got, err := a.run(fmt.Sprintf(`tools.bash("sh hold.sh %d", "5m")`, a.call))
	if err != nil {
		return err
	}
	refused := strings.Contains(got, "command refused by rule")
	if refused != (a.gated == "forbidden") {
		return fmt.Errorf("rules_path_scope_gate: decided under prefix %s but the result was %q", a.gated, got)
	}
	if refused {
		a.bash = "refused"
		if a.exists(fmt.Sprintf("start-%d", a.call)) {
			return fmt.Errorf("rules_path_scope_gate: a refused call started anyway")
		}
		return nil
	}
	a.bash = "inflight"
	return a.waitFile(fmt.Sprintf("start-%d", a.call))
}

func (a *rpsAdapter) waitFile(p string) error {
	for end := time.Now().Add(10 * time.Second); time.Now().Before(end); time.Sleep(20 * time.Millisecond) {
		if a.exists(p) {
			return nil
		}
	}
	return fmt.Errorf("rules_path_scope_gate: %s never appeared", p)
}

func (a *rpsAdapter) FinishBash() error {
	if !a.gate.pass(a.bash == "inflight") {
		return nil
	}
	// The job's exit wakes the idle agent: one more model turn.
	a.turn++
	name := fmt.Sprintf("k%04dw", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "ok", Text: "noted the job"})
	if err := a.write(fmt.Sprintf("go-%d", a.call), ""); err != nil {
		return err
	}
	if err := a.waitFile(fmt.Sprintf("ran-%d", a.call)); err != nil {
		return err
	}
	control.WaitTaken(a.t, a.dir, name, actionTimeout)
	if _, err := waitRow(a.s, a.id, "the wake turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning }); err != nil {
		return err
	}
	a.bash = "ran"
	return nil
}

var rpsActions = map[string]map[string]fmbt.ActionFunc{"Rules": {
	"AddScoped":       action((*rpsAdapter).AddScoped),
	"RemoveScoped":    action((*rpsAdapter).RemoveScoped),
	"TouchMatching":   action((*rpsAdapter).TouchMatching),
	"AddForbidden":    action((*rpsAdapter).AddForbidden),
	"RemoveForbidden": action((*rpsAdapter).RemoveForbidden),
	"StartBash":       action((*rpsAdapter).StartBash),
	"FinishBash":      action((*rpsAdapter).FinishBash),
}}

func rpsOptions() map[string]any {
	return map[string]any{"max-seq-runs": 60, "max-actions": 6, "max-parallel-runs": 0}
}

// rulesPathScopeGateHistory: the flow's steps are mostly files on disk,
// which a transcript does not carry, so only the start is projected.
func rulesPathScopeGateHistory(entries []history.Entry) []tracecheck.Step {
	return []tracecheck.Step{{Action: "Init", State: map[string]any{"Rules#0.bash": "idle"}}}
}

func init() { historyProjections["rules_path_scope_gate"] = rulesPathScopeGateHistory }

func TestRulesPathScopeGate(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newRpsAdapter(t)
	if err := runMBT(t, "rules_path_scope_gate", a, rpsActions, rpsOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

func TestRulesPathScopeGateCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newRpsAdapter(t)
	a.ignoreRule = true
	if err := runMBT(t, "rules_path_scope_gate", a, rpsActions, rpsOptions()); err == nil {
		t.Fatal("a run whose forbidden rule is never written passed; the runner is not checking state")
	}
}
