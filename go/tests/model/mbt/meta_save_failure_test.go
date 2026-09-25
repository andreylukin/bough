//go:build !windows

package mbt

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"reflect"
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

// specs/meta_save_failure.fizz against a real serve: every verb that
// writes meta.json (archive, unarchive, project filing, ack, project
// delete) with the save failing, and what the page makes of the answer.
//
// A full disk is a directory where serve writes meta.json.tmp: the save
// fails as ENOSPC would, and nothing else serve or the child writes is
// touched (history is the child's, a different file). The page is the
// adapter: pageMark, toast and silentFails are its own, and it acts as
// app.tsx's act() does (success clears the toast, failure sets it, and
// either way the list is read again). Its auto-ack reports a failure
// and does not post again while that report is up, which is the spec's
// page; whether app.tsx does is the browser layer's to check, so
// silentFails stays 0 here.
//
// The rest is read off the server on every GetState: archived, project,
// live and mark off the session's row (serve's memory), archivedDisk,
// projectDisk and markDisk off meta.json and the transcript (what a
// restart would load), projectExists off ~/.bough/projects/p.

const msfSlug = "p"

type metaSaveFailureAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	id                  string
	pageMark, toast     string
	silentFails         int
	turn                int
	ids                 []string
	name                string // turn name prefix: unique per adapter, the queue is per serve
	full                bool   // the adapter's record of diskFull, for the requires
	stepsRun, restarted int

	// fillNoop is the deliberate bug the wrong-adapter test injects:
	// DiskFills blocks nothing, so every save still succeeds.
	fillNoop bool
}

func newMetaSaveFailureAdapter(t *testing.T, name string) *metaSaveFailureAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &metaSaveFailureAdapter{t: t, s: s, dir: control.Dir(s.Home), name: name}
}

// metaTmp is where saveMetaLocked writes before its rename: a directory
// there fails every save.
func (a *metaSaveFailureAdapter) metaTmp() string {
	return filepath.Join(a.s.Home, ".bough", "serve", "meta.json.tmp")
}

func (a *metaSaveFailureAdapter) projectDir() string {
	return filepath.Join(a.s.Home, ".bough", "projects", msfSlug)
}

func (a *metaSaveFailureAdapter) Init() error {
	if err := os.RemoveAll(a.metaTmp()); err != nil {
		return err
	}
	a.full = false
	ctx, cancel := actionCtx()
	defer cancel()
	if _, err := os.Stat(a.projectDir()); errors.Is(err, os.ErrNotExist) {
		var r struct {
			Project serve.Project `json:"project"`
		}
		if err := msfCall(ctx, a.s, http.MethodPost, "/api/projects", map[string]string{"name": msfSlug}, &r); err != nil {
			return err
		}
		if r.Project.Slug != msfSlug {
			return fmt.Errorf("project %q got slug %q", msfSlug, r.Project.Slug)
		}
	}
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	// Create leases a child at once; the spec starts from a session with
	// none. Every verb 404s until the child has written its history file.
	if _, err := waitRow(a.s, row.ID, "the new session's file", func(serve.Row) bool { return true }); err != nil {
		return err
	}
	if _, err := a.s.Archive(ctx, row.ID); err != nil {
		return err
	}
	if _, err := a.s.Unarchive(ctx, row.ID); err != nil {
		return err
	}
	a.id, a.pageMark, a.toast, a.silentFails = row.ID, "", "", 0
	a.gate.reset()
	a.ids = append(a.ids, row.ID)
	return nil
}

// Cleanup frees the disk and archives the walk's session, so its child
// is not left running beside the next walk's.
func (a *metaSaveFailureAdapter) Cleanup() error {
	if err := os.RemoveAll(a.metaTmp()); err != nil {
		return err
	}
	a.full = false
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Archive(ctx, a.id)
	return err
}

