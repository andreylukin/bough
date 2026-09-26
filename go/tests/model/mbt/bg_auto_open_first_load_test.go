//go:build !windows

package mbt

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
)

// specs/bg_auto_open_first_load.fizz: the sidebar's one-time auto-open
// of the background group, raced across two tabs of the same origin
// sharing one localStorage flag ("bough:bg-auto").
//
// The adapter plays app.tsx's mount effect (go/internal/serve/web/src/
// app.tsx, the Sidebar mount effect around bgRunning/bgFailed and the
// "bough:bg-auto" read-then-set) directly: ReadFlag is the effect's
// synchronous read of the shared flag, MaybeOpen the write-then-open
// that follows it. Nothing here drives the DOM or React — the race is
// in the read-then-write pair itself, which two Go structs sharing one
// int reproduce exactly; the browser step (not part of this flow's Go
// MBT test) is what would additionally prove that pair is wired to the
// real localStorage and the real effect.
//
// A real serve backs the walk anyway, seeded in Init with one
// background session already failed, so the condition ReadFlag and
// MaybeOpen are gating on ("background work present") is the same real
// condition app.tsx's effect guards on, not an assumption the test
// invents. This spec has no model turns and writes nothing to any
// session's transcript, so there is no history projection to register.

// bgAdapter is the fmbt.Model; its two Tab roles are bgTab structs
// sharing the adapter's flag, the one piece of shared state that is not
// itself a role field (fizzbee-mbt compares only role state).
type bgAdapter struct {
	t    *testing.T
	s    *servetest.Server
	gate gate

	flag int // the shared "bough:bg-auto" localStorage value: 0 or 1
	a, b *bgTab
}

// bgTab is one Tab role instance: a browser tab's own view of the race,
// exactly the fields the spec's Tab role carries.
type bgTab struct {
	owner  *bgAdapter
	read   bool
	wrote  bool
	saw    int // this tab's ReadFlag snapshot of the shared flag
	opened bool
}

func newBGAdapter(t *testing.T) *bgAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &bgAdapter{t: t, s: s}
	a.a = &bgTab{owner: a}
	a.b = &bgTab{owner: a}
	return a
}

// Init seeds one background session already failed (origin "headless",
// like a wiki ingest, closed with an "error" entry inside its last
// turn), so bgRunning||bgFailed is true from both tabs' first render,
// the way the spec assumes throughout, and resets the race's state for
// a fresh pair of tabs.
func (a *bgAdapter) Init() error {
	if err := a.seedFailedBackground(); err != nil {
		return err
	}
	a.flag = 0
	a.a.reset()
	a.b.reset()
	a.gate.reset()
	return nil
}

func (a *bgAdapter) Cleanup() error { return nil }

// seedFailedBackground writes a history file the way TestRowMarksBackground
// (internal/serve/origin_test.go) does: origin "headless" makes the row
// background, and a turn that closes with an "error" entry inside it
// makes its status "error" — the failure hasFailure (web/src/status.tsx)
// looks for. serve derives status and background from the file on every
// read, so nothing more is needed to make it a background failure.
func (a *bgAdapter) seedFailedBackground() error {
	now := time.Now()
	entries := []history.Entry{
		{Kind: "meta", Data: map[string]any{"cwd": a.s.Home, "origin": "headless"}},
		{Kind: "input", Data: map[string]any{"text": "/llm-wiki ingest x"}},
		{Kind: "error", Data: map[string]any{"error": "boom"}},
		{Kind: "done", Data: map[string]any{}},
	}
	var buf bytes.Buffer
	for i, e := range entries {
		e.Seq, e.At = int64(i+1), now
		b, err := json.Marshal(e)
		if err != nil {
			return err
		}
		buf.Write(append(b, '\n'))
	}
	dir := filepath.Join(a.s.Home, ".bough", "history")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	id := history.NewID()
	tmp := filepath.Join(dir, id+".seed")
	if err := os.WriteFile(tmp, buf.Bytes(), 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, filepath.Join(dir, id+".jsonl"))
}

