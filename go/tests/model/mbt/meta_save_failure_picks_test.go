//go:build !windows

package mbt

import (
	"fmt"
	"net/http"
	"net/url"
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

// specs/meta_save_failure_picks.fizz against a real serve: the model and
// effort pickers (POST /model, /effort) with meta.json's save failing,
// the way meta_save_failure_test.go fails it (a directory at
// meta.json.tmp).
//
// model and effort are the row's (serve's memory), modelDisk and
// effortDisk meta.json's. childModel and childEffort are what the child
// was last told: the last "/model" and "/think" command its transcript
// records, since the child writes every line it is sent as a "command"
// entry. The spec's models "a" and "b" are the ids "ma" and "mb" on the
// wire; llm-control answers as itself whatever id it is given, which is
// all this flow needs (the row shows meta.Model when it is set).
// toast is the adapter's, as the page's.

var picksModelID = map[string]string{"a": "ma", "b": "mb"}

// picksAbstract maps a model id back to the spec's name.
func picksAbstract(id string) string {
	for k, v := range picksModelID {
		if v == id {
			return k
		}
	}
	return id
}

type metaSaveFailurePicksAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string
	gate gate

	id, toast string
	full      bool
	turn      int
	name      string
	ids       []string
	stepsRun  int

	// fillNoop: DiskFills blocks nothing (the wrong-adapter test).
	fillNoop bool
}

func newMetaSaveFailurePicksAdapter(t *testing.T, name string) *metaSaveFailurePicksAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &metaSaveFailurePicksAdapter{t: t, s: s, dir: control.Dir(s.Home), name: name}
}

func (a *metaSaveFailurePicksAdapter) metaTmp() string {
	return filepath.Join(a.s.Home, ".bough", "serve", "meta.json.tmp")
}

// Init makes a session whose saved and told model and effort are the
// spec's first ones ("a", "low"), with no child: the picks spawn one,
// and archiving ends it.
func (a *metaSaveFailurePicksAdapter) Init() error {
	if err := os.RemoveAll(a.metaTmp()); err != nil {
		return err
	}
	a.full, a.toast = false, ""
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	if _, err := waitRow(a.s, row.ID, "the new session's file", func(serve.Row) bool { return true }); err != nil {
		return err
	}
	a.id = row.ID
	if err := a.post("model", map[string]string{"model": picksModelID["a"]}); err != nil {
		return err
	}
	if err := a.post("effort", map[string]string{"effort": "low"}); err != nil {
		return err
	}
	if err := a.waitTold("a", "low"); err != nil {
		return err
	}
	if _, err := a.s.Archive(ctx, row.ID); err != nil {
		return err
	}
	if _, err := a.s.Unarchive(ctx, row.ID); err != nil {
		return err
	}
	a.gate.reset()
	a.ids = append(a.ids, row.ID)
	return nil
}

func (a *metaSaveFailurePicksAdapter) Cleanup() error {
	if err := os.RemoveAll(a.metaTmp()); err != nil {
		return err
	}
	a.full = false
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Archive(ctx, a.id)
	return err
}

func (a *metaSaveFailurePicksAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

// told is the last model and effort the transcript says the child was
// sent.
func (a *metaSaveFailurePicksAdapter) told() (model, effort string, err error) {
	entries, err := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
	if err != nil {
		return "", "", err
	}
	model, effort = picksTold(entries)
	return model, effort, nil
}

func picksTold(entries []history.Entry) (model, effort string) {
	for _, e := range entries {
		if e.Kind != "command" {
			continue
		}
		text, _ := e.Data["text"].(string)
		if m, ok := strings.CutPrefix(text, "/model "); ok {
			model = picksAbstract(m)
		}
		if l, ok := strings.CutPrefix(text, "/think "); ok {
			effort = l
		}
	}
	return model, effort
}

// waitTold waits for the child to record the lines a pick sent it: the
// POST answers when the line is written to its stdin, not read.
func (a *metaSaveFailurePicksAdapter) waitTold(model, effort string) error {
	ctx, cancel := actionCtx()
	defer cancel()
	var m, e string
	for {
		var err error
		if m, e, err = a.told(); err == nil && m == model && e == effort {
			return nil
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("waiting for the child to be told %s/%s: told %q/%q", model, effort, m, e)
		case <-time.After(50 * time.Millisecond):
		}
	}
}

type picksState struct {
	row                     serve.Row
	modelDisk, effortDisk   string
	childModel, childEffort string
}

func (a *metaSaveFailurePicksAdapter) state() (picksState, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return picksState{}, err
	}
	disk, err := savedMeta(a.s.Home, a.id)
	if err != nil {
		return picksState{}, err
	}
	m, e, err := a.told()
	if err != nil {
		return picksState{}, err
	}
	return picksState{row: row, modelDisk: picksAbstract(disk.Model), effortDisk: disk.Effort, childModel: m, childEffort: e}, nil
}

