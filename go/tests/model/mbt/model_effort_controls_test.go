//go:build !windows

package mbt

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"slices"
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

// specs/model_effort_controls.fizz: the "Next turn" model and effort
// pickers of one web session, driven through a real serve.
//
// The serve and its sessions deliberately run different llm rows, the
// way a serve started from ~ runs sessions in a repo with its own
// bough.yml: serve's config names llm-echo with a model, the session's
// cwd names llm-control with none. A picker that names serve's model,
// or stops offering the configured row once the session has answered,
// is the bug the spec is about.

// meServeConfig is serve's own ~/.bough/bough.yml; meSessionConfig is
// the bough.yml in every session's cwd.
const (
	meServeConfig   = "- id: llm\n  plugin: llm-echo\n  config:\n    model: serve-default\n"
	meSessionConfig = "- id: llm\n  plugin: llm-control\n"
	// meUnlisted is the spec's x: an id no provider lists.
	meUnlisted = "unlisted-model-x"
)

// meCatalogue is GET /api/models as the page reads it.
type meCatalogue struct {
	Providers []serve.ProviderInfo `json:"providers"`
	Efforts   []string             `json:"efforts"`
	Default   *serve.ModelDefault  `json:"default"`
}

// meAdapter plays the session page: it is the client whose catalogue
// fetch loads or fails, whose picker has a POST in flight, and it reads
// what the picker shows with the same derivation Controls uses.
type meAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	work string // every session's cwd, with meSessionConfig in it
	gate gate

	id        string
	catalogue string       // loading | failed | loaded
	cat       *meCatalogue // the loaded catalogue, nil otherwise
	runs      string       // cfg | m | x: what the adapter asked the next turn to run
	switching bool
	archived  bool
	turn      int
	ids       []string

	// m is the spec's m, chosen from the first catalogue: a model with
	// its own levels, which include the one PickEffort picks.
	mPlugin, mID string

	// readsServeDefault is the deliberate bug TestModelEffortControls-
	// CatchesWrongAdapter injects, the one this flow was written for:
	// the picker takes serve's /api/models default as the session's
	// configured row. It shows on the first step after the catalogue
	// loads, so a short run cannot miss it.
	readsServeDefault bool
}

func newMEAdapter(t *testing.T) *meAdapter {
	s := servetest.Start(t, servetest.Options{Config: meServeConfig})
	work := s.Dir(t, "work")
	if err := os.WriteFile(filepath.Join(work, "bough.yml"), []byte(meSessionConfig), 0o644); err != nil {
		t.Fatal(err)
	}
	return &meAdapter{t: t, s: s, dir: control.Dir(s.Home), work: work}
}

// call is one API request as another client (or the page) makes it; it
// returns the status so a refused call can be told from a broken one.
func (a *meAdapter) call(method, path string, body, out any, auth bool) (int, error) {
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
	if auth {
		req.Header.Set("Authorization", "Bearer "+a.s.Token)
	}
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

// expect makes one session verb and requires the status the spec says.
func (a *meAdapter) expect(want int, verb string, body any) error {
	code, err := a.call(http.MethodPost, "/api/sessions/"+url.PathEscape(a.id)+"/"+verb, body, nil, true)
	if err != nil {
		return err
	}
	if code != want {
		return fmt.Errorf("POST %s %v: status %d, want %d", verb, body, code, want)
	}
	return nil
}

// Init starts each walk on a fresh session whose page has just asked
// for the catalogue.
func (a *meAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.work, "")
	if err != nil {
		return err
	}
	a.id, a.ids = row.ID, append(a.ids, row.ID)
	a.catalogue, a.cat, a.runs, a.switching, a.archived = "loading", nil, "cfg", false, false
	a.gate.reset()
	return nil
}

// Cleanup archives the walk's session: that kills its child, so a
// hundred walks do not leave a hundred children running.
func (a *meAdapter) Cleanup() error {
	if a.archived {
		return nil
	}
	return a.expect(http.StatusOK, "archive", nil)
}

