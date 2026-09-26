//go:build !windows

package mbt

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/servetest"
)

// specs/config_hot_reload_merge.fizz against a real serve: one session
// whose bough.yml is edited live while the file watcher (main.go's
// watchConfig -> reload -> Reconcile) is running, exactly the surface
// the home-config-only-row bug (5dd3daec) broke.
//
// The real rows behind the spec's three fields, all from the embedded
// default tree (go/bough.yml), are the one place this codebase has a
// genuine, non-core, mandatory two-hop dependency:
//
//		agent-tools (Inject nil, Provides "agent-tools")
//		  -> loop/engine-unreal (Inject ["history", "agent-tools"])
//
//	  - upstream is the "agent-tools" row: `disabled: true` in the
//	    session's own bough.yml withdraws it (UpstreamBreaks/Heals).
//	  - a and b are both read off the SAME row, "loop": the spec's
//	    DependentTracksProvider requires a=="mounted" iff b=="mounted" in
//	    every reachable state, which is trivially and exactly what one
//	    row's live State already gives us, and it is the row the fizz
//	    spec's own comment names ("the loop's own dependency"). No other
//	    shipped plugin mandatorily injects a non-core key (checked against
//	    every plugin's Inject()), so there is no second real row to stand
//	    in for a distinct "b" without inventing one.
//	  - a's own bad-edit lever is `turn_settle` on the loop row itself
//	    (plugins/engine/config.go): a duration string fails Apply
//	    (EditBadConfig -> Failed), a different valid one is a spec change
//	    that still mounts (EditGoodConfig).
//	  - override has no externally reachable lever for an arbitrary row —
//	    runtime --set only ever reaches a live child through /model
//	    (internal/serve/models.go -> Supervisor.SetModel), which swaps
//	    the llm row, not this one. SetOverride is therefore implemented as
//	    the same file edit as EditGoodConfig: it exercises the identical
//	    reconcile drop+settle path the spec says it must (a bare edit is
//	    "the same kind of edit as EditGoodConfig"), while the config
//	    source's own replay-across-reload bookkeeping (main.go's
//	    overrides.all()) lives in package main and is not reachable from
//	    here.
//	  - turnOpen/StartTurn/EndTurn are adapter bookkeeping only, not a
//	    real held turn: no assertion in the spec reads turnOpen, and
//	    routing a real request through the very rows an action is about
//	    to unmount (loop provides "inputs", which "ui" mandatorily
//	    injects — breaking loop cascades to the session's own stdin
//	    reader) would risk hanging a walk rather than testing anything
//	    these assertions check. The orthogonality the spec claims is
//	    proven the honest way here: the adapter never has to touch
//	    turnOpen to get UpstreamBreaks/Heals/Edit*Config right.
//
// The loop row's live state is not exposed over HTTP, so it is read the
// way the supervisor already surfaces it: reconcile.go's row-pending
// and row-FAILED lines are unconditional stderr (kernel/reconcile.go),
// the session's child's stderr becomes "error" events
// (internal/serve/supervisor.go pumpStderr/emit), and those are exactly
// what GET /api/sessions/{id}/events streams. observeLoop reads new
// events since the last check and keeps the newest verdict about the
// "loop" row; no matching line since the edit means it mounted clean.
type chrAdapter struct {
	t    *testing.T
	s    *servetest.Server
	gate gate

	walks int
	work  string
	id    string
	ids   []string

	upOK     bool   // agent-tools row enabled (spec: upstream == "ok")
	turnVal  string // loop row's turn_settle config
	override bool
	turnOpen bool

	lastTurnVal string // turnVal as of the last write (see applyEdit)

	row     string // cached last-observed state of the loop row
	lastSeq int64  // events already accounted for, this session
}

func newCHRAdapter(t *testing.T) *chrAdapter {
	s := servetest.Start(t, servetest.Options{Config: servetest.DefaultConfig})
	return &chrAdapter{t: t, s: s}
}

// chrSettle is comfortably clear of watchConfig's 300ms debounce plus
// one reconcile pass.
const chrSettle = 700 * time.Millisecond

