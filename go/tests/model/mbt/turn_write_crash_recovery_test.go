//go:build !windows

package mbt

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/turn_write_crash_recovery.fizz against a real serve: one
// session's history.jsonl, its child and the recovery OpenExisting owes
// a torn last line. The one role is HistoryFile, not a Session, because
// the field under test (torn) belongs to the file, not the page.
//
// "loaded" is self-tracked: the adapter is the only thing that ever
// crashes or reopens the file, so it knows without asking. "torn" is
// read off the real file's bytes every time, which is what actually
// proves OpenExisting recovered it: a wrong adapter that skips tearing
// the file, or skips calling the real OpenExisting, shows up as a
// mismatch there.

const twcFragment = `{"kind":"twc-crash-fragment","data":{"text":"cut`

type turnWriteCrashRecoveryAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	id, cwd string
	n       int
	entries int
	loaded  bool

	ids []string

	// skipTear is TestTurnWriteCrashRecoveryCatchesWrongAdapter's bug:
	// CrashMidAppend kills the child but never actually tears the file.
	skipTear bool
}

func newTurnWriteCrashRecoveryAdapter(t *testing.T) *turnWriteCrashRecoveryAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &turnWriteCrashRecoveryAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

func (a *turnWriteCrashRecoveryAdapter) path() string {
	return filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl")
}

// killCwd (SIGKILLs the bough process working in cwd, a crash so
// nothing in flight reaches the file) is shared with
// history_io_failure_test.go.

// Init starts each walk on a fresh idle session in the same serve: no
// child, no history file yet, which is what the spec's Init means by
// entries=0, torn=False, loaded=True.
func (a *turnWriteCrashRecoveryAdapter) Init() error {
	a.gate.reset()
	if a.cwd != "" {
		killCwd(a.cwd)
	}
	a.n++
	a.cwd = a.s.Dir(a.t, fmt.Sprintf("w%d", a.n))
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.cwd, "")
	if err != nil {
		return err
	}
	a.id, a.entries, a.loaded = row.ID, 0, true
	a.ids = append(a.ids, row.ID)
	return nil
}

// Cleanup kills a walk's child so it cannot reach the next walk's file.
func (a *turnWriteCrashRecoveryAdapter) Cleanup() error {
	if a.cwd != "" {
		killCwd(a.cwd)
		a.cwd = ""
	}
	return nil
}

func (a *turnWriteCrashRecoveryAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "HistoryFile", Index: 0}: a}, nil
}

// currentlyTorn reads the file's last byte off disk: True when the file
// ends without a trailing newline, the shape a write cut mid-line
// leaves. A file that does not exist yet (no child has ever started)
// is not torn.
func (a *turnWriteCrashRecoveryAdapter) currentlyTorn() (bool, error) {
	b, err := os.ReadFile(a.path())
	if errors.Is(err, os.ErrNotExist) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	return len(b) > 0 && b[len(b)-1] != '\n', nil
}

func (a *turnWriteCrashRecoveryAdapter) GetState() (map[string]any, error) {
	torn, err := a.currentlyTorn()
	if err != nil {
		return nil, err
	}
	return map[string]any{"entries": a.entries, "torn": torn, "loaded": a.loaded}, nil
}

// Append is one whole turn: an input and its done, through a real
// serve child. The spec's require is loaded and not torn.
func (a *turnWriteCrashRecoveryAdapter) Append() error {
	torn, err := a.currentlyTorn()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.loaded && !torn && a.entries < 2) {
		return nil
	}
	a.n++
	name := fmt.Sprintf("t%05d", a.n)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "ok", Text: "finished " + name})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, "turn "+name); err != nil {
		return err
	}
	if _, err := waitRow(a.s, a.id, "the turn to finish", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		return err
	}
	a.entries++
	return nil
}

