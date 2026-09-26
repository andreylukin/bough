//go:build !windows

package mbt

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/mode_picker_effort_model_race.fizz: the model/effort picker
// racing a provider change underneath it — a config hot reload that
// remounts the session's own llm row, or a resume that hands the
// session a different provider. Neither invalidates a pick already in
// flight (the POST still lands), but the picker must never present a
// pick that landed under a provider epoch that has since moved on as
// if it came from the *current* catalogue, and must not grant it that
// model's own restricted effort levels.
//
// GET /api/models itself never depends on the session's own provider —
// it lists every compiled llm-* plugin's curated models regardless of
// which one a session runs (internal/serve/models.go) — so nothing
// about a provider change can ever make a real, still-listed model id
// drop out of "catalogue" group. The race the spec is about is
// therefore about the session's own row, not the shared catalogue: an
// epoch is the adapter's own bookkeeping for "which provider the
// session ran when this pick was made", exactly the way the example
// flow's `viewing` is client state the adapter tracks rather than
// fetches (see go/tests/model/README.md, "Every field must be
// observable twice"). What is real and is checked against the live
// server on every step: the catalogue fetch itself, the session's own
// `configured` row actually flipping when its bough.yml is edited
// underneath it (a real hot reload), and the model the next turn
// really runs after a pick lands — read straight off GET
// /api/sessions/<id>, never assumed.

// mprUnlisted is the spec's x: an id no provider lists.
const mprUnlisted = "unlisted-model-x"

// mprConfigA/B are the session's own bough.yml, toggled by
// ProviderChanges: the same plugin (llm-echo, so a turn never needs a
// key) with a different `model:`, so row.Configured really does change
// on a real hot reload.
func mprConfig(b bool) string {
	m := "provider-a"
	if b {
		m = "provider-b"
	}
	return fmt.Sprintf("- id: llm\n  plugin: llm-echo\n  config:\n    model: %q\n", m)
}

// mprCatalogue is GET /api/models as the page reads it.
type mprCatalogue struct {
	Providers []serve.ProviderInfo `json:"providers"`
	Efforts   []string             `json:"efforts"`
}

// mprAdapter plays the session page. catalogue/switching/epoch/
// pickEpoch/stale are the adapter's own bookkeeping — the spec's own
// fields, computed the same way the spec computes them, per the
// README's "tracked by the adapter" allowance; runs and effort are
// read back from the real session row every GetState, never assumed.
type mprAdapter struct {
	t    *testing.T
	s    *servetest.Server
	work string // the session's cwd, holding mprConfig(providerB)
	gate gate

	id  string
	ids []string

	catalogue string // loading | loaded | stale
	switching bool
	epoch     bool
	pickEpoch bool
	stale     bool
	providerB bool

	mPlugin, mID string // chosen from the first real catalogue fetch

	// badRuns is the deliberate wrong wiring
	// TestModePickerEffortModelRaceCatchesWrongAdapter injects: the
	// picker never notices a landed pick, always reporting the
	// configured default even after Land really changed row.Model.
	badRuns bool
}

func newMPRAdapter(t *testing.T) *mprAdapter {
	s := servetest.Start(t, servetest.Options{Config: "- id: llm\n  plugin: llm-echo\n"})
	work := s.Dir(t, "work")
	return &mprAdapter{t: t, s: s, work: work}
}

func (a *mprAdapter) writeConfig() error {
	return os.WriteFile(filepath.Join(a.work, "bough.yml"), []byte(mprConfig(a.providerB)), 0o644)
}

func (a *mprAdapter) Init() error {
	a.providerB = false
	if err := a.writeConfig(); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.work, "")
	if err != nil {
		return err
	}
	a.id, a.ids = row.ID, append(a.ids, row.ID)
	a.catalogue, a.switching, a.epoch, a.pickEpoch, a.stale = "loading", false, false, false, false
	a.gate.reset()
	return nil
}

// Cleanup archives the walk's session so a hundred walks do not leave a
// hundred children running.
func (a *mprAdapter) Cleanup() error {
	return a.expect(http.StatusOK, "archive", nil)
}

func (a *mprAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

func (a *mprAdapter) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return nil, err
	}
	runs := "cfg"
	if !a.badRuns {
		switch {
		case row.Configured != nil:
			runs = "cfg"
		case row.Model == a.mID && a.mID != "":
			runs = "m"
		case row.Model == mprUnlisted:
			runs = "x"
		}
	}
	shown, group, offersDefault, efforts := mprView(a.catalogue, runs, a.stale)
	return map[string]any{
		"catalogue":      a.catalogue,
		"runs":           runs,
		"shown":          shown,
		"group":          group,
		"offers_default": offersDefault,
		"efforts":        efforts,
		"effort":         row.Effort,
		"switching":      a.switching,
		"stale":          a.stale,
		"epoch":          a.epoch,
		"pick_epoch":     a.pickEpoch,
	}, nil
}