func (a *meAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

func (a *meAdapter) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return nil, err
	}
	if a.readsServeDefault && row.Configured != nil && a.cat != nil && a.cat.Default != nil {
		serveRow := *a.cat.Default
		row.Configured = &serveRow
	}
	v := pickerView(row, a.cat)
	return map[string]any{
		"catalogue":      a.catalogue,
		"runs":           a.runs,
		"shown":          a.name(v.shown),
		"group":          v.group,
		"offers_default": v.offersDefault,
		"efforts":        a.levels(v),
		"effort":         row.Effort,
		"archived":       row.Archived,
		"switching":      a.switching,
	}, nil
}

// name is the spec's name for what the picker shows. The session's
// configured row is llm-control, which names no model in its config and
// answers as "control": either names cfg.
func (a *meAdapter) name(shown string) string {
	switch shown {
	case "":
		return ""
	case "llm-control", "control":
		return "cfg"
	case a.mID:
		return "m"
	case meUnlisted:
		return "x"
	}
	return "other:" + shown
}

// levels is the spec's name for the effort select's list.
func (a *meAdapter) levels(v pickerState) string {
	switch {
	case len(v.efforts) == 0:
		return "none"
	case a.cat != nil && slices.Equal(v.efforts, a.cat.Efforts):
		return "all"
	case v.effortsOf == a.mID && a.mID != "":
		return "own"
	}
	return fmt.Sprintf("other:%v", v.efforts)
}

// pickerState is what the Next turn controls show.
type pickerState struct {
	shown         string // the picked option's label, "" when it names nothing
	group         string // configured | catalogue | inuse | none
	offersDefault bool
	efforts       []string
	effortsOf     string // the catalogue model whose own levels these are, "" otherwise
}

type pickerOption struct{ value, label, group string }

// pickerView is what Controls (web/src/app.tsx) must derive from the row
// and the catalogue: the model options, the picked one, and the effort
// levels. It is the contract the page is held to, in Go.
//
//   - Until the catalogue loads the picker names nothing.
//   - While the row carries `configured` (the session still runs its own
//     llm row) the Configured option is offered and picked, labelled by
//     its model, or its plugin when the config names none. Not serve's
//     /api/models default: that is serve's row, not the session's.
//   - Otherwise the picked option is row.model, under "In use" when no
//     provider lists it.
//   - The effort levels are those of the model the next turn runs.
func pickerView(row serve.Row, cat *meCatalogue) pickerState {
	st := pickerState{group: "none"}
	if cat == nil {
		return st
	}
	var opts []pickerOption
	conf := row.Configured
	value, runsAs := row.Model, row.Model
	switch {
	case conf != nil:
		label := conf.Model
		if label == "" {
			label = conf.Plugin
		}
		opts = append(opts, pickerOption{"", label, "configured"})
		value, runsAs = "", conf.Model
	case row.Model == "":
		// No row says what the session runs: the option names nothing.
		opts = append(opts, pickerOption{"", "", "none"})
	}
	var listed []serve.ModelInfo
	for _, p := range cat.Providers {
		for _, m := range p.Models {
			opts = append(opts, pickerOption{m.ID, m.ID, "catalogue"})
			listed = append(listed, m)
		}
	}
	if value != "" && !slices.ContainsFunc(opts, func(o pickerOption) bool { return o.value == value }) {
		opts = append(opts, pickerOption{value, value, "inuse"})
	}
	for _, o := range opts {
		if o.value == value {
			st.shown, st.group = o.label, o.group
		}
		if o.value == "" && o.group == "configured" {
			st.offersDefault = true
		}
	}
	st.efforts = cat.Efforts
	if i := slices.IndexFunc(listed, func(m serve.ModelInfo) bool { return m.ID == runsAs }); i >= 0 && len(listed[i].Efforts) > 0 {
		st.efforts, st.effortsOf = listed[i].Efforts, runsAs
	}
	return st
}

// CatalogueLoads is the page's GET /api/models answering. The first one
// also picks the spec's m.
func (a *meAdapter) CatalogueLoads() error {
	if !a.gate.pass(a.catalogue == "loading") {
		return nil
	}
	var cat meCatalogue
	code, err := a.call(http.MethodGet, "/api/models", nil, &cat, true)
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
			return errors.New("the catalogue lists no model with its own levels including high")
		}
	}
	a.catalogue, a.cat = "loaded", &cat
	return nil
}

