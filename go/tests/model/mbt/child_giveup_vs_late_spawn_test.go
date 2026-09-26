//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/child_giveup_vs_late_spawn.fizz against a real serve: a client
// gives up on a background-agent create (children_api.go's createChild,
// children.go's CreateChild/WithdrawChild) at any point up to serve's
// post-make recheck, and the async spawn (the child's own process
// reaching a point past its own boot) races the give-up independently.
//
// The two hold points the product exposes for tests drive the race:
//   - BOUGH_TEST_CREATE_HOLD_DIR (children_api.go's holdCreate) parks the
//     request itself before Make (create.hold) and after it, before
//     serve's recheck (respond.hold); its ".held" files announce what
//     stage it is parked at and, at "respond", the id it made.
//   - llm-control's hold_boot parks the child's own process before it
//     writes its history file — the "async spawn" completing (Boot) is
//     this test releasing that hold, which can happen before or after
//     the give-up has already withdrawn the id (LateBoot).
//
// Make's gate also requires !given_up: the spec lets Make follow a
// GiveUp unconditionally (an over-approximation for the checker), but
// the product's pre-Make check (children_api.go) means a give-up already
// landed before create.hold is even released stops CreateChild from
// ever running — there is truly nothing to make. That half of the spec's
// state space is a disabled action here, same as any other precondition
// the product enforces beyond the spec's literal text.

// childGiveupConfig holds every fresh session (the child a create
// spawns) at boot, before its history file exists, so this adapter
// decides exactly when the async part of a spawn "finishes".
const childGiveupConfig = "- id: llm\n  plugin: llm-control\n  config:\n    hold_boot: true\n"

type cgResult struct {
	id     string
	queued bool
	err    error
}

type childGiveupAdapter struct {
	t     *testing.T
	s     *servetest.Server
	dir   string // llm-control's dir (hold_boot's handshake)
	holds string // BOUGH_TEST_CREATE_HOLD_DIR
	cwd   string
	gate  gate

	parent string // a history file with no process, as background_agent_launch_failure's does

	cancel   context.CancelFunc
	done     chan cgResult
	finished bool
	result   cgResult
	id       string // this walk's child, learned off respond.held

	retryID string

	givenUp, made, checked, booted, withdrawn, live, retried, retryLive bool

	// noCancel is the deliberate bug TestChildGiveupVsLateSpawnCatchesWrongAdapter
	// injects: GiveUp claims the client gave up without actually cancelling
	// the request, so the product never sees it.
	noCancel bool
}

func newChildGiveupAdapter(t *testing.T) *childGiveupAdapter {
	holds := t.TempDir()
	s := servetest.Start(t, servetest.Options{
		Config: childGiveupConfig,
		Env:    []string{"BOUGH_TEST_CREATE_HOLD_DIR=" + holds},
	})
	return &childGiveupAdapter{t: t, s: s, dir: control.Dir(s.Home), holds: holds, cwd: s.Dir(t, "work")}
}

// Init writes a fresh parent with no process (as blfAdapter's does), so
// CreateChild has a parent to look up without that session's own boot
// hold ever getting in the way, and arms both of createChild's hold
// points for the request Init starts at once.
func (a *childGiveupAdapter) Init() error {
	id := history.NewID()
	path := a.histPath(id)
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(path, nil, 0o644); err != nil {
		return err
	}
	if _, err := history.AppendFile(path, "meta", map[string]any{"cwd": a.cwd}); err != nil {
		return err
	}
	a.parent = id
	for _, f := range []string{"create.hold", "create.held", "respond.hold", "respond.held"} {
		os.Remove(filepath.Join(a.holds, f))
	}
	if err := os.WriteFile(filepath.Join(a.holds, "create.hold"), nil, 0o644); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(a.holds, "respond.hold"), nil, 0o644); err != nil {
		return err
	}
	a.givenUp, a.made, a.checked, a.booted, a.withdrawn, a.live, a.retried, a.retryLive = false, false, false, false, false, false, false, false
	a.id, a.retryID, a.finished, a.result = "", "", false, cgResult{}
	ctx, cancel := context.WithCancel(context.Background())
	a.cancel = cancel
	a.done = make(chan cgResult, 1)
	go func() {
		row, queued, err := a.s.CreateAgent(ctx, a.parent, "", 0, 0)
		a.done <- cgResult{row.ID, queued, err}
	}()
	a.gate.reset()
	return nil
}