func (a *metaSaveFailureAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

// msfState is the server's side of the spec's state.
type msfState struct {
	archived, archivedDisk, live, projectExists bool
	project, projectDisk, mark, markDisk        string
}

func (a *metaSaveFailureAdapter) state() (msfState, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return msfState{}, err
	}
	disk, err := savedMeta(a.s.Home, a.id)
	if err != nil {
		return msfState{}, err
	}
	entries, err := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
	if err != nil {
		return msfState{}, err
	}
	_, statErr := os.Stat(a.projectDir())
	return msfState{
		archived: row.Archived, archivedDisk: disk.Archived,
		live:    row.Live,
		project: row.Project, projectDisk: disk.Project,
		projectExists: statErr == nil,
		mark:          rowMark(row),
		markDisk:      markFrom(row.Status, entries, disk.Ack),
	}, nil
}

// rowMark is the spec's mark for a row: its trouble or its unseen dot.
func rowMark(r serve.Row) string {
	switch {
	case r.Trouble != "":
		return "trouble"
	case r.Unseen:
		return "unseen"
	}
	return ""
}

// markFrom is the mark serve would derive with ack as meta.Ack (see
// rowDigest.troubled and unseen): trouble when the last turn failed and
// something is newer than the ack, other than the turn's trailing
// summary and title; unseen when it finished cleanly after the ack.
func markFrom(st serve.Status, entries []history.Entry, ack int64) string {
	var last, done int64
	for i := len(entries) - 1; i >= 0; i-- {
		if k := entries[i].Kind; k != "turn-summary" && k != "title" {
			last = entries[i].Seq
			break
		}
	}
	for i := len(entries) - 1; i >= 0; i-- {
		if entries[i].Kind == "done" {
			done = entries[i].Seq
			break
		}
	}
	switch {
	case (st == serve.StatusError || st == serve.StatusInterrupted) && last > ack:
		return "trouble"
	case st == serve.StatusDone && done > ack:
		return "unseen"
	}
	return ""
}