// CrashMidAppend kills the child (a real SIGKILL, so nothing pending
// reaches the file) and, as a real crash mid-write does, leaves the
// file ending in a fragment with no trailing newline.
func (a *turnWriteCrashRecoveryAdapter) CrashMidAppend() error {
	torn, err := a.currentlyTorn()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.loaded && !torn) {
		return nil
	}
	killCwd(a.cwd)
	if _, err := waitRow(a.s, a.id, "the child to die", func(r serve.Row) bool { return !r.Live }); err != nil {
		return err
	}
	if !a.skipTear {
		b, err := os.ReadFile(a.path())
		if err != nil {
			return err
		}
		if err := os.WriteFile(a.path(), append(b, twcFragment...), 0o644); err != nil {
			return err
		}
	}
	a.loaded = false
	return nil
}

// Reopen calls the real OpenExisting (the function under test) on the
// file directly: it must recover (readEntries skips the torn line,
// dropTornTail truncates it) rather than fail the session. The next
// real Append then proves serve's own resume path reopens the same
// file the same way.
func (a *turnWriteCrashRecoveryAdapter) Reopen() error {
	if !a.gate.pass(!a.loaded) {
		return nil
	}
	st, err := history.OpenExisting(a.path())
	if err != nil {
		return err
	}
	if err := st.Close(); err != nil {
		return err
	}
	a.loaded = true
	return nil
}

func turnWriteCrashRecoveryAction(name string, f func(*turnWriteCrashRecoveryAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) { return nil, f(m.(*turnWriteCrashRecoveryAdapter)) }
}

var turnWriteCrashRecoveryActions = map[string]map[string]fmbt.ActionFunc{"HistoryFile": {
	"Append":         turnWriteCrashRecoveryAction("Append", (*turnWriteCrashRecoveryAdapter).Append),
	"CrashMidAppend": turnWriteCrashRecoveryAction("CrashMidAppend", (*turnWriteCrashRecoveryAdapter).CrashMidAppend),
	"Reopen":         turnWriteCrashRecoveryAction("Reopen", (*turnWriteCrashRecoveryAdapter).Reopen),
}}

// Every step is a real turn, kill or open against a real file, so a
// walk of 8 already exercises every transition a few times over.
func turnWriteCrashRecoveryOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

// turnWriteCrashRecoveryHistory reads the abstract trace off a
// transcript: CrashMidAppend and Reopen leave nothing in the file (a
// crash-then-recover cycle is, from the file's own view, a no-op back
// to the same state), so every "done" is one Append.
func turnWriteCrashRecoveryHistory(entries []history.Entry) []tracecheck.Step {
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{
		"HistoryFile#0.entries": 0, "HistoryFile#0.torn": false, "HistoryFile#0.loaded": true,
	}}}
	n := 0
	for _, e := range entries {
		if e.Kind == "done" {
			n++
			steps = append(steps, tracecheck.Step{Action: "HistoryFile#0.Append", State: map[string]any{"HistoryFile#0.entries": n}})
		}
	}
	return steps
}

func init() {
	historyProjections["turn_write_crash_recovery"] = turnWriteCrashRecoveryHistory
}

func checkTurnWriteCrashRecoveryHistories(t *testing.T, a *turnWriteCrashRecoveryAdapter) {
	t.Helper()
	a.Cleanup()
	g, err := tracecheck.Load(fizzCheck(t, "turn_write_crash_recovery"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), turnWriteCrashRecoveryHistory)
	}
}

// walkTurnWriteCrashRecoveryPaths walks every generated path against
// one serve; stopFirst ends the run at the first divergence, which the
// wrong-adapter check needs.
func walkTurnWriteCrashRecoveryPaths(t *testing.T, a *turnWriteCrashRecoveryAdapter, cover tracecheck.Cover, stopFirst bool) error {
	t.Helper()
	b, err := pathsJSONCover("turn_write_crash_recovery", cover)
	if err != nil {
		return err
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		return err
	}
	var errs []error
	for i, p := range doc.Paths {
		if err := a.walk(p.Trace); err != nil {
			errs = append(errs, fmt.Errorf("path %d: %w", i, err))
			if stopFirst {
				break
			}
		}
	}
	return errors.Join(errs...)
}

