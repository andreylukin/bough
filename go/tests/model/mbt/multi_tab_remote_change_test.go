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
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/multi_tab_remote_change.fizz against a real serve: tab A acts on
// one web session (archive, unarchive, rename, stop, ack) while tab B
// keeps the copy of the row it last read and acts on that copy.
//
// Both tabs are the adapter. A's actions are single requests. B's copy
// of the row (bArchived, bTitle, bStatus, bUnseen) is what the adapter
// last read off GET /api/sessions/{id}; bVisible, bFresh and bRefused
// are B's page, kept by the adapter the way app.tsx keeps them. The
// event stream is not opened: BEvent copies the row's status, which is
// all the stream carries; the browser layer drives the real one.
//
// The ack is split where Supervisor.Acknowledge was: the read of what
// the tab shows (AAckRead, BAckRead: the adapter notes the seq of the
// finish it saw) and the save (AAckSave, BAckSave: POST /ack with that
// seq). That is what lets a walk hold an ack in flight while a new
// finish and the other tab's ack land.
//
// seq and ack are the spec's counts of clean finishes: a "done" that
// closes an open turn (the done the loop writes after a cancel is
// bookkeeping, and a killed turn writes none). ack is how many of them
// meta.Ack covers.

// mtrRenamed is the spec's "t1" on the wire. Any other name (the empty
// one a new session has, or the one a turn writes) is the spec's "t0":
// the spec renames once, and a turn's title is not its business.
const mtrRenamed = "t1"

type mtrServer struct {
	archived bool
	title    string
	status   string
	seq, ack int
	unseen   bool
	dones    []int64 // the history seq of each clean finish
}

type multiTabRemoteChangeAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	id   string
	ids  []string
	turn int
	held string // the model request in flight, "" when none

	// Tab B's page.
	bArchived, bUnseen, bVisible, bFresh, bRefused bool
	bTitle, bStatus                                string
	// Acks in flight: the spec's count the read saw (-1 when none) and
	// the history seq of that finish, which the save posts.
	aAck, bAck       int
	aAckSeq, bAckSeq int64

	// menuFromServer is the deliberate bug the wrong-adapter test
	// injects: B's archive menu picks Archive or Unarchive from the
	// server's row instead of B's copy.
	menuFromServer bool
}

func newMultiTabRemoteChangeAdapter(t *testing.T) *multiTabRemoteChangeAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &multiTabRemoteChangeAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

func (a *multiTabRemoteChangeAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	// Every verb 404s until the child has written its history file.
	if _, err := waitRow(a.s, row.ID, "the new session's file", func(serve.Row) bool { return true }); err != nil {
		return err
	}
	a.id, a.held = row.ID, ""
	a.ids = append(a.ids, row.ID)
	a.gate.reset()
	a.aAck, a.bAck = -1, -1
	a.bVisible = true
	return a.bRead()
}

// Cleanup releases a held turn and archives the session, which ends its
// child: hundreds of walks must not leave hundreds of idle children.
func (a *multiTabRemoteChangeAdapter) Cleanup() error {
	if a.held != "" {
		control.Release(a.t, a.dir, a.held)
		a.held = ""
	}
	if a.id == "" {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Archive(ctx, a.id)
	return err
}

func (a *multiTabRemoteChangeAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

// server reads the spec's server fields: the row, meta.Ack as serve
// saved it, and the transcript.
func (a *multiTabRemoteChangeAdapter) server() (mtrServer, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return mtrServer{}, err
	}
	ack, err := a.savedAck()
	if err != nil {
		return mtrServer{}, err
	}
	entries, err := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
	if err != nil {
		return mtrServer{}, err
	}
	st := mtrServer{
		archived: row.Archived,
		title:    mtrTitle(row.Title),
		status:   mtrStatus(row.Status),
		unseen:   row.Unseen,
		dones:    cleanDones(entries),
	}
	st.seq = len(st.dones)
	for _, d := range st.dones {
		if d <= ack {
			st.ack++
		}
	}
	return st, nil
}

func mtrTitle(s string) string {
	if s == mtrRenamed {
		return "t1"
	}
	return "t0"
}

// mtrStatus folds serve's statuses into the spec's. A turn killed by an
// archive reads "interrupted" (open, no child) and a cancelled one
// "stopped": both are the spec's stopped. Anything the spec does not
// name passes through and shows as a mismatch.
func mtrStatus(s serve.Status) string {
	switch s {
	case serve.StatusRunning:
		return "working"
	case serve.StatusInterrupted:
		return "stopped"
	}
	return string(s)
}

// cleanDones is the seq of every "done" that closes an open turn, as
// StatusOf scans it: the done after a cancel closes nothing.
func cleanDones(entries []history.Entry) []int64 {
	var out []int64
	open := false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			open = true
		case "cancelled":
			open = false
		case "done":
			if open {
				out = append(out, e.Seq)
			}
			open = false
		}
	}
	return out
}