// savedMeta is the session's meta as meta.json on disk has it: what a
// restart would load.
func savedMeta(home, id string) (serve.SessionMeta, error) {
	p := filepath.Join(home, ".bough", "serve", "meta.json")
	b, err := os.ReadFile(p)
	if errors.Is(err, os.ErrNotExist) {
		return serve.SessionMeta{}, nil
	}
	if err != nil {
		return serve.SessionMeta{}, err
	}
	var f struct {
		Sessions map[string]serve.SessionMeta `json:"sessions"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		return serve.SessionMeta{}, fmt.Errorf("parse %s: %w", p, err)
	}
	return f.Sessions[id], nil
}

func (a *metaSaveFailureAdapter) GetState() (map[string]any, error) {
	st, err := a.state()
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"diskFull": a.full,
		"archived": st.archived, "archivedDisk": st.archivedDisk,
		"project": st.project, "projectDisk": st.projectDisk,
		"live": st.live, "projectExists": st.projectExists,
		"mark": st.mark, "markDisk": st.markDisk,
		"pageMark": a.pageMark, "toast": a.toast, "silentFails": a.silentFails,
	}, nil
}

// enabled reads a require off the server's state; a read that fails
// closes the gate, and the step reports nothing.
func (a *metaSaveFailureAdapter) enabled(require func(msfState) bool) (msfState, bool) {
	if a.gate.off {
		return msfState{}, false
	}
	st, err := a.state()
	if err != nil {
		a.gate.off = true
		return st, false
	}
	return st, a.gate.pass(require(st))
}

func (a *metaSaveFailureAdapter) DiskFills() error {
	if !a.gate.pass(!a.full) {
		return nil
	}
	a.full = true
	if a.fillNoop {
		return nil
	}
	return os.Mkdir(a.metaTmp(), 0o755)
}

func (a *metaSaveFailureAdapter) DiskFrees() error {
	if !a.gate.pass(a.full) {
		return nil
	}
	a.full = false
	return os.RemoveAll(a.metaTmp())
}

func (a *metaSaveFailureAdapter) Finish() error { return a.runTurn(false) }
func (a *metaSaveFailureAdapter) Fail() error   { return a.runTurn(true) }

// runTurn is the composer's Send and the turn it starts, run to its end.
// The turn is found by its text: waiting on status alone would take the
// previous turn's done for this one's.
func (a *metaSaveFailureAdapter) runTurn(fail bool) error {
	if _, ok := a.enabled(func(s msfState) bool { return !s.archived }); !ok {
		return nil
	}
	a.turn++
	name := fmt.Sprintf("%s%05d", a.name, a.turn)
	turn, want := control.Turn{Mode: "ok", Text: "answered " + name}, serve.StatusDone
	if fail {
		turn, want = control.Turn{Mode: "error", Error: "failed " + name}, serve.StatusError
	}
	control.Queue(a.t, a.dir, name, turn)
	text := "turn " + name
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, text); err != nil {
		return err
	}
	var last string
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
			if closed && row.Status == want && row.Live {
				return nil
			}
			last = fmt.Sprintf("status %s live %v input %v closed %v", row.Status, row.Live, in >= 0, closed)
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("waiting for %q to end %s: %s", text, want, last)
		case <-time.After(50 * time.Millisecond):
		}
	}
}

// settle is the end of app.tsx act(): the toast says whether the verb
// failed, and the page reads the list again. An error the full disk does
// not explain is the step's failure, not a toast.
func (a *metaSaveFailureAdapter) settle(err error, label string) error {
	if err != nil && !a.full {
		return fmt.Errorf("%s with room on the disk: %w", label, err)
	}
	if err != nil {
		a.toast = label
	} else {
		a.toast = ""
	}
	st, serr := a.state()
	if serr != nil {
		return serr
	}
	a.pageMark = st.mark
	return nil
}

func (a *metaSaveFailureAdapter) post(verb string, body any) error {
	ctx, cancel := actionCtx()
	defer cancel()
	return msfCall(ctx, a.s, http.MethodPost, "/api/sessions/"+url.PathEscape(a.id)+"/"+verb, body, nil)
}

func (a *metaSaveFailureAdapter) archive() error { return a.settle(a.post("archive", nil), "archive") }
func (a *metaSaveFailureAdapter) unarchive() error {
	return a.settle(a.post("unarchive", nil), "unarchive")
}
func (a *metaSaveFailureAdapter) assign(to string) error {
	return a.settle(a.post("project", map[string]string{"project": to}), "assign:"+to)
}
func (a *metaSaveFailureAdapter) ack(label string) error { return a.settle(a.post("ack", nil), label) }
func (a *metaSaveFailureAdapter) deleteProject() error {
	ctx, cancel := actionCtx()
	defer cancel()
	return a.settle(msfCall(ctx, a.s, http.MethodDelete, "/api/projects/"+msfSlug, nil, nil), "delete")
}

func (a *metaSaveFailureAdapter) Archive() error {
	if _, ok := a.enabled(func(s msfState) bool { return !s.archived }); !ok {
		return nil
	}
	return a.archive()
}

func (a *metaSaveFailureAdapter) Unarchive() error {
	if _, ok := a.enabled(func(s msfState) bool { return s.archived }); !ok {
		return nil
	}
	return a.unarchive()
}

func (a *metaSaveFailureAdapter) AssignProject() error {
	if _, ok := a.enabled(func(s msfState) bool { return s.project == "" && s.projectExists }); !ok {
		return nil
	}
	return a.assign(msfSlug)
}

func (a *metaSaveFailureAdapter) UnassignProject() error {
	if _, ok := a.enabled(func(s msfState) bool { return s.project == msfSlug }); !ok {
		return nil
	}
	return a.assign("")
}

func (a *metaSaveFailureAdapter) MarkSeen() error {
	if !a.gate.pass(a.pageMark == "trouble") {
		return nil
	}
	return a.ack("ack")
}

func (a *metaSaveFailureAdapter) MarkAllSeen() error {
	if !a.gate.pass(a.pageMark == "trouble") {
		return nil
	}
	return a.ack("ackall")
}

// AutoAck is the page acking the unseen finish it has on screen. A
// failure is reported and not posted again while that report is up; a
// success clears the page's copy of the mark and leaves any other
// verb's toast alone.
func (a *metaSaveFailureAdapter) AutoAck() error {
	if !a.gate.pass(a.pageMark == "unseen" && a.toast != "ack" && a.toast != "ackall") {
		return nil
	}
	if err := a.post("ack", nil); err != nil {
		if !a.full {
			return fmt.Errorf("auto-ack with room on the disk: %w", err)
		}
		a.toast = "ack"
		return nil
	}
	a.silentFails, a.pageMark = 0, ""
	return nil
}

func (a *metaSaveFailureAdapter) DeleteProject() error {
	if _, ok := a.enabled(func(s msfState) bool { return s.projectExists }); !ok {
		return nil
	}
	return a.deleteProject()
}

func (a *metaSaveFailureAdapter) ToastRetry() error {
	if !a.gate.pass(a.toast != "") {
		return nil
	}
	switch t := a.toast; t {
	case "archive":
		return a.archive()
	case "unarchive":
		return a.unarchive()
	case "assign:" + msfSlug:
		return a.assign(msfSlug)
	case "assign:":
		return a.assign("")
	case "delete":
		return a.deleteProject()
	default:
		return a.ack(t)
	}
}

func (a *metaSaveFailureAdapter) ListPoll() error {
	st, ok := a.enabled(func(s msfState) bool { return a.pageMark != s.mark })
	if !ok {
		return nil
	}
	a.pageMark = st.mark
	return nil
}

// ServeRestart stops serve with SIGTERM and starts it again on the same
// HOME: its children die with it, and it loads meta.json.
func (a *metaSaveFailureAdapter) ServeRestart() error {
	if !a.gate.pass(true) {
		return nil
	}
	a.s.Shutdown()
	a.restarted++
	return a.s.Resume("")
}

func msfAction(f func(*metaSaveFailureAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*metaSaveFailureAdapter)
		err := f(a)
		if !a.gate.off {
			a.stepsRun++
		}
		return nil, err
	}
}

var metaSaveFailureActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"DiskFills":       msfAction((*metaSaveFailureAdapter).DiskFills),
	"DiskFrees":       msfAction((*metaSaveFailureAdapter).DiskFrees),
	"Finish":          msfAction((*metaSaveFailureAdapter).Finish),
	"Fail":            msfAction((*metaSaveFailureAdapter).Fail),
	"Archive":         msfAction((*metaSaveFailureAdapter).Archive),
	"Unarchive":       msfAction((*metaSaveFailureAdapter).Unarchive),
	"AssignProject":   msfAction((*metaSaveFailureAdapter).AssignProject),
	"UnassignProject": msfAction((*metaSaveFailureAdapter).UnassignProject),
	"MarkSeen":        msfAction((*metaSaveFailureAdapter).MarkSeen),
	"MarkAllSeen":     msfAction((*metaSaveFailureAdapter).MarkAllSeen),
	"AutoAck":         msfAction((*metaSaveFailureAdapter).AutoAck),
	"DeleteProject":   msfAction((*metaSaveFailureAdapter).DeleteProject),
	"ToastRetry":      msfAction((*metaSaveFailureAdapter).ToastRetry),
	"ListPoll":        msfAction((*metaSaveFailureAdapter).ListPoll),
	"ServeRestart":    msfAction((*metaSaveFailureAdapter).ServeRestart),
}}

// metaSaveFailureHistory reads the turns off a transcript: nothing else
// the flow does is written to history (meta.json is serve's, not the
// session's), and every turn is a Finish or a Fail, told apart by the
// error entry a failed one records before its done.
func metaSaveFailureHistory(entries []history.Entry) []tracecheck.Step {
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Session#0.mark": ""}}}
	open, failed := false, false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			open, failed = true, false
		case "error":
			failed = true
		case "done":
			if !open {
				continue
			}
			open = false
			if failed {
				steps = append(steps, tracecheck.Step{Action: "Session#0.Fail", State: map[string]any{"Session#0.mark": "trouble"}})
			} else {
				steps = append(steps, tracecheck.Step{Action: "Session#0.Finish", State: map[string]any{"Session#0.mark": "unseen"}})
			}
		}
	}
	return steps
}

func init() { historyProjections["meta_save_failure"] = metaSaveFailureHistory }

// TestMetaSaveFailure is the fizzbee-mbt random run (the exhaustive
// job's); the paths walk below is what covers the graph on every run.
func TestMetaSaveFailure(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newMetaSaveFailureAdapter(t, "r")
	if err := runMBT(t, "meta_save_failure", a, metaSaveFailureActions, map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkMetaSaveFailureHistory(t, a)
}

func checkMetaSaveFailureHistory(t *testing.T, a *metaSaveFailureAdapter) {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join("..", "testdata", "meta_save_failure"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), metaSaveFailureHistory)
	}
}

// TestMetaSaveFailurePaths walks every generated walk over the checked-
// in graph against real serves, comparing the whole state after every
// step, and then replays each session's transcript on the graph. The
// walks are split over a few serves at once: MODEL_COVER=transitions is
// thousands of steps, a turn or a restart each.
func TestMetaSaveFailurePaths(t *testing.T) {
	t.Parallel()
	walks := msfWalks(t, "meta_save_failure", envCover())
	msfShards(t, walks, func(t *testing.T, name string, mine []msfWalk) {
		a := newMetaSaveFailureAdapter(t, name)
		if err := walkMSF(a, metaSaveFailureActions["Session"], mine); err != nil {
			t.Fatal(err)
		}
		t.Logf("%d walks, %d steps, %d restarts", len(mine), a.stepsRun, a.restarted)
		checkMetaSaveFailureHistory(t, a)
	})
}

// A fault that injects nothing (every save succeeds) must fail the walk:
// else a green run could be a run in which no save ever failed.
func TestMetaSaveFailurePathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	walks := msfWalks(t, "meta_save_failure", tracecheck.CoverStates)
	a := newMetaSaveFailureAdapter(t, "w")
	a.fillNoop = true
	if err := walkMSF(a, metaSaveFailureActions["Session"], walks); err == nil {
		t.Fatal("a paths walk whose disk never fills passed; the walk is not checking state")
	}
}

// The trace check must reject a transcript that is not a path: a failed
// turn read as a clean finish claims a mark the graph never shows after
// Fail.
func TestMetaSaveFailureHistoryRejects(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(filepath.Join("..", "testdata", "meta_save_failure"))
	if err != nil {
		t.Fatal(err)
	}
	ok := []tracecheck.Step{
		{Action: "Init", State: map[string]any{"Session#0.mark": ""}},
		{Action: "Session#0.Fail", State: map[string]any{"Session#0.mark": "trouble"}},
		{Action: "Session#0.Finish", State: map[string]any{"Session#0.mark": "unseen"}},
	}
	if v := g.Check(ok); v != nil {
		t.Fatalf("a Fail then a Finish is a path in the model: %v", v)
	}
	bad := []tracecheck.Step{ok[0], {Action: "Session#0.Fail", State: map[string]any{"Session#0.mark": "unseen"}}}
	if g.Check(bad) == nil {
		t.Fatal("a Fail that leaves the row unseen passed the trace check")
	}
}

// msfWalk is one generated walk's trace.
type msfWalk struct {
	Trace []tracecheck.Step `json:"trace"`
}

func msfWalks(t *testing.T, spec string, cover tracecheck.Cover) []msfWalk {
	t.Helper()
	b, err := pathsJSONCover(spec, cover)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []msfWalk `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		t.Fatal(err)
	}
	if len(doc.Paths) == 0 {
		t.Fatalf("no walks over testdata/%s", spec)
	}
	return doc.Paths
}