// mprView is the spec's own view(): given whether the catalogue is
// loaded, what the next turn runs and whether that pick landed under a
// provider epoch that has since moved on, it derives what the picker
// shows. Mirrored in Go rather than fetched a second time, the way the
// example flow's `viewing` is adapter-tracked client state.
func mprView(catalogue, runs string, stale bool) (shown, group string, offersDefault bool, efforts string) {
	if catalogue != "loaded" {
		return "", "none", false, "none"
	}
	shown = runs
	offersDefault = runs == "cfg"
	switch {
	case runs == "cfg":
		group = "configured"
	case runs == "m" && !stale:
		group = "catalogue"
	default:
		group = "inuse"
	}
	if runs == "m" && !stale {
		efforts = "own"
	} else {
		efforts = "all"
	}
	return
}

// call is one API request as the page makes it.
func (a *mprAdapter) call(method, path string, body, out any) (int, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	var rd io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return 0, err
		}
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, a.s.URL+path, rd)
	if err != nil {
		return 0, err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return resp.StatusCode, err
	}
	if resp.StatusCode/100 == 2 && out != nil {
		if err := json.Unmarshal(raw, out); err != nil {
			return resp.StatusCode, fmt.Errorf("%s %s: %w: %s", method, path, err, raw)
		}
	}
	return resp.StatusCode, nil
}

func (a *mprAdapter) expect(want int, verb string, body any) error {
	code, err := a.call(http.MethodPost, "/api/sessions/"+url.PathEscape(a.id)+"/"+verb, body, nil)
	if err != nil {
		return err
	}
	if code != want {
		return fmt.Errorf("POST %s %v: status %d, want %d", verb, body, code, want)
	}
	return nil
}

// CatalogueLoads is the page's GET /api/models answering, a real
// request every time (the content never depends on the session's
// provider, but the endpoint's health does).
func (a *mprAdapter) CatalogueLoads() error {
	if !a.gate.pass(a.catalogue == "loading") {
		return nil
	}
	var cat mprCatalogue
	code, err := a.call(http.MethodGet, "/api/models", nil, &cat)
	if err != nil {
		return err
	}
	if code != http.StatusOK {
		return fmt.Errorf("GET /api/models: status %d", code)
	}
	if a.mID == "" {
		for _, p := range cat.Providers {
			for _, m := range p.Models {
				if a.mID == "" && slices.Contains(m.Efforts, "high") && !slices.Equal(m.Efforts, cat.Efforts) {
					a.mPlugin, a.mID = p.Plugin, m.ID
				}
			}
		}
		if a.mID == "" {
			return fmt.Errorf("mode-picker-effort-model-race: the catalogue lists no model with its own levels including high")
		}
	}
	a.catalogue = "loaded"
	return nil
}

// ProviderChanges is a real hot reload of the session's own bough.yml:
// its llm row's model flips, so row.Configured really changes
// underneath whatever is running or in flight.
func (a *mprAdapter) ProviderChanges() error {
	if !a.gate.pass(a.catalogue == "loaded") {
		return nil
	}
	a.providerB = !a.providerB
	if err := a.writeConfig(); err != nil {
		return err
	}
	want := "provider-a"
	if a.providerB {
		want = "provider-b"
	}
	if _, err := waitRow(a.s, a.id, "the config reload to land", func(r serve.Row) bool {
		return r.Configured != nil && r.Configured.Model == want
	}); err != nil {
		return err
	}
	a.epoch = !a.epoch
	a.catalogue = "stale"
	return nil
}

// CatalogueReload is the page noticing its catalogue no longer
// describes the current provider and asking for it again.
func (a *mprAdapter) CatalogueReload() error {
	if a.gate.pass(a.catalogue == "stale") {
		a.catalogue = "loading"
	}
	return nil
}

// PickModel is choosing m in the picker under whatever epoch is
// current right now.
func (a *mprAdapter) PickModel() error {
	st, err := a.GetState()
	if err != nil {
		return err
	}
	if a.gate.pass(a.catalogue == "loaded" && !a.switching && st["runs"] != "m") {
		a.switching = true
		a.pickEpoch = a.epoch
	}
	return nil
}

// Land is the picker's POST /model answering: it lands regardless of a
// provider change meanwhile.
func (a *mprAdapter) Land() error {
	if !a.gate.pass(a.switching) {
		return nil
	}
	a.switching = false
	if err := a.expect(http.StatusOK, "model", map[string]string{"plugin": a.mPlugin, "model": a.mID}); err != nil {
		return err
	}
	a.stale = a.pickEpoch != a.epoch
	return nil
}