// savedAck is meta.Ack as serve wrote it to meta.json (0 when unset).
func (a *multiTabRemoteChangeAdapter) savedAck() (int64, error) {
	b, err := os.ReadFile(filepath.Join(a.s.Home, ".bough", "serve", "meta.json"))
	if os.IsNotExist(err) {
		return 0, nil
	}
	if err != nil {
		return 0, err
	}
	var m struct {
		Sessions map[string]struct {
			Ack int64 `json:"ack"`
		} `json:"sessions"`
	}
	if err := json.Unmarshal(b, &m); err != nil {
		return 0, fmt.Errorf("meta.json: %w", err)
	}
	return m.Sessions[a.id].Ack, nil
}

func (a *multiTabRemoteChangeAdapter) GetState() (map[string]any, error) {
	sv, err := a.server()
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"archived": sv.archived, "title": sv.title, "status": sv.status,
		"seq": sv.seq, "ack": sv.ack, "unseen": sv.unseen,
		"bArchived": a.bArchived, "bTitle": a.bTitle, "bStatus": a.bStatus, "bUnseen": a.bUnseen,
		"bVisible": a.bVisible, "bFresh": a.bFresh, "bRefused": a.bRefused,
		"aAck": a.aAck, "bAck": a.bAck,
	}, nil
}

// enabled reads a require off the server's view; a failed read closes
// the gate like a disabled pick.
func (a *multiTabRemoteChangeAdapter) enabled(require func(mtrServer) bool) (mtrServer, bool) {
	if a.gate.off {
		return mtrServer{}, false
	}
	sv, err := a.server()
	if err != nil {
		a.gate.off = true
		return sv, false
	}
	return sv, a.gate.pass(require(sv))
}

// bRead is B's refresh: the open session's row replaces B's copy.
func (a *multiTabRemoteChangeAdapter) bRead() error {
	sv, err := a.server()
	if err != nil {
		return err
	}
	a.bArchived, a.bTitle, a.bStatus, a.bUnseen = sv.archived, sv.title, sv.status, sv.unseen
	a.bFresh, a.bRefused = true, false
	return nil
}

func (a *multiTabRemoteChangeAdapter) waitStatus(what string, ok func(serve.Status) bool) error {
	_, err := waitRow(a.s, a.id, what, func(r serve.Row) bool { return ok(r.Status) })
	return err
}

func notRunning(s serve.Status) bool { return s != serve.StatusRunning }

// archive is POST /archive; a turn in flight dies with the child.
func (a *multiTabRemoteChangeAdapter) archive(working bool) error {
	ctx, cancel := actionCtx()
	defer cancel()
	if _, err := a.s.Archive(ctx, a.id); err != nil {
		return err
	}
	if !working {
		return nil
	}
	a.dropHeld()
	return a.waitStatus("the archived turn to end", notRunning)
}

// dropHeld lets go of a model request whose child was stopped or killed.
func (a *multiTabRemoteChangeAdapter) dropHeld() {
	if a.held != "" {
		control.Release(a.t, a.dir, a.held)
		a.held = ""
	}
}

// --- tab A ---

func (a *multiTabRemoteChangeAdapter) AArchive() error {
	sv, ok := a.enabled(func(s mtrServer) bool { return !s.archived })
	if !ok {
		return nil
	}
	a.bFresh = false
	return a.archive(sv.status == "working")
}

func (a *multiTabRemoteChangeAdapter) AUnarchive() error {
	if _, ok := a.enabled(func(s mtrServer) bool { return s.archived }); !ok {
		return nil
	}
	a.bFresh = false
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Unarchive(ctx, a.id)
	return err
}

func (a *multiTabRemoteChangeAdapter) ARename() error {
	if _, ok := a.enabled(func(s mtrServer) bool { return s.title == "t0" && s.seq == 0 && s.status == "idle" }); !ok {
		return nil
	}
	a.bFresh = false
	ctx, cancel := actionCtx()
	defer cancel()
	return a.s.Rename(ctx, a.id, mtrRenamed)
}

func (a *multiTabRemoteChangeAdapter) AStop() error {
	if _, ok := a.enabled(func(s mtrServer) bool { return s.status == "working" }); !ok {
		return nil
	}
	a.bFresh = false
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Interrupt(ctx, a.id); err != nil {
		return err
	}
	a.dropHeld()
	return a.waitStatus("the stopped turn", func(s serve.Status) bool {
		return s == serve.StatusStopped || s == serve.StatusInterrupted
	})
}