// msfShards runs walks over up to four serves at once, walk i on shard
// i mod n, each shard its own subtest.
func msfShards(t *testing.T, walks []msfWalk, run func(t *testing.T, name string, mine []msfWalk)) {
	n := min(4, (len(walks)+2)/3)
	for k := range n {
		var mine []msfWalk
		for i := k; i < len(walks); i += n {
			mine = append(mine, walks[i])
		}
		name := fmt.Sprintf("s%d", k)
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			run(t, name, mine)
		})
	}
}

// msfModel is what walkMSF drives: the adapter as the runner sees it.
type msfModel interface {
	fmbt.Model
	GetState() (map[string]any, error)
}

// walkMSF drives m down each walk, comparing every field the spec's
// state names with the adapter's after every step.
func walkMSF(m msfModel, acts map[string]fmbt.ActionFunc, walks []msfWalk) error {
	for wi, w := range walks {
		if err := m.Init(); err != nil {
			return fmt.Errorf("walk %d: Init: %w", wi, err)
		}
		var names []string
		for si, step := range w.Trace {
			name := strings.TrimPrefix(step.Action, "Session#0.")
			names = append(names, name)
			if si > 0 {
				f, ok := acts[name]
				if !ok {
					return fmt.Errorf("walk %d: no action %s", wi, name)
				}
				if _, err := f(m, nil); err != nil {
					return fmt.Errorf("walk %d %v: %w", wi, names, err)
				}
				if g, ok := m.(interface{ gateOff() bool }); ok && g.gateOff() {
					return fmt.Errorf("walk %d %v: the adapter found %s not enabled", wi, names, name)
				}
			}
			got, err := m.GetState()
			if err != nil {
				return fmt.Errorf("walk %d %v: %w", wi, names, err)
			}
			gotN, _ := normalizeState(got)
			var diffs []string
			for k, v := range step.State {
				field, ok := strings.CutPrefix(k, "Session#0.")
				if ok && !reflect.DeepEqual(gotN[field], v) {
					diffs = append(diffs, fmt.Sprintf("%s is %v, the spec says %v", field, gotN[field], v))
				}
			}
			if len(diffs) > 0 {
				return fmt.Errorf("walk %d %v: %s", wi, names, strings.Join(diffs, "; "))
			}
		}
		if c, ok := m.(interface{ Cleanup() error }); ok {
			if err := c.Cleanup(); err != nil {
				return fmt.Errorf("walk %d %v: Cleanup: %w", wi, names, err)
			}
		}
	}
	return nil
}

func (a *metaSaveFailureAdapter) gateOff() bool { return a.gate.off }

// normalizeState round-trips a state through JSON, so an int compares
// equal to the float64 the graph decodes to.
func normalizeState(s map[string]any) (map[string]any, error) {
	b, err := json.Marshal(s)
	if err != nil {
		return nil, err
	}
	var out map[string]any
	return out, json.Unmarshal(b, &out)
}

// msfCall is one call to serve's HTTP API as the page makes it; servetest
// has no project or filing verbs.
func msfCall(ctx context.Context, s *servetest.Server, method, path string, body, out any) error {
	var rd io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return err
		}
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, s.URL+path, rd)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+s.Token)
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return err
	}
	if resp.StatusCode/100 != 2 {
		return &servetest.APIError{Status: resp.StatusCode, Msg: strings.TrimSpace(string(raw))}
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(raw, out)
}