// CatalogueFails is a real failed fetch: the request goes out without
// the page's token and the page, like useCatalogue, keeps nothing.
func (a *meAdapter) CatalogueFails() error {
	if !a.gate.pass(a.catalogue == "loading") {
		return nil
	}
	code, err := a.call(http.MethodGet, "/api/models", nil, nil, false)
	if err != nil {
		return err
	}
	if code/100 == 2 {
		return fmt.Errorf("GET /api/models without the token answered %d", code)
	}
	a.catalogue, a.cat = "failed", nil
	return nil
}

func (a *meAdapter) RetryCatalogue() error {
	if a.gate.pass(a.catalogue == "failed") {
		a.catalogue = "loading"
	}
	return nil
}

// Turn sends a prompt and waits for its turn to close. A turn is queued
// with llm-control whatever the session runs: on m (a provider with no
// key here) the turn fails without taking it, and the unused turn is
// taken back so a later session does not answer with it.
func (a *meAdapter) Turn() error {
	if !a.gate.pass(a.catalogue == "loaded" && !a.archived && !a.switching) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, before, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return err
	}
	a.turn++
	name := fmt.Sprintf("me%04d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "ok", Text: "answered " + name})
	defer os.Remove(filepath.Join(a.dir, name+".json"))
	if err := a.s.Prompt(ctx, a.id, "turn "+name); err != nil {
		return err
	}
	for dones(lines(a.s, a.id)) <= dones(before) {
		if ctx.Err() != nil {
			return fmt.Errorf("waiting for turn %s to close: %w", name, ctx.Err())
		}
		time.Sleep(50 * time.Millisecond)
	}
	_, err = waitRow(a.s, a.id, "the turn to settle", func(r serve.Row) bool { return r.Status != serve.StatusRunning })
	return err
}

// lines is the session's transcript, nil when it cannot be read.
func lines(s *servetest.Server, id string) []serve.Line {
	ctx, cancel := actionCtx()
	defer cancel()
	_, ls, _ := s.GetSession(ctx, id)
	return ls
}

func dones(lines []serve.Line) int {
	n := 0
	for _, l := range lines {
		if l.Kind == "done" {
			n++
		}
	}
	return n
}

// PickModel is choosing m in the picker; its POST answers in Land.
func (a *meAdapter) PickModel() error {
	if a.gate.pass(a.catalogue == "loaded" && !a.switching && a.runs != "m") {
		a.switching = true
	}
	return nil
}

// Land is the picker's POST /model answering: the provider is the one
// whose group listed m. Archived meanwhile, it is refused.
func (a *meAdapter) Land() error {
	if !a.gate.pass(a.switching) {
		return nil
	}
	a.switching = false
	body := map[string]string{"plugin": a.mPlugin, "model": a.mID}
	if a.archived {
		return a.expect(http.StatusConflict, "model", body)
	}
	a.runs = "m"
	return a.expect(http.StatusOK, "model", body)
}

// SetUnlistedModel is another client's POST /model with a bare id no
// provider lists.
func (a *meAdapter) SetUnlistedModel() error {
	if !a.gate.pass(a.catalogue == "loaded" && !a.switching && a.runs != "x") {
		return nil
	}
	body := map[string]string{"model": meUnlisted}
	if a.archived {
		return a.expect(http.StatusConflict, "model", body)
	}
	a.runs = "x"
	return a.expect(http.StatusOK, "model", body)
}

func (a *meAdapter) PickEffort() error {
	if !a.gate.pass(a.catalogue == "loaded" && !a.switching && a.effortNow() == "") {
		return nil
	}
	if a.archived {
		return a.expect(http.StatusConflict, "effort", map[string]string{"effort": "high"})
	}
	return a.expect(http.StatusOK, "effort", map[string]string{"effort": "high"})
}

// BadEffort is another client asking for a level no provider has:
// refused as invalid, archived or not.
func (a *meAdapter) BadEffort() error {
	if !a.gate.pass(a.catalogue == "loaded" && !a.switching) {
		return nil
	}
	return a.expect(http.StatusInternalServerError, "effort", map[string]string{"effort": "extreme"})
}