func (a *multiTabRemoteChangeAdapter) AAckRead() error {
	sv, ok := a.enabled(func(s mtrServer) bool { return s.unseen && a.aAck == -1 })
	if !ok {
		return nil
	}
	a.aAck, a.aAckSeq = sv.seq, sv.dones[sv.seq-1]
	return nil
}

func (a *multiTabRemoteChangeAdapter) AAckSave() error {
	if !a.gate.pass(a.aAck != -1) {
		return nil
	}
	seq := a.aAckSeq
	a.aAck, a.aAckSeq = -1, 0
	a.bFresh = false
	return a.ackSeq(seq)
}

// ackSeq is POST /ack for the finish the tab showed.
func (a *multiTabRemoteChangeAdapter) ackSeq(seq int64) error {
	ctx, cancel := actionCtx()
	defer cancel()
	b, _ := json.Marshal(map[string]int64{"seq": seq})
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.s.URL+"/api/sessions/"+url.PathEscape(a.id)+"/ack", bytes.NewReader(b))
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
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("POST ack: %d %s", resp.StatusCode, raw)
	}
	return nil
}

// --- the session's child ---

func (a *multiTabRemoteChangeAdapter) ChildWritesEntry() error {
	if _, ok := a.enabled(func(s mtrServer) bool { return s.status == "working" }); !ok {
		return nil
	}
	a.bFresh = false
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	return a.waitStatus("done", func(s serve.Status) bool { return s == serve.StatusDone })
}

// --- tab B ---

func (a *multiTabRemoteChangeAdapter) BPollTick() error {
	if !a.gate.pass(a.bVisible && !a.bFresh) {
		return nil
	}
	return a.bRead()
}

func (a *multiTabRemoteChangeAdapter) BEvent() error {
	sv, ok := a.enabled(func(s mtrServer) bool { return !s.archived && a.bStatus != s.status })
	if !ok {
		return nil
	}
	a.bStatus = sv.status
	return nil
}

func (a *multiTabRemoteChangeAdapter) BHide() error {
	if !a.gate.pass(a.bVisible) {
		return nil
	}
	a.bVisible = false
	return nil
}

func (a *multiTabRemoteChangeAdapter) BShow() error {
	if !a.gate.pass(!a.bVisible) {
		return nil
	}
	a.bVisible = true
	return nil
}

// BArchiveMenu is archiveRow on B's copy; both requests are idempotent,
// and act's refresh follows.
func (a *multiTabRemoteChangeAdapter) BArchiveMenu() error {
	sv, ok := a.enabled(func(mtrServer) bool { return a.bVisible })
	if !ok {
		return nil
	}
	archived := a.bArchived
	if a.menuFromServer {
		archived = sv.archived
	}
	if archived {
		ctx, cancel := actionCtx()
		_, err := a.s.Unarchive(ctx, a.id)
		cancel()
		if err != nil {
			return err
		}
	} else if err := a.archive(sv.status == "working"); err != nil {
		return err
	}
	return a.bRead()
}

// BSend is the composer's send. Serve refuses an archived session with
// 409, which deliverTo shows; otherwise a turn starts and is held.
func (a *multiTabRemoteChangeAdapter) BSend() error {
	sv, ok := a.enabled(func(s mtrServer) bool {
		return a.bVisible && !a.bArchived && a.bStatus != "working" && s.seq < 2
	})
	if !ok {
		return nil
	}
	a.turn++
	name := fmt.Sprintf("m%05d", a.turn)
	if !sv.archived {
		control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	}
	ctx, cancel := actionCtx()
	err := a.s.Prompt(ctx, a.id, "turn "+name)
	cancel()
	refused := false
	var apiErr *servetest.APIError
	switch {
	case sv.archived && errors.As(err, &apiErr) && apiErr.Status == http.StatusConflict:
		refused = true
	case sv.archived:
		return fmt.Errorf("a send to an archived session answered %v, not 409", err)
	case err != nil:
		return err
	default:
		a.bFresh = false
		a.held = name
		control.WaitTaken(a.t, a.dir, name, actionTimeout)
		if err := a.waitStatus("running", func(s serve.Status) bool { return s == serve.StatusRunning }); err != nil {
			return err
		}
	}
	if err := a.bRead(); err != nil {
		return err
	}
	a.bRefused = refused
	return nil
}