func (a *childGiveupAdapter) checkDone() {
	if a.finished {
		return
	}
	select {
	case r := <-a.done:
		a.finished, a.result = true, r
	default:
	}
}

func (a *childGiveupAdapter) histPath(id string) string {
	return filepath.Join(a.s.Home, ".bough", "history", id+".jsonl")
}

func (a *childGiveupAdapter) GetState() map[string]any {
	a.checkDone()
	return map[string]any{
		"given_up": a.givenUp, "made": a.made, "checked": a.checked,
		"booted": a.booted, "withdrawn": a.withdrawn, "live": a.live,
		"retried": a.retried, "retry_live": a.retryLive,
	}
}

// GiveUp is the client's serveTimeout or Esc: it cancels the create
// request in flight, at any point before Check has run.
func (a *childGiveupAdapter) GiveUp() error {
	if !a.gate.pass(!a.givenUp && !a.checked) {
		return nil
	}
	a.givenUp = true
	if !a.noCancel {
		a.cancel()
	}
	return nil
}

// Make releases create.hold, letting createChild reserve the slot
// (children.go's CreateChild), and waits for the id it made off
// respond.held. Once given up, the product's own pre-Make check means
// this can never happen — see the header — so a give-up already seen
// disables it here too.
func (a *childGiveupAdapter) Make() error {
	a.checkDone()
	if !a.gate.pass(!a.made && !a.givenUp) {
		return nil
	}
	if err := os.Remove(filepath.Join(a.holds, "create.hold")); err != nil {
		return fmt.Errorf("Make: %w", err)
	}
	deadline := time.Now().Add(actionTimeout)
	for {
		if b, err := os.ReadFile(filepath.Join(a.holds, "respond.held")); err == nil && len(b) > 0 {
			a.id, a.made = string(b), true
			return nil
		}
		a.checkDone()
		if a.finished {
			return fmt.Errorf("Make: the create request settled without ever reserving a child")
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("Make: CreateChild did not reserve within %s", actionTimeout)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

// Boot is the async spawn (the child's own process) reaching past its
// own boot hold and writing its history file, independent of whether
// serve has rechecked yet.
func (a *childGiveupAdapter) Boot() error {
	if !a.gate.pass(a.made && !a.booted && !a.withdrawn) {
		return nil
	}
	control.WaitBooting(a.t, a.dir, a.id, actionTimeout)
	control.ReleaseBoot(a.t, a.dir, a.id)
	deadline := time.Now().Add(actionTimeout)
	for {
		if _, err := os.Stat(a.histPath(a.id)); err == nil {
			a.booted, a.live = true, true
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("Boot: %s never wrote its history after release", a.id)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

// Check is serve's post-make recheck of ctx.Err(): given up, it
// withdraws the reservation (children.go's WithdrawChild), which kills
// whatever already booted and waits for the reap before returning, so
// nothing is left alive or with a history file under the id.
func (a *childGiveupAdapter) Check() error {
	if !a.gate.pass(a.made && !a.checked) {
		return nil
	}
	if err := os.Remove(filepath.Join(a.holds, "respond.hold")); err != nil {
		return fmt.Errorf("Check: %w", err)
	}
	deadline := time.Now().Add(actionTimeout)
	for !a.finished {
		a.checkDone()
		if time.Now().After(deadline) {
			return fmt.Errorf("Check: the create request did not settle within %s", actionTimeout)
		}
		time.Sleep(10 * time.Millisecond)
	}
	a.checked = true
	if !a.givenUp {
		if a.result.err != nil {
			return fmt.Errorf("Check: create answered with an error though nobody gave up: %w", a.result.err)
		}
		return nil
	}
	a.withdrawn, a.live = true, false
	if pid := control.BootPID(a.dir, a.id); pid != 0 && alive(pid) {
		return fmt.Errorf("Check: withdrawn child %s is still alive (pid %d)", a.id, pid)
	}
	if _, err := os.Stat(a.histPath(a.id)); err == nil {
		return fmt.Errorf("Check: withdrawn child %s left a history file behind", a.id)
	}
	return nil
}

// LateBoot is the race the flow is named for: the async spawn (Boot)
// arrives after Check already withdrew. Correct behaviour tears the
// late arrival down too — WithdrawChild already killed it, so letting
// it past its boot hold now must start nothing.
func (a *childGiveupAdapter) LateBoot() error {
	if !a.gate.pass(a.made && !a.booted && a.withdrawn) {
		return nil
	}
	if pid := control.BootPID(a.dir, a.id); pid != 0 && alive(pid) {
		return fmt.Errorf("LateBoot: withdrawn child %s is alive (pid %d) before its late release", a.id, pid)
	}
	if _, ok := control.Booting(a.dir)[a.id]; ok {
		control.ReleaseBoot(a.t, a.dir, a.id) // a no-op: nothing is left to read it
	}
	a.booted, a.live = true, false
	if _, err := os.Stat(a.histPath(a.id)); err == nil {
		return fmt.Errorf("LateBoot: withdrawn child %s wrote history after being killed", a.id)
	}
	return nil
}

// Retry is the client trying again once told the create failed: a
// second, ordinary create for the same parent.
func (a *childGiveupAdapter) Retry() error {
	if !a.gate.pass(a.checked && a.withdrawn && !a.retried) {
		return nil
	}
	ctx, cancel := actionCtx()
	row, queued, err := a.s.CreateAgent(ctx, a.parent, "", 0, 0)
	cancel()
	if err != nil {
		return fmt.Errorf("Retry: %w", err)
	}
	if queued {
		return fmt.Errorf("Retry: unexpectedly queued")
	}
	a.retryID, a.retried = row.ID, true
	control.WaitBooting(a.t, a.dir, a.retryID, actionTimeout)
	control.ReleaseBoot(a.t, a.dir, a.retryID)
	deadline := time.Now().Add(actionTimeout)
	for {
		if _, err := os.Stat(a.histPath(a.retryID)); err == nil {
			a.retryLive = true
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("Retry: %s never wrote its history after release", a.retryID)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

// Cleanup unblocks whatever the walk left holding a stage or a boot
// hold and stops any child it started, so nothing bleeds into the next
// walk's Init on the same serve.
func (a *childGiveupAdapter) Cleanup() error {
	var errs []error
	os.Remove(filepath.Join(a.holds, "create.hold"))
	os.Remove(filepath.Join(a.holds, "respond.hold"))
	if a.cancel != nil {
		a.cancel()
	}
	if a.done != nil && !a.finished {
		select {
		case r := <-a.done:
			a.finished, a.result = true, r
		case <-time.After(actionTimeout):
			errs = append(errs, fmt.Errorf("Cleanup: the create request never settled"))
		}
	}
	for _, id := range []string{a.id, a.retryID} {
		if id == "" {
			continue
		}
		if _, ok := control.Booting(a.dir)[id]; ok {
			control.ReleaseBoot(a.t, a.dir, id)
		}
		ctx, cancel := actionCtx()
		_, _ = a.s.Stop(ctx, id) // best-effort: withdrawn or already idle answers an error we do not care about
		cancel()
	}
	return errors.Join(errs...)
}

var childGiveupActions = map[string]func(*childGiveupAdapter) error{
	"GiveUp":   (*childGiveupAdapter).GiveUp,
	"Make":     (*childGiveupAdapter).Make,
	"Boot":     (*childGiveupAdapter).Boot,
	"Check":    (*childGiveupAdapter).Check,
	"LateBoot": (*childGiveupAdapter).LateBoot,
	"Retry":    (*childGiveupAdapter).Retry,
}

// walk runs one generated path, comparing the adapter's state with the
// spec's after every step, and reports how many of its steps were
// actually checked: Make's gate is stricter than the spec's literal
// text (see its comment), so a walk whose GiveUp precedes its Make ends
// there, checked, with the rest of it never a path the real system can
// walk — the same as any other disabled action (README.md).
func (a *childGiveupAdapter) walk(trace []tracecheck.Step) (checked int, err error) {
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	names := make([]string, len(trace))
	for j, s := range trace {
		names[j] = strings.TrimPrefix(s.Action, "Flow#0.")
	}
	for j, s := range trace {
		if s.Action == "Init" {
			err = a.Init()
		} else if names[j] == "end" {
			// deadlock_detection is off (specs/child_giveup_vs_late_spawn.fizz):
			// fizz links a settled state with nothing enabled to itself as
			// "end", offered everywhere; it changes nothing.
		} else if f, ok := childGiveupActions[names[j]]; ok {
			err = f(a)
		} else {
			err = fmt.Errorf("no adapter action for %s", s.Action)
		}
		if err != nil {
			return checked, fmt.Errorf("step %d (%s): %w", j, names[j], err)
		}
		if a.gate.off {
			return checked, nil
		}
		checked++
		got := a.GetState()
		var diff []string
		for k, v := range s.State {
			f, ok := strings.CutPrefix(k, "Flow#0.")
			if !ok {
				continue
			}
			if got[f] != v {
				diff = append(diff, fmt.Sprintf("%s is %v, the spec says %v", f, got[f], v))
			}
		}
		if len(diff) > 0 {
			sort.Strings(diff)
			return checked, fmt.Errorf("step %d (%s): %s", j, names[j], strings.Join(diff, "; "))
		}
	}
	return checked, nil
}

// childGiveupWalks is testdata/child_giveup_vs_late_spawn's walks for cover.
func childGiveupWalks(t *testing.T, cover tracecheck.Cover) [][]tracecheck.Step {
	t.Helper()
	b, err := pathsJSONCover("child_giveup_vs_late_spawn", cover)
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
	walks := make([][]tracecheck.Step, len(doc.Paths))
	for i, p := range doc.Paths {
		walks[i] = p.Trace
	}
	return walks
}

// TestChildGiveupVsLateSpawnPaths walks every generated walk (every
// settled state; every link under MODEL_COVER=transitions) against a
// real serve.
func TestChildGiveupVsLateSpawnPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newChildGiveupAdapter(t)
	walks := childGiveupWalks(t, envCover())
	total := 0
	for i, w := range walks {
		n, err := a.walk(w)
		total += n
		if err != nil {
			t.Errorf("walk %d: %v", i, err)
		}
		t.Logf("walk %d: %d/%d steps checked", i, n, len(w))
	}
	if total == 0 {
		t.Fatal("no walk checked a single step; the adapter's gate is disabling every action")
	}
}

// A run whose GiveUp does not actually cancel the request must fail, or
// a green TestChildGiveupVsLateSpawnPaths proves nothing: the wrong
// wiring shows once a give-up that happened after Make reaches Check,
// which only a walk of every link is sure to take.
func TestChildGiveupVsLateSpawnCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	var walk []tracecheck.Step
	for _, w := range childGiveupWalks(t, tracecheck.CoverTransitions) {
		madeAt, gaveUpAt := -1, -1
		for j, s := range w {
			switch s.Action {
			case "Flow#0.Make":
				madeAt = j
			case "Flow#0.GiveUp":
				gaveUpAt = j
			case "Flow#0.Check":
				if madeAt >= 0 && gaveUpAt > madeAt {
					walk = w[:j+1]
				}
			}
			if walk != nil {
				break
			}
		}
		if walk != nil {
			break
		}
	}
	if walk == nil {
		t.Fatal("no generated walk gives up after Make and reaches Check; the wrong-adapter bug cannot be exercised")
	}
	a := newChildGiveupAdapter(t)
	a.noCancel = true
	_, err := a.walk(walk)
	if err == nil {
		t.Fatal("a run whose GiveUp never cancels the request passed; the walk is not checking state")
	}
	t.Logf("caught as expected: %v", err)
}