// SetUnlistedModel is another client's POST /model with a bare id no
// provider lists.
func (a *mprAdapter) SetUnlistedModel() error {
	st, err := a.GetState()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.catalogue == "loaded" && !a.switching && st["runs"] != "x") {
		return nil
	}
	return a.expect(http.StatusOK, "model", map[string]string{"model": mprUnlisted})
}

func (a *mprAdapter) PickEffort() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return err
	}
	if !a.gate.pass(a.catalogue == "loaded" && !a.switching && row.Effort == "") {
		return nil
	}
	return a.expect(http.StatusOK, "effort", map[string]string{"effort": "high"})
}

var mprActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"CatalogueLoads":   action((*mprAdapter).CatalogueLoads),
	"ProviderChanges":  action((*mprAdapter).ProviderChanges),
	"CatalogueReload":  action((*mprAdapter).CatalogueReload),
	"PickModel":        action((*mprAdapter).PickModel),
	"Land":             action((*mprAdapter).Land),
	"SetUnlistedModel": action((*mprAdapter).SetUnlistedModel),
	"PickEffort":       action((*mprAdapter).PickEffort),
}}

// Every action is a quick HTTP call or a file write, so walks can be
// generous.
func mprOptions() map[string]any {
	return map[string]any{"max-seq-runs": 150, "max-actions": 10, "max-parallel-runs": 0}
}

// modePickerEffortModelRaceHistory reads the trace off a transcript.
// ProviderChanges/CatalogueReload leave nothing (a file edit and a
// page-local refetch, like the example flow's own client-only steps),
// so the projection supplies the one step every recorded run needs
// first: the catalogue loading. The rest is what the child was sent:
// "/model <plugin> <id>" is PickModel+Land, a bare "/model <id>"
// SetUnlistedModel, "/think <level>" PickEffort.
func modePickerEffortModelRaceHistory(entries []history.Entry) []tracecheck.Step {
	runs := func(r string) map[string]any { return map[string]any{"Session#0.runs": r} }
	steps := []tracecheck.Step{
		{Action: "Init", State: map[string]any{"Session#0.runs": "cfg", "Session#0.effort": ""}},
		{Action: "Session#0.CatalogueLoads", State: map[string]any{"Session#0.catalogue": "loaded"}},
	}
	for _, e := range entries {
		text, _ := e.Data["text"].(string)
		switch {
		case e.Kind == "command" && strings.HasPrefix(text, "/model "):
			if len(strings.Fields(text)) == 3 {
				steps = append(steps, tracecheck.Step{Action: "Session#0.PickModel"},
					tracecheck.Step{Action: "Session#0.Land", State: runs("m")})
			} else {
				steps = append(steps, tracecheck.Step{Action: "Session#0.SetUnlistedModel", State: runs("x")})
			}
		case e.Kind == "command" && strings.HasPrefix(text, "/think "):
			steps = append(steps, tracecheck.Step{Action: "Session#0.PickEffort",
				State: map[string]any{"Session#0.effort": strings.TrimPrefix(text, "/think ")}})
		}
	}
	return steps
}

func init() {
	historyProjections["mode_picker_effort_model_race"] = modePickerEffortModelRaceHistory
}

func TestModePickerEffortModelRace(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newMPRAdapter(t)
	if err := runMBT(t, "mode_picker_effort_model_race", a, mprActions, mprOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "mode_picker_effort_model_race"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), modePickerEffortModelRaceHistory)
	}
}

// A picker that never notices a landed pick must fail the run.
func TestModePickerEffortModelRaceCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newMPRAdapter(t)
	a.badRuns = true
	if err := runMBT(t, "mode_picker_effort_model_race", a, mprActions, mprOptions()); err == nil {
		t.Fatal("a run whose picker never notices a landed pick passed; the runner is not checking state")
	}
}

// The projection reads a transcript the way the walks write one, and a
// transcript the spec does not allow (the effort picked twice: after
// the first pick the select offers no Default to pick from) is refused.
func TestModePickerEffortModelRaceHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(fizzCheck(t, "mode_picker_effort_model_race"))
	if err != nil {
		t.Fatal(err)
	}
	cmd := func(text string) history.Entry {
		return history.Entry{Kind: "command", Data: map[string]any{"text": text}}
	}
	ok := []history.Entry{{Kind: "meta"},
		cmd("/model llm-anthropic claude-fable-5-1"), cmd("/think high"), cmd("/model " + mprUnlisted)}
	if v := g.Check(modePickerEffortModelRaceHistory(ok)); v != nil {
		t.Fatalf("a transcript the walks write is refused: %v", v)
	}
	twice := append(slices.Clone(ok), cmd("/think high"))
	if v := g.Check(modePickerEffortModelRaceHistory(twice)); v == nil || !strings.Contains(v.Reason, "not enabled") {
		t.Fatalf("effort picked twice: Check = %v, want PickEffort not enabled", v)
	}
}