func (a *multiTabRemoteChangeAdapter) BAckRead() error {
	sv, ok := a.enabled(func(mtrServer) bool { return a.bVisible && a.bUnseen && a.bAck == -1 })
	if !ok {
		return nil
	}
	a.bAck = sv.seq
	if sv.seq > 0 {
		a.bAckSeq = sv.dones[sv.seq-1]
	}
	return nil
}

func (a *multiTabRemoteChangeAdapter) BAckSave() error {
	if !a.gate.pass(a.bAck != -1) {
		return nil
	}
	seq := a.bAckSeq
	a.bAck, a.bAckSeq = -1, 0
	if err := a.ackSeq(seq); err != nil {
		return err
	}
	return a.bRead()
}

var multiTabRemoteChangeActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"AArchive":         action((*multiTabRemoteChangeAdapter).AArchive),
	"AUnarchive":       action((*multiTabRemoteChangeAdapter).AUnarchive),
	"ARename":          action((*multiTabRemoteChangeAdapter).ARename),
	"AStop":            action((*multiTabRemoteChangeAdapter).AStop),
	"AAckRead":         action((*multiTabRemoteChangeAdapter).AAckRead),
	"AAckSave":         action((*multiTabRemoteChangeAdapter).AAckSave),
	"ChildWritesEntry": action((*multiTabRemoteChangeAdapter).ChildWritesEntry),
	"BPollTick":        action((*multiTabRemoteChangeAdapter).BPollTick),
	"BEvent":           action((*multiTabRemoteChangeAdapter).BEvent),
	"BHide":            action((*multiTabRemoteChangeAdapter).BHide),
	"BShow":            action((*multiTabRemoteChangeAdapter).BShow),
	"BArchiveMenu":     action((*multiTabRemoteChangeAdapter).BArchiveMenu),
	"BSend":            action((*multiTabRemoteChangeAdapter).BSend),
	"BAckRead":         action((*multiTabRemoteChangeAdapter).BAckRead),
	"BAckSave":         action((*multiTabRemoteChangeAdapter).BAckSave),
}, "": {
	// The spec turns deadlock detection off, so the runner offers a
	// role-less "end"; a missing one is a nil-pointer panic in the lib
	// (see unseenTroubleAckActions).
	"end": func(m any, _ []fmbt.Arg) (any, error) {
		m.(*multiTabRemoteChangeAdapter).gate.pass(false)
		return nil, errDisabled
	},
}}

func multiTabRemoteChangeOptions() map[string]any {
	return map[string]any{"max-seq-runs": 300, "max-actions": 8, "max-parallel-runs": 0}
}

// multiTabRemoteChangeHistory reads the trace off a transcript. Only the
// turn is in history (an input, a done, a cancel); archive, rename and
// the acks are meta, and B's page is not recorded. So the trace is the
// turn's steps with what they imply: B sent a later prompt only after
// it read that the last turn had ended (a poll), and a turn that closed
// with neither a done nor a cancel was killed by an archive, which was
// undone before the next send.
func multiTabRemoteChangeHistory(entries []history.Entry) []tracecheck.Step {
	state := func(status string, seq int) map[string]any {
		return map[string]any{"Session#0.status": status, "Session#0.seq": seq}
	}
	steps := []tracecheck.Step{{Action: "Init", State: state("idle", 0)}}
	add := func(action string, st map[string]any) {
		steps = append(steps, tracecheck.Step{Action: "Session#0." + action, State: st})
	}
	status, seq, open := "idle", 0, false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			if open {
				add("AArchive", state("stopped", seq))
				add("AUnarchive", state("stopped", seq))
				status = "stopped"
			}
			if status != "idle" {
				add("BPollTick", state(status, seq))
			}
			open, status = true, "working"
			add("BSend", state(status, seq))
		case "cancelled":
			if open {
				open, status = false, "stopped"
				add("AStop", state(status, seq))
			}
		case "done":
			if open {
				open, status = false, "done"
				seq++
				add("ChildWritesEntry", state(status, seq))
			}
		}
	}
	return steps
}

func init() { historyProjections["multi_tab_remote_change"] = multiTabRemoteChangeHistory }