func (a *meAdapter) Archive() error {
	if !a.gate.pass(a.catalogue == "loaded" && !a.archived) {
		return nil
	}
	a.archived = true
	return a.expect(http.StatusOK, "archive", nil)
}

func (a *meAdapter) Unarchive() error {
	if !a.gate.pass(a.archived && !a.switching) {
		return nil
	}
	a.archived = false
	return a.expect(http.StatusOK, "unarchive", nil)
}

// effortNow is the row's effort, for PickEffort's require.
func (a *meAdapter) effortNow() string {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return "?"
	}
	return row.Effort
}

var meActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"CatalogueLoads":   action((*meAdapter).CatalogueLoads),
	"CatalogueFails":   action((*meAdapter).CatalogueFails),
	"RetryCatalogue":   action((*meAdapter).RetryCatalogue),
	"Turn":             action((*meAdapter).Turn),
	"PickModel":        action((*meAdapter).PickModel),
	"Land":             action((*meAdapter).Land),
	"SetUnlistedModel": action((*meAdapter).SetUnlistedModel),
	"PickEffort":       action((*meAdapter).PickEffort),
	"BadEffort":        action((*meAdapter).BadEffort),
	"Archive":          action((*meAdapter).Archive),
	"Unarchive":        action((*meAdapter).Unarchive),
}}

// Most actions are an HTTP call, and Turn one quick llm-control turn,
// so walks can be longer than the example's.
func meOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

// modelEffortControlsHistory reads the trace off a transcript. What the
// page does alone (the catalogue, archiving, a refused call) leaves
// nothing, so the projection supplies the one such step every recorded
// action needs first: the catalogue loading. The rest is what the child
// was sent: an input is a Turn, "/model <plugin> <id>" is the picker's
// PickModel and Land, a bare "/model <id>" another client's
// SetUnlistedModel, "/think <level>" PickEffort.
func modelEffortControlsHistory(entries []history.Entry) []tracecheck.Step {
	runs := func(r string) map[string]any { return map[string]any{"Session#0.runs": r} }
	steps := []tracecheck.Step{
		{Action: "Init", State: map[string]any{"Session#0.runs": "cfg", "Session#0.effort": ""}},
		{Action: "Session#0.CatalogueLoads", State: map[string]any{"Session#0.catalogue": "loaded"}},
	}
	for _, e := range entries {
		text, _ := e.Data["text"].(string)
		switch {
		case e.Kind == "input":
			steps = append(steps, tracecheck.Step{Action: "Session#0.Turn"})
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

func init() { historyProjections["model_effort_controls"] = modelEffortControlsHistory }

func TestModelEffortControls(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newMEAdapter(t)
	if err := runMBT(t, "model_effort_controls", a, meActions, meOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "model_effort_controls"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), modelEffortControlsHistory)
	}
}

// A picker that names serve's configured model must fail the run.
func TestModelEffortControlsCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newMEAdapter(t)
	a.readsServeDefault = true
	if err := runMBT(t, "model_effort_controls", a, meActions, meOptions()); err == nil {
		t.Fatal("a run whose picker names serve's model passed; the runner is not checking state")
	}
}

// The projection reads a transcript the way the walks write one, and a
// transcript the spec does not allow (the effort picked twice: after
// the first pick the select offers no Default to pick from) is refused.
func TestModelEffortControlsHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(fizzCheck(t, "model_effort_controls"))
	if err != nil {
		t.Fatal(err)
	}
	cmd := func(text string) history.Entry {
		return history.Entry{Kind: "command", Data: map[string]any{"text": text}}
	}
	input := history.Entry{Kind: "input", Data: map[string]any{"text": "turn"}}
	ok := []history.Entry{{Kind: "meta"}, input,
		cmd("/model llm-anthropic claude-fable-5-1"), cmd("/think high"), cmd("/model " + meUnlisted), input}
	if v := g.Check(modelEffortControlsHistory(ok)); v != nil {
		t.Fatalf("a transcript the walks write is refused: %v", v)
	}
	twice := append(slices.Clone(ok), cmd("/think high"))
	if v := g.Check(modelEffortControlsHistory(twice)); v == nil || !strings.Contains(v.Reason, "not enabled") {
		t.Fatalf("effort picked twice: Check = %v, want PickEffort not enabled", v)
	}
}