func (a *metaSaveFailurePicksAdapter) GetState() (map[string]any, error) {
	st, err := a.state()
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"diskFull": a.full,
		"model":    picksAbstract(st.row.Model), "modelDisk": st.modelDisk,
		"effort": st.row.Effort, "effortDisk": st.effortDisk,
		"childModel": st.childModel, "childEffort": st.childEffort,
		"live": st.row.Live, "toast": a.toast,
	}, nil
}

func (a *metaSaveFailurePicksAdapter) post(verb string, body any) error {
	ctx, cancel := actionCtx()
	defer cancel()
	return msfCall(ctx, a.s, http.MethodPost, "/api/sessions/"+url.PathEscape(a.id)+"/"+verb, body, nil)
}

func (a *metaSaveFailurePicksAdapter) DiskFills() error {
	if !a.gate.pass(!a.full) {
		return nil
	}
	a.full = true
	if a.fillNoop {
		return nil
	}
	return os.Mkdir(a.metaTmp(), 0o755)
}

func (a *metaSaveFailurePicksAdapter) DiskFrees() error {
	if !a.gate.pass(a.full) {
		return nil
	}
	a.full = false
	return os.RemoveAll(a.metaTmp())
}

// Prompt is a turn from the composer, run to its end: it spawns the
// child when none is up.
func (a *metaSaveFailurePicksAdapter) Prompt() error {
	st, err := a.state()
	if err != nil {
		return err
	}
	if !a.gate.pass(!st.row.Live) {
		return nil
	}
	a.turn++
	name := fmt.Sprintf("%s%05d", a.name, a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "ok", Text: "answered " + name})
	text := "turn " + name
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, text); err != nil {
		return err
	}
	for {
		row, lines, err := a.s.GetSession(ctx, a.id)
		if err == nil {
			in, closed := -1, false
			for i, l := range lines {
				if l.Kind == "input" && strings.Contains(l.Text, text) {
					in = i
				}
				if in >= 0 && i > in && l.Kind == "done" {
					closed = true
				}
			}
			if closed && row.Status == serve.StatusDone && row.Live {
				return nil
			}
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("waiting for %q to be answered", text)
		case <-time.After(50 * time.Millisecond):
		}
	}
}

// pick is the picker's POST and act()'s toast. On success it waits for
// the child to record the line, so the next step sees it told.
func (a *metaSaveFailurePicksAdapter) pick(kind, to string) error {
	var err error
	if kind == "model" {
		err = a.post("model", map[string]string{"model": picksModelID[to]})
	} else {
		err = a.post("effort", map[string]string{"effort": to})
	}
	if err != nil {
		if !a.full {
			return fmt.Errorf("%s %s with room on the disk: %w", kind, to, err)
		}
		a.toast = kind + ":" + to
		return nil
	}
	a.toast = ""
	st, err := a.state()
	if err != nil {
		return err
	}
	model, effort := st.row.Model, st.row.Effort
	if kind == "model" {
		model = picksModelID[to]
	} else {
		effort = to
	}
	return a.waitTold(picksAbstract(model), effort)
}

func (a *metaSaveFailurePicksAdapter) PickModel() error {
	if !a.gate.pass(true) {
		return nil
	}
	st, err := a.state()
	if err != nil {
		return err
	}
	to := "b"
	if picksAbstract(st.row.Model) == "b" {
		to = "a"
	}
	return a.pick("model", to)
}

func (a *metaSaveFailurePicksAdapter) PickEffort() error {
	if !a.gate.pass(true) {
		return nil
	}
	st, err := a.state()
	if err != nil {
		return err
	}
	to := "high"
	if st.row.Effort == "high" {
		to = "low"
	}
	return a.pick("effort", to)
}

func (a *metaSaveFailurePicksAdapter) ToastRetry() error {
	if !a.gate.pass(a.toast != "") {
		return nil
	}
	kind, to, _ := strings.Cut(a.toast, ":")
	return a.pick(kind, to)
}

func (a *metaSaveFailurePicksAdapter) ServeRestart() error {
	if !a.gate.pass(true) {
		return nil
	}
	a.s.Shutdown()
	return a.s.Resume("")
}

func (a *metaSaveFailurePicksAdapter) gateOff() bool { return a.gate.off }

