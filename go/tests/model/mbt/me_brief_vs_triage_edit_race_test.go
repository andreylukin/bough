//go:build !windows

package mbt

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/me_brief_vs_triage_edit_race.fizz against a real serve: the Page
// role is the Me page's view of one signal, "k1". The background
// regenerate (the launchd tick's headless `/llm-wiki brief`) is not run
// for real here — spawning it would cost a model turn per gather and
// the race is entirely about file timing, not the skill's prose — so
// the adapter plays its two file touches itself: GatherStart reads
// triage.json's dismissed set the way SKILL.md's "Then read
// topics/me/triage.json" does, near the top of the run, and GatherWrite
// writes topics/me/signals.json the way runBrief's one rewrite does at
// the end, filtering by that stale snapshot rather than by triage.json
// as it now stands. Dismiss, Undismiss, Pin and Unpin go through the
// real product: POST /api/me/triage, wiki.Mark's own unlocked
// read-modify-write of the same file. present, dismissed and pinned are
// read back through GET /api/me, the same call the Me page itself makes
// on every render — never cached, which is exactly the property that
// clears DismissedNeverShown despite the race.

const meBriefKey = "k1"

type meBriefAdapter struct {
	t    *testing.T
	s    *servetest.Server
	gate gate

	// The background regenerate's own state: gathering is whether one
	// is between its read and its write, snapDismissed is what its one
	// read of triage.json saw.
	gathering     bool
	snapDismissed bool

	// alwaysPresent is the deliberate bug TestMeBriefVsTriageEditRace
	// CatchesWrongAdapter injects: GatherWrite ignores the snapshot it
	// took and always writes the signal present, the kind of wiring bug
	// a from-scratch adapter can have.
	alwaysPresent bool
}

func newMeBriefAdapter(t *testing.T) *meBriefAdapter {
	s := servetest.Start(t, servetest.Options{})
	return &meBriefAdapter{t: t, s: s}
}

func (a *meBriefAdapter) mePaths() (triage, signals string) {
	me := filepath.Join(a.s.Home, ".bough", "wiki", "topics", "me")
	return filepath.Join(me, "triage.json"), filepath.Join(me, "signals.json")
}

// Init is a fresh page: no triage.json (nothing dismissed or pinned)
// and signals.json with "k1" present, matching the spec's Init.
func (a *meBriefAdapter) Init() error {
	triage, _ := a.mePaths()
	if err := os.MkdirAll(filepath.Dir(triage), 0o755); err != nil {
		return err
	}
	if err := os.Remove(triage); err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	a.gathering, a.snapDismissed = false, false
	a.gate.reset()
	return a.writeSignals(true)
}

// writeSignals is runBrief's one rewrite of signals.json: "k1" present
// or, dismissed at the snapshot it read, left out of items the way
// SKILL.md's "never list [dismissed keys] again... in rows" has it.
func (a *meBriefAdapter) writeSignals(present bool) error {
	_, signals := a.mePaths()
	items := "[]"
	if present {
		items = fmt.Sprintf(`[{"kind":"needs-you","source":"gh","cite":%q,"title":"Review"}]`, meBriefKey)
	}
	body := fmt.Sprintf(`{"asOf":%q,"items":%s,"sources":[]}`, time.Now().Format(time.RFC3339), items)
	return os.WriteFile(signals, []byte(body), 0o644)
}

type meTriageWire struct {
	Dismissed map[string]string `json:"dismissed"`
	Pinned    []string          `json:"pinned"`
}

// read is GET /api/me, the same call groupSignals in me.tsx renders
// from on every page load: triage.json and signals.json, both fresh,
// never the brief's own stale copy.
func (a *meBriefAdapter) read() (dismissed, pinned, present bool, err error) {
	var d struct {
		Triage  meTriageWire `json:"triage"`
		Signals struct {
			Items []struct {
				Cite string `json:"cite"`
			} `json:"items"`
		} `json:"signals"`
	}
	if err = a.call(http.MethodGet, "/api/me", nil, &d); err != nil {
		return
	}
	_, dismissed = d.Triage.Dismissed[meBriefKey]
	for _, p := range d.Triage.Pinned {
		if p == meBriefKey {
			pinned = true
		}
	}
	for _, it := range d.Signals.Items {
		if it.Cite == meBriefKey {
			present = true
		}
	}
	return
}

func (a *meBriefAdapter) mark(action string) error {
	return a.call(http.MethodPost, "/api/me/triage", map[string]string{"action": action, "key": meBriefKey}, nil)
}

