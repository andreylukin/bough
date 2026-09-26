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

// specs/model_command_apply_race.fizz: two callers racing
// Supervisor.pick (POST /api/sessions/{id}/model) on one session, where
// the save (meta.Model, what the picker shows) and the deliver (the
// "/model" line reaching the child's stdin, what it actually runs on)
// are two separate steps a second caller's own pick can land between.
//
// The race needs the two callers' sends to land in a chosen order, not
// whatever a real race gives on this machine, so the adapter drives it
// through BOUGH_TEST_STEP_GATE (internal/stepgate): each SetModel call
// carries a tag, Supervisor.pick holds on "pick-<tag>" between its save
// and its Send, and Pick1/Pick2 launch the calls in goroutines that
// Deliver1/Deliver2 release one at a time, in whatever order a walk
// picks.
const modelRaceConfig = "- id: llm\n  plugin: llm-control\n  config:\n    model: m1\n"

// modelCommandApplyRaceAdapter plays one session's model picker against
// a real serve. model and applied are read off the server on every
// call — model from the row (meta.Model, what /api/models and the
// picker show), applied from the last "model: <plugin> · <model>"
// system line (what actually reached the child, live, whether or not a
// turn is running: see plugins/commands/model.go and
// plugins/ui/headless.go's hlDispatch). turnModel is the one field with
// no API of its own: the spec defines it as applied snapshotted when a
// turn ends, so the adapter snapshots it exactly then, the same way the
// worked example tracks `viewing`.
type modelCommandApplyRaceAdapter struct {
	t       *testing.T
	s       *servetest.Server
	dir     string // llm-control's queue
	gateDir string // BOUGH_TEST_STEP_GATE
	gate    gate

	id        string
	turn      int    // turn names are unique across walks: the queue is shared
	held      string // the turn in flight, "" when none
	turnModel string
	req1      string
	req2      string
	seq       int
	pend1     *racePick
	pend2     *racePick
	ids       []string

	// wrongFinish is the deliberate bug TestModelCommandApplyRaceCatchesWrongAdapter
	// injects: Finish always attributes the turn to "m1".
	wrongFinish bool
}

// racePick is one SetModel call parked on the step gate between its
// save and its deliver.
type racePick struct {
	tag    string
	target string
	err    chan error
}

func newModelCommandApplyRaceAdapter(t *testing.T) *modelCommandApplyRaceAdapter {
	gateDir := t.TempDir()
	s := servetest.Start(t, servetest.Options{
		Config: modelRaceConfig,
		Env:    []string{"BOUGH_TEST_STEP_GATE=" + gateDir},
	})
	// BOUGH_TEST_STEP_GATE also holds every stdin line a child reads
	// (plugins/ui/headless.go's hlGate, "in.<n>"), one process-wide gate
	// dir for every process this env var reaches. Writing "open" would
	// release those, but the same dir is where Supervisor.pick's own
	// "pick-<tag>" holds live (the child inherits serve's env), so
	// "open" would let those through too. Auto-release only the "in.*"
	// holds instead, leaving "pick-*" for the test to release by hand.
	autoReleaseStdinHolds(t, gateDir)
	return &modelCommandApplyRaceAdapter{t: t, s: s, dir: control.Dir(s.Home), gateDir: gateDir}
}

// autoReleaseStdinHolds drops "<name>.go" for every "in.<n>.held" that
// appears in dir, until the test ends.
func autoReleaseStdinHolds(t *testing.T, dir string) {
	stop := make(chan struct{})
	t.Cleanup(func() { close(stop) })
	go func() {
		seen := map[string]bool{}
		for {
			select {
			case <-stop:
				return
			case <-time.After(5 * time.Millisecond):
			}
			held, _ := filepath.Glob(filepath.Join(dir, "in.*.held"))
			for _, h := range held {
				name := strings.TrimSuffix(filepath.Base(h), ".held")
				if seen[name] {
					continue
				}
				seen[name] = true
				os.WriteFile(filepath.Join(dir, name+".go"), nil, 0o644)
			}
		}
	}()
}