func (a *turnWriteCrashRecoveryAdapter) walk(trace []tracecheck.Step) error {
	for j, s := range trace {
		name := strings.TrimPrefix(s.Action, "HistoryFile#0.")
		var err error
		if s.Action == "Init" {
			err = a.Init()
		} else {
			_, err = turnWriteCrashRecoveryActions["HistoryFile"][name](a, nil)
			if err == nil && a.gate.off {
				err = errors.New("the adapter found it disabled")
			}
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", j, name, err)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w", j, name, err)
		}
		var diff []string
		for k, v := range s.State {
			f, ok := strings.CutPrefix(k, "HistoryFile#0.")
			if ok && fmt.Sprint(got[f]) != fmt.Sprint(v) {
				diff = append(diff, fmt.Sprintf("%s: spec %v, got %v", f, v, got[f]))
			}
		}
		if len(diff) > 0 {
			return fmt.Errorf("step %d (%s): %s", j, name, strings.Join(diff, "; "))
		}
	}
	return nil
}

// TestTurnWriteCrashRecovery lets fizzbee-mbt walk the spec at random
// (the exhaustive run only; see runMBT).
func TestTurnWriteCrashRecovery(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newTurnWriteCrashRecoveryAdapter(t)
	if err := runMBT(t, "turn_write_crash_recovery", a, turnWriteCrashRecoveryActions, turnWriteCrashRecoveryOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkTurnWriteCrashRecoveryHistories(t, a)
}

// TestTurnWriteCrashRecoveryPaths walks the generated paths against one
// serve: the default run, not gated on MODEL_COVER.
func TestTurnWriteCrashRecoveryPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newTurnWriteCrashRecoveryAdapter(t)
	if err := walkTurnWriteCrashRecoveryPaths(t, a, envCover(), false); err != nil {
		t.Fatalf("spec path: %v", err)
	}
	checkTurnWriteCrashRecoveryHistories(t, a)
}

// The projection is a check only if a transcript the model forbids is
// refused: two appends in a row is fine, but an Append the spec says
// must bump entries claiming it did not is not.
func TestTurnWriteCrashRecoveryHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(fizzCheck(t, "turn_write_crash_recovery"))
	if err != nil {
		t.Fatal(err)
	}
	ok := []history.Entry{{Kind: "meta"}, {Kind: "input"}, {Kind: "done"}, {Kind: "input"}, {Kind: "done"}}
	if v := g.Check(turnWriteCrashRecoveryHistory(ok)); v != nil {
		t.Fatalf("two appends: %v", v)
	}
	bad := []tracecheck.Step{
		{Action: "Init", State: map[string]any{"HistoryFile#0.entries": 0, "HistoryFile#0.torn": false, "HistoryFile#0.loaded": true}},
		{Action: "HistoryFile#0.Append", State: map[string]any{"HistoryFile#0.entries": 0}},
	}
	if v := g.Check(bad); v == nil {
		t.Fatal("an Append that claims it did not bump entries passed the trace check")
	} else {
		t.Logf("refused as expected: %v", v)
	}
}

// A run whose CrashMidAppend never tears the file must fail, or a
// green TestTurnWriteCrashRecoveryPaths proves nothing: the file would
// stay clean, and Reopen's real OpenExisting recovery is never tested.
func TestTurnWriteCrashRecoveryCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newTurnWriteCrashRecoveryAdapter(t)
	a.skipTear = true
	err := walkTurnWriteCrashRecoveryPaths(t, a, tracecheck.CoverTransitions, true)
	if err == nil {
		t.Fatal("a run whose CrashMidAppend never tears the file passed; the paths are not checking state")
	}
	t.Logf("caught as expected: %v", err)
}