func (a *meBriefAdapter) call(method, path string, body, out any) error {
	ctx, cancel := actionCtx()
	defer cancel()
	var rd io.Reader
	if body != nil {
		b, _ := json.Marshal(body)
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, a.s.URL+path, rd)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	if resp.StatusCode/100 != 2 {
		return &servetest.APIError{Status: resp.StatusCode, Msg: string(raw)}
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(raw, out)
}

// Cleanup: no walk holds anything open (no turns, no queue) to release.
func (a *meBriefAdapter) Cleanup() error { return nil }

func (a *meBriefAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Page", Index: 0}: a}, nil
}

// GetState is the Page role's five fields: dismissed, pinned and
// present come off the server fresh every time (GET /api/me), gathering
// and snap_dismissed are the background regenerate's own state, which
// only the adapter playing it can see.
func (a *meBriefAdapter) GetState() (map[string]any, error) {
	dismissed, pinned, present, err := a.read()
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"dismissed":      dismissed,
		"pinned":         pinned,
		"gathering":      a.gathering,
		"snap_dismissed": a.snapDismissed,
		"present":        present,
	}, nil
}

// GatherStart: "Then read topics/me/triage.json" — the one read, taken
// before the person's own Mark can race it.
func (a *meBriefAdapter) GatherStart() error {
	dismissed, _, _, err := a.read()
	if err != nil {
		return err
	}
	if !a.gate.pass(!a.gathering) {
		return nil
	}
	a.gathering = true
	a.snapDismissed = dismissed
	return nil
}

// GatherWrite: runBrief's single rewrite of signals.json, filtered by
// the snapshot GatherStart took, not by triage.json as it now stands.
func (a *meBriefAdapter) GatherWrite() error {
	if !a.gate.pass(a.gathering) {
		return nil
	}
	a.gathering = false
	present := !a.snapDismissed
	if a.alwaysPresent {
		present = true
	}
	return a.writeSignals(present)
}

func (a *meBriefAdapter) Dismiss() error {
	dismissed, _, _, err := a.read()
	if err != nil {
		return err
	}
	if !a.gate.pass(!dismissed) {
		return nil
	}
	return a.mark("dismiss")
}

func (a *meBriefAdapter) Undismiss() error {
	dismissed, _, _, err := a.read()
	if err != nil {
		return err
	}
	if !a.gate.pass(dismissed) {
		return nil
	}
	return a.mark("undismiss")
}

func (a *meBriefAdapter) Pin() error {
	dismissed, pinned, _, err := a.read()
	if err != nil {
		return err
	}
	if !a.gate.pass(!pinned && !dismissed) {
		return nil
	}
	return a.mark("pin")
}

func (a *meBriefAdapter) Unpin() error {
	_, pinned, _, err := a.read()
	if err != nil {
		return err
	}
	if !a.gate.pass(pinned) {
		return nil
	}
	return a.mark("unpin")
}

var meBriefActions = map[string]map[string]fmbt.ActionFunc{"Page": {
	"GatherStart": action((*meBriefAdapter).GatherStart),
	"GatherWrite": action((*meBriefAdapter).GatherWrite),
	"Dismiss":     action((*meBriefAdapter).Dismiss),
	"Undismiss":   action((*meBriefAdapter).Undismiss),
	"Pin":         action((*meBriefAdapter).Pin),
	"Unpin":       action((*meBriefAdapter).Unpin),
}}

// No action here starts a model turn (the race is two file touches, not
// a session), so a walk costs an HTTP round trip, not a queued turn:
// more walks and longer ones are cheap.
func meBriefOptions() map[string]any {
	return map[string]any{"max-seq-runs": 200, "max-actions": 10, "max-parallel-runs": 0}
}

// meBriefHistory: this flow drives no session, so no walk leaves a
// transcript for the browser spec's trace check to project; registered
// for parity with every other flow and for a future browser spec to
// build on.
func meBriefHistory([]history.Entry) []tracecheck.Step {
	return []tracecheck.Step{{Action: "Init", State: map[string]any{
		"Page#0.dismissed": false, "Page#0.pinned": false, "Page#0.gathering": false,
		"Page#0.snap_dismissed": false, "Page#0.present": true,
	}}}
}

func init() { historyProjections["me_brief_vs_triage_edit_race"] = meBriefHistory }

func TestMeBriefVsTriageEditRace(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newMeBriefAdapter(t)
	if err := runMBT(t, "me_brief_vs_triage_edit_race", a, meBriefActions, meBriefOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// The run above proves nothing unless a server that breaks the model
// fails it. This adapter writes the signal present regardless of the
// snapshot GatherStart took, the kind of wrong wiring a flow's adapter
// can have, and the run must say so.
func TestMeBriefVsTriageEditRaceCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newMeBriefAdapter(t)
	a.alwaysPresent = true
	if err := runMBT(t, "me_brief_vs_triage_edit_race", a, meBriefActions, meBriefOptions()); err == nil {
		t.Fatal("a run whose GatherWrite ignores the snapshot passed; the runner is not checking state")
	}
}