// TestMultiTabRemoteChange lets fizzbee-mbt walk the spec at random; it
// runs only in the exhaustive run (runMBT). The paths are the cover.
func TestMultiTabRemoteChange(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newMultiTabRemoteChangeAdapter(t)
	if err := runMBT(t, "multi_tab_remote_change", a, multiTabRemoteChangeActions, multiTabRemoteChangeOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkMultiTabRemoteChangeHistories(t, a)
}

// walkMultiTabRemoteChangePaths walks the derived paths through the
// adapter, comparing every field after every step. With all, every path
// runs so one run reports every divergence; otherwise it stops at the
// first.
func walkMultiTabRemoteChangePaths(t *testing.T, a *multiTabRemoteChangeAdapter, cover tracecheck.Cover, all bool) error {
	t.Helper()
	b, err := pathsJSONCover("multi_tab_remote_change", cover)
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
	steps := 0
	for _, p := range doc.Paths {
		steps += len(p.Trace)
	}
	t.Logf("%s cover: %d paths, %d steps", cover, len(doc.Paths), steps)
	var errs []error
	for i, p := range doc.Paths {
		if err := a.walk(p.Trace); err != nil {
			var names []string
			for _, s := range p.Trace {
				names = append(names, strings.TrimPrefix(s.Action, "Session#0."))
			}
			errs = append(errs, fmt.Errorf("path %d %v: %w", i, names, err))
			if !all {
				break
			}
		}
	}
	return errors.Join(errs...)
}

func (a *multiTabRemoteChangeAdapter) walk(trace []tracecheck.Step) (err error) {
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	for j, s := range trace {
		if s.Action == "Init" {
			err = a.Init()
		} else {
			_, err = multiTabRemoteChangeActions["Session"][strings.TrimPrefix(s.Action, "Session#0.")](a, nil)
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", j, s.Action, err)
		}
		if a.gate.off {
			return fmt.Errorf("step %d (%s): the adapter says the spec's require does not hold", j, s.Action)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w", j, s.Action, err)
		}
		var diff []string
		for k, v := range s.State {
			f, ok := strings.CutPrefix(k, "Session#0.")
			if !ok {
				continue
			}
			// The graph's ints decode as float64.
			if n, isInt := got[f].(int); isInt {
				if w, ok := v.(float64); ok && float64(n) == w {
					continue
				}
			} else if got[f] == v {
				continue
			}
			diff = append(diff, fmt.Sprintf("%s is %v, the spec says %v", f, got[f], v))
		}
		if len(diff) > 0 {
			return fmt.Errorf("step %d (%s): %s", j, s.Action, strings.Join(diff, "; "))
		}
	}
	return nil
}

// TestMultiTabRemoteChangePaths walks every derived path against one
// real serve: every settled state by default, every link under
// MODEL_COVER=transitions.
func TestMultiTabRemoteChangePaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newMultiTabRemoteChangeAdapter(t)
	if err := walkMultiTabRemoteChangePaths(t, a, envCover(), true); err != nil {
		t.Fatalf("spec path: %v", err)
	}
	checkMultiTabRemoteChangeHistories(t, a)
}

// Every session a walk used left a transcript that is a path too.
func checkMultiTabRemoteChangeHistories(t *testing.T, a *multiTabRemoteChangeAdapter) {
	t.Helper()
	g, err := tracecheck.Load(fizzCheck(t, "multi_tab_remote_change"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), multiTabRemoteChangeHistory)
	}
	t.Logf("trace-checked %d sessions' histories", len(a.ids))
}

// The projection is only a check if a transcript the model forbids is
// refused: a third finish (the spec stops sending at two).
func TestMultiTabRemoteChangeHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(fizzCheck(t, "multi_tab_remote_change"))
	if err != nil {
		t.Fatal(err)
	}
	in := history.Entry{Kind: "input"}
	done := history.Entry{Kind: "done"}
	cancelled := history.Entry{Kind: "cancelled"}
	for name, es := range map[string][]history.Entry{
		"two turns":     {in, done, in, done},
		"stop then one": {in, cancelled, done, in, done},
		"killed":        {in, in, done},
	} {
		if v := g.Check(multiTabRemoteChangeHistory(es)); v != nil {
			t.Errorf("%s: %v", name, v)
		}
	}
	if v := g.Check(multiTabRemoteChangeHistory([]history.Entry{in, done, in, done, in, done})); v == nil {
		t.Error("a third finish passed the trace check")
	} else {
		t.Logf("refused as expected: %v", v)
	}
}

// A green paths test proves nothing unless a wrong client fails it: this
// adapter's archive menu reads the server's row instead of B's copy,
// which shows only on the one link where B is stale about archived.
func TestMultiTabRemoteChangeCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newMultiTabRemoteChangeAdapter(t)
	a.menuFromServer = true
	err := walkMultiTabRemoteChangePaths(t, a, tracecheck.CoverTransitions, false)
	if err == nil {
		t.Fatal("a run whose archive menu ignores B's copy passed; the paths are not checking state")
	}
	t.Logf("caught as expected: %v", err)
}