// chrObserve bounds how long observeLoop waits for a stale/live event:
// short, because every edit here is local and reconcile is synchronous
// once the debounce fires.
const chrObserve = 900 * time.Millisecond

func (a *chrAdapter) writeConfig() error {
	cfg := fmt.Sprintf("- id: loop\n  plugin: engine-unreal\n  config:\n    turn_settle: %s\n", a.turnVal)
	if !a.upOK {
		cfg += "- id: agent-tools\n  disabled: true\n"
	}
	return os.WriteFile(filepath.Join(a.work, "bough.yml"), []byte(cfg), 0o644)
}

// observeLoop reads events newer than a.lastSeq and returns the newest
// verdict reconcile logged about the "loop" row, or "mounted" when
// nothing did.
func (a *chrAdapter) observeLoop() (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), chrObserve)
	defer cancel()
	st, err := a.s.Events(ctx, a.id)
	if err != nil {
		return "", fmt.Errorf("events: %w", err)
	}
	defer st.Close()
	state := ""
	for {
		ev, err := st.Next()
		if err != nil {
			break
		}
		if ev.Seq <= a.lastSeq {
			continue
		}
		a.lastSeq = ev.Seq
		if ev.Kind != "error" || !strings.Contains(ev.Text, `row "loop"`) {
			continue
		}
		switch {
		case strings.Contains(ev.Text, "FAILED"):
			state = "failed"
		case strings.Contains(ev.Text, "pending"):
			state = "pending"
		}
	}
	if state == "" {
		return "mounted", nil
	}
	return state, nil
}

// applyEdit writes the current config, waits it past the debounce, and
// refreshes a.row from what the server actually did.
//
// observeLoop's "mounted" default reads silence as "it mounted clean",
// which only holds when this edit could have moved the row: reconcile.go
// never retries a Failed row until its own spec changes (fail()'s
// comment), and unlike a Pending row — which settle() logs again on
// every single reconcile pass, whether or not anything changed — a
// Failed row is logged once, at the moment Apply errors, and then
// never again. So a Failed row that this edit's turn_settle value
// leaves untouched (UpstreamBreaks/Heals, or a repeated EditBadConfig)
// produces no new line, and silence has to be read as "still failed",
// not "mounted".
func (a *chrAdapter) applyEdit() error {
	specChanged := a.turnVal != a.lastTurnVal
	if err := a.writeConfig(); err != nil {
		return err
	}
	a.lastTurnVal = a.turnVal
	time.Sleep(chrSettle)
	st, err := a.observeLoop()
	if err != nil {
		return err
	}
	if st == "mounted" && a.row == "failed" && !specChanged {
		st = "failed"
	}
	a.row = st
	return nil
}

// Init starts each walk on a fresh session in its own directory (its
// own bough.yml is what every edit below rewrites) in the same serve.
func (a *chrAdapter) Init() error {
	a.walks++
	a.work = a.s.Dir(a.t, fmt.Sprintf("chr%04d", a.walks))
	a.upOK, a.turnVal, a.override, a.turnOpen = true, "5s", false, false
	a.row, a.lastSeq = "mounted", 0
	a.lastTurnVal = a.turnVal
	if err := a.writeConfig(); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.work, "")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, a.id)
	a.gate.reset()
	// Let the child mount before the first observation call.
	time.Sleep(chrSettle)
	if _, err := a.observeLoop(); err != nil {
		return err
	}
	return nil
}

// Cleanup archives the walk's session so a run does not leave a child
// per walk.
func (a *chrAdapter) Cleanup() error {
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Archive(ctx, a.id)
	return err
}

func (a *chrAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Config", Index: 0}: a}, nil
}

// GetState is the Config role's state, read straight off the adapter:
// a and b are both the loop row's cached state (see the type comment),
// upstream/override/turnOpen are the adapter's own bookkeeping of what
// it told the server to do.
func (a *chrAdapter) GetState() (map[string]any, error) {
	upstream := "ok"
	if !a.upOK {
		upstream = "missing"
	}
	return map[string]any{
		"a":        a.row,
		"b":        a.row,
		"upstream": upstream,
		"override": a.override,
		"turnOpen": a.turnOpen,
	}, nil
}