// Init starts each walk on a fresh session, then forces one ordinary
// (untagged, non-racing) /model m1 so the session's saved choice and
// its applied model both start at "m1" — the row's own config already
// answers as "m1", but nothing has told the picker that yet.
func (a *modelCommandApplyRaceAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.id, a.held, a.req1, a.req2 = row.ID, "", "", ""
	a.pend1, a.pend2 = nil, nil
	a.gate.reset()
	if err := a.s.SetModel(ctx, a.id, "", "m1", ""); err != nil {
		return err
	}
	// Send only writes the "/model" line to the child's stdin; the
	// system line that names the swap lands once the child (which
	// SetModel also spawns) has actually read it, a moment later.
	if err := a.waitApplied("m1"); err != nil {
		return err
	}
	a.turnModel = "m1"
	a.ids = append(a.ids, row.ID)
	return nil
}

// waitApplied polls the row until its last "model: …" system line
// names want, the way Deliver's effect actually reaches history: a
// moment after Send returns, not inside the same request.
func (a *modelCommandApplyRaceAdapter) waitApplied(want string) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		ctx, cancel := actionCtx()
		_, lines, err := a.s.GetSession(ctx, a.id)
		cancel()
		if err != nil {
			return err
		}
		if appliedModel(lines) == want {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("model_command_apply_race: applied never reached %q", want)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func (a *modelCommandApplyRaceAdapter) Cleanup() error {
	if a.held == "" {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	_, err := waitRow(a.s, a.id, "the held turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning })
	return err
}

func (a *modelCommandApplyRaceAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

func (a *modelCommandApplyRaceAdapter) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, lines, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return nil, err
	}
	status := "idle"
	if row.Status == serve.StatusRunning {
		status = "running"
	}
	return map[string]any{
		"model":     row.Model,
		"applied":   appliedModel(lines),
		"turnModel": a.turnModel,
		"status":    status,
		"req1":      a.req1,
		"req2":      a.req2,
	}, nil
}

// appliedModel is the model named by the most recent "model: <plugin>
// · <model>" system line — the same echo internal/serve/api.go's
// lastModel reads to show a session's model before its next reply.
func appliedModel(lines []serve.Line) string {
	for i := len(lines) - 1; i >= 0; i-- {
		if lines[i].Kind != "system" {
			continue
		}
		rest, ok := strings.CutPrefix(lines[i].Text, "model: ")
		if !ok || strings.Contains(rest, "\n") {
			continue
		}
		if _, m, ok := strings.Cut(rest, " · "); ok && m != "" {
			return m
		}
	}
	return ""
}

// otherModel is the spec's toggle: Pick(N) reads the current save and
// asks for the other of the two models this flow uses.
func otherModel(cur string) string {
	if cur == "m1" {
		return "m2"
	}
	return "m1"
}

// startPick launches a SetModel call tagged so Supervisor.pick parks it
// on the step gate right after its save, before its deliver, and waits
// for that hold to announce itself (pick-<tag>.held) — i.e. for the
// save to have landed — before returning.
func (a *modelCommandApplyRaceAdapter) startPick(target string) (*racePick, error) {
	a.seq++
	tag := fmt.Sprintf("c%d", a.seq)
	p := &racePick{tag: tag, target: target, err: make(chan error, 1)}
	ctx, cancel := actionCtx()
	go func() {
		defer cancel()
		p.err <- a.s.SetModel(ctx, a.id, "", target, tag)
	}()
	held := filepath.Join(a.gateDir, "pick-"+tag+".held")
	if err := waitFile(held, actionTimeout); err != nil {
		return nil, fmt.Errorf("model_command_apply_race: %s never parked at the step gate: %w", tag, err)
	}
	return p, nil
}

// releasePick drops the "go" file the held pick is waiting on and
// waits for its SetModel call to return.
func releasePick(gateDir string, p *racePick) error {
	if err := os.WriteFile(filepath.Join(gateDir, "pick-"+p.tag+".go"), nil, 0o644); err != nil {
		return err
	}
	return <-p.err
}

func waitFile(path string, timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	for {
		if _, err := os.Stat(path); err == nil {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("timed out waiting for %s", path)
		}
		time.Sleep(5 * time.Millisecond)
	}
}

func (a *modelCommandApplyRaceAdapter) Pick1() error {
	if !a.gate.pass(a.req1 == "") {
		return nil
	}
	ctx, cancel := actionCtx()
	row, _, err := a.s.GetSession(ctx, a.id)
	cancel()
	if err != nil {
		return err
	}
	target := otherModel(row.Model)
	p, err := a.startPick(target)
	if err != nil {
		return err
	}
	a.req1, a.pend1 = target, p
	return nil
}

func (a *modelCommandApplyRaceAdapter) Pick2() error {
	if !a.gate.pass(a.req2 == "") {
		return nil
	}
	ctx, cancel := actionCtx()
	row, _, err := a.s.GetSession(ctx, a.id)
	cancel()
	if err != nil {
		return err
	}
	target := otherModel(row.Model)
	p, err := a.startPick(target)
	if err != nil {
		return err
	}
	a.req2, a.pend2 = target, p
	return nil
}

func (a *modelCommandApplyRaceAdapter) Deliver1() error {
	if !a.gate.pass(a.req1 != "") {
		return nil
	}
	target := a.pend1.target
	if err := releasePick(a.gateDir, a.pend1); err != nil {
		return err
	}
	a.req1, a.pend1 = "", nil
	return a.waitApplied(target)
}

func (a *modelCommandApplyRaceAdapter) Deliver2() error {
	if !a.gate.pass(a.req2 != "") {
		return nil
	}
	target := a.pend2.target
	if err := releasePick(a.gateDir, a.pend2); err != nil {
		return err
	}
	a.req2, a.pend2 = "", nil
	return a.waitApplied(target)
}

func (a *modelCommandApplyRaceAdapter) Prompt() error {
	if !a.gate.pass(a.held == "") {
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
	_, err := waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
	return err
}

func (a *modelCommandApplyRaceAdapter) Finish() error {
	if !a.gate.pass(a.held != "") {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	if _, err := waitRow(a.s, a.id, "done", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, lines, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return err
	}
	if a.wrongFinish {
		a.turnModel = "m1"
	} else {
		a.turnModel = appliedModel(lines)
	}
	return nil
}

var modelCommandApplyRaceActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Pick1":    action((*modelCommandApplyRaceAdapter).Pick1),
	"Pick2":    action((*modelCommandApplyRaceAdapter).Pick2),
	"Deliver1": action((*modelCommandApplyRaceAdapter).Deliver1),
	"Deliver2": action((*modelCommandApplyRaceAdapter).Deliver2),
	"Prompt":   action((*modelCommandApplyRaceAdapter).Prompt),
	"Finish":   action((*modelCommandApplyRaceAdapter).Finish),
}}

func modelCommandApplyRaceOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

// modelCommandApplyRaceHistory reads the abstract trace off a
// transcript on status alone: an "input" opens a turn (Prompt), its
// "done" closes it (Finish). Pick1/Pick2/Deliver1/Deliver2 leave no
// status change (model and applied are not in this projection's
// State), so they are not distinguishable after the fact and are left
// out, the same way the worked example leaves out View/Leave.
func modelCommandApplyRaceHistory(entries []history.Entry) []tracecheck.Step {
	status := func(s string) map[string]any { return map[string]any{"Session#0.status": s} }
	steps := []tracecheck.Step{{Action: "Init", State: status("idle")}}
	for _, e := range entries {
		switch e.Kind {
		case "input":
			steps = append(steps, tracecheck.Step{Action: "Session#0.Prompt", State: status("running")})
		case "done":
			steps = append(steps, tracecheck.Step{Action: "Session#0.Finish", State: status("idle")})
		}
	}
	return steps
}

func init() { historyProjections["model_command_apply_race"] = modelCommandApplyRaceHistory }

func TestModelCommandApplyRace(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newModelCommandApplyRaceAdapter(t)
	if err := runMBT(t, "model_command_apply_race", a, modelCommandApplyRaceActions, modelCommandApplyRaceOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "model_command_apply_race"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), modelCommandApplyRaceHistory)
	}
}

// The run above proves nothing unless a server that breaks the model
// fails it: this adapter reports every finished turn as attributed to
// "m1" no matter what actually ran, the kind of wiring bug a flow's
// adapter can have.
func TestModelCommandApplyRaceCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newModelCommandApplyRaceAdapter(t)
	a.wrongFinish = true
	if err := runMBT(t, "model_command_apply_race", a, modelCommandApplyRaceActions, modelCommandApplyRaceOptions()); err == nil {
		t.Fatal("a run whose Finish always attributes m1 passed; the runner is not checking state")
	}
}