func (a *bgAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{
		{RoleName: "Tab", Index: 0}: a.a,
		{RoleName: "Tab", Index: 1}: a.b,
	}, nil
}

// GetState is the spec's global state, which holds only the two roles.
func (a *bgAdapter) GetState() (map[string]any, error) { return map[string]any{}, nil }

func (tab *bgTab) reset() { tab.read, tab.wrote, tab.saw, tab.opened = false, false, 0, false }

// GetState is one tab's Tab role state: read, wrote, opened, exactly as
// the spec's role fields are named. saw only exists once ReadFlag has
// assigned it, exactly as the spec's role never sets self.saw before
// then — reporting it early would claim a field the graph's Init node
// does not have.
func (tab *bgTab) GetState() (map[string]any, error) {
	state := map[string]any{"read": tab.read, "wrote": tab.wrote, "opened": tab.opened}
	if tab.read {
		state["saw"] = tab.saw
	}
	return state, nil
}

// ReadFlag: `self.saw = flag`, the effect's snapshot of the shared
// localStorage value before it decides whether to write.
func (tab *bgTab) ReadFlag() error {
	if !tab.owner.gate.pass(!tab.read) {
		return nil
	}
	tab.read = true
	tab.saw = tab.owner.flag
	return nil
}

// MaybeOpen: `if (flag === "1") return; localStorage.setItem(...)` — a
// tab writes and opens locally only when it saw the flag unset.
func (tab *bgTab) MaybeOpen() error {
	if !tab.owner.gate.pass(tab.read && !tab.wrote) {
		return nil
	}
	tab.wrote = true
	if tab.saw == 0 {
		tab.owner.flag = 1
		tab.opened = true
	}
	return nil
}

var bgAutoOpenFirstLoadActions = map[string]map[string]fmbt.ActionFunc{"Tab": {
	"ReadFlag":  action((*bgTab).ReadFlag),
	"MaybeOpen": action((*bgTab).MaybeOpen),
}, "": {
	// deadlock_detection is off (both tabs settled is rest, not a
	// deadlock), so fizz links a state with nothing enabled to itself as
	// a role-less "end"; the runner offers it everywhere, and a missing
	// entry is a nil-pointer panic in the library, not a test failure.
	"end": func(any, []fmbt.Arg) (any, error) { return nil, nil },
}}

func bgAutoOpenFirstLoadOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 6, "max-parallel-runs": 0}
}

func TestBgAutoOpenFirstLoad(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newBGAdapter(t)
	if err := runMBT(t, "bg_auto_open_first_load", a, bgAutoOpenFirstLoadActions, bgAutoOpenFirstLoadOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// The run above proves nothing unless a server that breaks the model
// fails it. This deliberate bug — ReadFlag itself opens the group, the
// way a stray `setUnfolded(...)` on the read path rather than the write
// path would — is the kind of wrong wiring a flow's adapter can have:
// every walk reports `opened: true` after its very first step, where
// the graph says `false`, so the run must say so at once.
func TestBgAutoOpenFirstLoadCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newBGAdapter(t)
	actions := map[string]map[string]fmbt.ActionFunc{"Tab": {
		"ReadFlag": action(func(tab *bgTab) error {
			if !tab.owner.gate.pass(!tab.read) {
				return nil
			}
			tab.read = true
			tab.saw = tab.owner.flag
			tab.opened = true // wrong: ReadFlag never opens; only MaybeOpen does
			return nil
		}),
		"MaybeOpen": action((*bgTab).MaybeOpen),
	}, "": {
		"end": func(any, []fmbt.Arg) (any, error) { return nil, nil },
	}}
	if err := runMBT(t, "bg_auto_open_first_load", a, actions, bgAutoOpenFirstLoadOptions()); err == nil {
		t.Fatal("a run whose ReadFlag itself opens the group passed; the runner is not checking state")
	}
}