func (a *chrAdapter) StartTurn() error {
	if !a.gate.pass(!a.turnOpen) {
		return nil
	}
	a.turnOpen = true
	return nil
}

func (a *chrAdapter) EndTurn() error {
	if !a.gate.pass(a.turnOpen) {
		return nil
	}
	a.turnOpen = false
	return nil
}

func (a *chrAdapter) UpstreamBreaks() error {
	if !a.gate.pass(a.upOK) {
		return nil
	}
	a.upOK = false
	return a.applyEdit()
}

func (a *chrAdapter) UpstreamHeals() error {
	if !a.gate.pass(!a.upOK) {
		return nil
	}
	a.upOK = true
	return a.applyEdit()
}

// nextGoodTurnVal alternates between two valid turn_settle durations,
// so every good edit is a genuine spec change (reconcile.go drops a row
// only when its composed spec actually differs).
func (a *chrAdapter) nextGoodTurnVal() string {
	if a.turnVal == "5s" {
		return "6s"
	}
	return "5s"
}

func (a *chrAdapter) EditGoodConfig() error {
	a.turnVal = a.nextGoodTurnVal()
	return a.applyEdit()
}

func (a *chrAdapter) EditBadConfig() error {
	if !a.gate.pass(a.upOK) {
		return nil
	}
	a.turnVal = "bad"
	return a.applyEdit()
}

func (a *chrAdapter) SetOverride() error {
	if !a.gate.pass(!a.override) {
		return nil
	}
	a.override = true
	// See the type comment: no row here has a runtime --set lever, so
	// this exercises the same reconcile path EditGoodConfig does.
	a.turnVal = a.nextGoodTurnVal()
	return a.applyEdit()
}

var chrActions = map[string]map[string]fmbt.ActionFunc{"Config": {
	"StartTurn":      action((*chrAdapter).StartTurn),
	"EndTurn":        action((*chrAdapter).EndTurn),
	"UpstreamBreaks": action((*chrAdapter).UpstreamBreaks),
	"UpstreamHeals":  action((*chrAdapter).UpstreamHeals),
	"EditGoodConfig": action((*chrAdapter).EditGoodConfig),
	"EditBadConfig":  action((*chrAdapter).EditBadConfig),
	"SetOverride":    action((*chrAdapter).SetOverride),
}}

// chrOptions keeps each walk short: every action but StartTurn/EndTurn
// costs a debounce wait and an events round trip against a real serve.
func chrOptions() map[string]any {
	return map[string]any{"max-seq-runs": 20, "max-actions": 8, "max-parallel-runs": 0}
}

func TestConfigHotReloadMerge(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newCHRAdapter(t)
	if err := runMBT(t, "config_hot_reload_merge", a, chrActions, chrOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// The run above proves nothing unless a server that breaks the model
// fails it. This adapter reports "mounted" no matter what reconcile
// actually logged, the kind of wiring bug a flow's adapter can have,
// and the run must say so.
func TestConfigHotReloadMergeCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newCHRAdapter(t)
	wrong := map[string]map[string]fmbt.ActionFunc{"Config": {
		"StartTurn": action((*chrAdapter).StartTurn),
		"EndTurn":   action((*chrAdapter).EndTurn),
		"UpstreamBreaks": action(func(a *chrAdapter) error {
			if !a.gate.pass(a.upOK) {
				return nil
			}
			a.upOK = false
			if err := a.writeConfig(); err != nil {
				return err
			}
			time.Sleep(chrSettle)
			// Deliberately skip observeLoop: a.row is left "mounted".
			return nil
		}),
		"UpstreamHeals":  action((*chrAdapter).UpstreamHeals),
		"EditGoodConfig": action((*chrAdapter).EditGoodConfig),
		"EditBadConfig":  action((*chrAdapter).EditBadConfig),
		"SetOverride":    action((*chrAdapter).SetOverride),
	}}
	if err := runMBT(t, "config_hot_reload_merge", a, wrong, chrOptions()); err == nil {
		t.Fatal("a run whose UpstreamBreaks never reads the server passed; the runner is not checking state")
	}
}