func picksAction(f func(*metaSaveFailurePicksAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*metaSaveFailurePicksAdapter)
		err := f(a)
		if !a.gate.off {
			a.stepsRun++
		}
		return nil, err
	}
}

var metaSaveFailurePicksActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"DiskFills":    picksAction((*metaSaveFailurePicksAdapter).DiskFills),
	"DiskFrees":    picksAction((*metaSaveFailurePicksAdapter).DiskFrees),
	"Prompt":       picksAction((*metaSaveFailurePicksAdapter).Prompt),
	"PickModel":    picksAction((*metaSaveFailurePicksAdapter).PickModel),
	"PickEffort":   picksAction((*metaSaveFailurePicksAdapter).PickEffort),
	"ToastRetry":   picksAction((*metaSaveFailurePicksAdapter).ToastRetry),
	"ServeRestart": picksAction((*metaSaveFailurePicksAdapter).ServeRestart),
}}

// metaSaveFailurePicksHistory reads the picks the child was told off a
// transcript: every "/model" and "/think" command after Init's first
// two. A pick whose save failed tells the child nothing, so each one
// recorded succeeded, and a success always moves the value away from
// the one before it; that is PickModel (or PickEffort) whatever the page
// clicked, the picker or the toast's Retry. Prompts and restarts are
// left out: neither changes what the child was told.
func metaSaveFailurePicksHistory(entries []history.Entry) []tracecheck.Step {
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Session#0.childModel": "a", "Session#0.childEffort": "low"}}}
	seenModel, seenEffort := false, false
	for _, e := range entries {
		if e.Kind != "command" {
			continue
		}
		text, _ := e.Data["text"].(string)
		if m, ok := strings.CutPrefix(text, "/model "); ok {
			if seenModel {
				steps = append(steps, tracecheck.Step{Action: "Session#0.PickModel", State: map[string]any{"Session#0.childModel": picksAbstract(m)}})
			}
			seenModel = true
		}
		if l, ok := strings.CutPrefix(text, "/think "); ok {
			if seenEffort {
				steps = append(steps, tracecheck.Step{Action: "Session#0.PickEffort", State: map[string]any{"Session#0.childEffort": l}})
			}
			seenEffort = true
		}
	}
	return steps
}

func init() { historyProjections["meta_save_failure_picks"] = metaSaveFailurePicksHistory }

func checkPicksHistory(t *testing.T, a *metaSaveFailurePicksAdapter) {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join("..", "testdata", "meta_save_failure_picks"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), metaSaveFailurePicksHistory)
	}
}

func TestMetaSaveFailurePicks(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newMetaSaveFailurePicksAdapter(t, "r")
	if err := runMBT(t, "meta_save_failure_picks", a, metaSaveFailurePicksActions, map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkPicksHistory(t, a)
}

func TestMetaSaveFailurePicksPaths(t *testing.T) {
	t.Parallel()
	walks := msfWalks(t, "meta_save_failure_picks", envCover())
	msfShards(t, walks, func(t *testing.T, name string, mine []msfWalk) {
		a := newMetaSaveFailurePicksAdapter(t, name)
		if err := walkMSF(a, metaSaveFailurePicksActions["Session"], mine); err != nil {
			t.Fatal(err)
		}
		t.Logf("%d walks, %d steps", len(mine), a.stepsRun)
		checkPicksHistory(t, a)
	})
}

func TestMetaSaveFailurePicksPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	walks := msfWalks(t, "meta_save_failure_picks", tracecheck.CoverStates)
	a := newMetaSaveFailurePicksAdapter(t, "w")
	a.fillNoop = true
	if err := walkMSF(a, metaSaveFailurePicksActions["Session"], walks); err == nil {
		t.Fatal("a paths walk whose disk never fills passed; the walk is not checking state")
	}
}

// A pick the child was told twice in a row to the same model is not a
// path: every recorded pick moves the value.
func TestMetaSaveFailurePicksHistoryRejects(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(filepath.Join("..", "testdata", "meta_save_failure_picks"))
	if err != nil {
		t.Fatal(err)
	}
	cmd := func(text string) history.Entry {
		return history.Entry{Kind: "command", Data: map[string]any{"text": text}}
	}
	init := []history.Entry{cmd("/model ma"), cmd("/think low")}
	if v := g.Check(metaSaveFailurePicksHistory(append(init, cmd("/model mb"), cmd("/think high"), cmd("/model ma")))); v != nil {
		t.Fatalf("alternating picks are a path in the model: %v", v)
	}
	if g.Check(metaSaveFailurePicksHistory(append(init, cmd("/model mb"), cmd("/model mb")))) == nil {
		t.Fatal("the same model told twice in a row passed the trace check")
	}
}
