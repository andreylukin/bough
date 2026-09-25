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
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/thread_report_routing.fizz against a real serve: where a project
// thread's finish report lands across main's turns, main's history loss
// and re-mint, the archive of main, the running cap and a serve restart.
// One project per walk, its main thread, one thread from the page and
// one other agent holding the running slot.
//
// Every project child runs on the fake container runtime, and every
// model request answers from llm-control's one queue, so a turn is
// queued only right before the request that must take it: anything else
// that asks (main's follow-up on a notice that came mid-turn) finds the
// queue empty and finishes at once.
//
// Two of the spec's states last microseconds in serve, so the adapter
// holds them itself, as a slow scheduler would:
//
//   - "pending" is the report goroutine not having run yet. ThreadFinish
//     keeps the thread's turn held, and Report (or ReportSaveFails)
//     releases it, which is when serve closes the turn and reports. No
//     enabled step can tell the two apart: in pending the spec allows
//     only main's side and the other agent, never the thread's.
//   - ArchiveMain's window is SetArchived between its flag and its kill.
//     ArchiveMain notes it; ArchiveKill (or a restart) makes the one
//     SetArchived call that does both.
//
// The running cap is folded to 1 as the spec does: a thread starts with
// maxRunning 1 (so it queues exactly when the other agent runs), and the
// other agent with 2 (so it runs beside a running thread). A failed
// Reported save is a directory where serve writes meta.json.tmp.
// Everything else is read off the server: main from meta.json's mains,
// the history dir and main's row, the thread from the project page,
// Reported from meta.json, the report from main's transcript.
const trrConfig = "- id: llm\n  plugin: llm-control\n" +
	"- id: orb\n  plugin: orb\n  config:\n    runtime: fake\n"

type trrAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	walk int
	slug string

	mains []string // every main id of this walk, in order: the spec's gen

	thread     string // the thread on the page, "" for none
	lastThread string // the last thread, kept after it is archived
	threadTurn string // the thread's current turn: its reply names it
	tparent    int    // the last thread's parent, kept after it is archived
	dropped    bool   // likewise

	finishing bool   // ThreadFinish taken, its report not yet run
	archiving bool   // ArchiveMain taken, its kill not yet made
	memOnly   bool   // Reported set in serve's memory by a failed save
	threadHel string // the thread's held turn
	mainHeld  string // main's held turn
	slot      string // the agent on the running slot
	slotHeld  string // its held turn
	slotPar   string // its parent: a CLI session with no process
	turn      int
	msg       int

	kids []string // every thread id, for the trace check

	// saveWorks is the deliberate wiring bug the CatchesWrongAdapter
	// test injects: ReportSaveFails lets the save succeed.
	saveWorks bool
	// archiveNow is the random runs' bug: ArchiveMain kills at once,
	// with no window. ArchiveMain is enabled at Init, so their short
	// walks reach it; ReportSaveFails is three exact steps in.
	archiveNow bool
}

func newTRRAdapter(t *testing.T) *trrAdapter {
	s := servetest.Start(t, servetest.Options{Config: trrConfig})
	return &trrAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

// Init starts each walk on a fresh project whose main has had one turn
// and is idle: the spec's initial state.
func (a *trrAdapter) Init() error {
	a.walk++
	a.mains, a.thread, a.lastThread, a.threadTurn = nil, "", "", ""
	a.tparent, a.dropped, a.finishing, a.archiving, a.memOnly = 0, false, false, false, false
	a.threadHel, a.mainHeld, a.slot, a.slotHeld = "", "", "", ""
	a.gate.reset()
	var r struct {
		Project serve.Project `json:"project"`
	}
	if err := a.api(http.MethodPost, "/api/projects", map[string]string{"name": fmt.Sprintf("Walk %d", a.walk)}, &r); err != nil {
		return err
	}
	a.slug = r.Project.Slug
	if err := a.makeSlotParent(); err != nil {
		return err
	}
	name := a.nextTurn()
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "ok", Text: "hello"})
	var out map[string]any
	if err := a.api(http.MethodPost, "/api/projects/"+a.slug+"/message", map[string]any{"text": a.nextMessage()}, &out); err != nil {
		return err
	}
	if err := a.waitTaken(name); err != nil {
		return err
	}
	main, _ := out["main"].(string)
	a.mains = append(a.mains, main)
	return a.mainQuiet()
}

// makeSlotParent writes a local session's file with no process, the
// way a CLI session leaves one: the other agent's report is stored
// there and wakes nobody, so it never takes a turn queued for another.
func (a *trrAdapter) makeSlotParent() error {
	id := history.NewID()
	p := a.histPath(id)
	if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(p, nil, 0o644); err != nil {
		return err
	}
	if _, err := history.AppendFile(p, "meta", map[string]any{"cwd": a.s.Home, "mode": "local"}); err != nil {
		return err
	}
	a.slotPar = id
	return nil
}

// Cleanup lets every held turn go and stops the project and the other
// agent, so nothing of this walk takes the next walk's turns.
func (a *trrAdapter) Cleanup() error {
	for _, h := range []string{a.threadHel, a.mainHeld, a.slotHeld} {
		if h != "" {
			control.Release(a.t, a.dir, h)
		}
	}
	a.threadHel, a.mainHeld, a.slotHeld = "", "", ""
	os.Remove(a.metaTmp())
	os.Remove(filepath.Join(a.dir, "start.exit"))
	control.ReleaseStart(a.t, a.dir)
	var errs []error
	if a.slot != "" {
		errs = append(errs, a.api(http.MethodPost, "/api/sessions/"+url.PathEscape(a.slot)+"/archive", nil, nil))
	}
	if a.slug != "" {
		errs = append(errs, a.api(http.MethodPost, "/api/projects/"+a.slug+"/archive", nil, nil))
	}
	return errors.Join(errs...)
}

func (a *trrAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Project", Index: 0}: a}, nil
}

// --- reading serve

func (a *trrAdapter) meta() (pmtMeta, error) {
	var m pmtMeta
	b, err := os.ReadFile(filepath.Join(a.s.Home, ".bough", "serve", "meta.json"))
	if errors.Is(err, os.ErrNotExist) {
		return m, nil
	}
	if err != nil {
		return m, err
	}
	return m, json.Unmarshal(b, &m)
}

func (a *trrAdapter) metaTmp() string {
	return filepath.Join(a.s.Home, ".bough", "serve", "meta.json.tmp")
}

func (a *trrAdapter) histPath(id string) string {
	return filepath.Join(a.s.Home, ".bough", "history", id+".jsonl")
}

func (a *trrAdapter) hasHistory(id string) bool {
	_, err := os.Stat(a.histPath(id))
	return err == nil
}

func (a *trrAdapter) row(id string) (serve.Row, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, id)
	return row, err
}

// mainState is the spec's main and the id meta.json records for it.
func (a *trrAdapter) mainState() (string, string, error) {
	m, err := a.meta()
	if err != nil {
		return "", "", err
	}
	id := m.Mains[a.slug]
	if id == "" {
		return "", "", fmt.Errorf("project %s has no main recorded", a.slug)
	}
	if !a.hasHistory(id) {
		return "gone", id, nil
	}
	row, err := a.row(id)
	if err != nil {
		return "", "", err
	}
	switch {
	case !row.Live:
		return "down", id, nil
	case row.Status == serve.StatusRunning:
		return "turn", id, nil
	}
	return "idle", id, nil
}

func (a *trrAdapter) mainIs(states ...string) bool {
	st, _, err := a.mainState()
	return err == nil && slices.Contains(states, st)
}

// archived is main's flag, or the adapter inside ArchiveMain's window.
func (a *trrAdapter) archived() bool {
	if a.archiving {
		return true
	}
	m, err := a.meta()
	return err == nil && m.Sessions[m.Mains[a.slug]].Archived
}

func (a *trrAdapter) threadRow() (serve.Row, bool, error) {
	if a.thread == "" {
		return serve.Row{}, false, nil
	}
	var d pmtDetail
	if err := a.api(http.MethodGet, "/api/projects/"+a.slug, nil, &d); err != nil {
		return serve.Row{}, false, err
	}
	for _, r := range d.Threads {
		if r.ID == a.thread {
			return r, true, nil
		}
	}
	return serve.Row{}, false, nil
}

// threadState is the spec's thread, read off the project page.
func (a *trrAdapter) threadState() (string, error) {
	row, ok, err := a.threadRow()
	if err != nil || !ok {
		return "none", err
	}
	if a.finishing {
		if row.Status != serve.StatusRunning {
			return "", fmt.Errorf("thread %s: its report is held but its turn reads %s", a.thread, row.Status)
		}
		return "pending", nil
	}
	switch row.Status {
	case serve.StatusQueued:
		return "queued", nil
	case serve.StatusRunning:
		return "running", nil
	case serve.StatusInterrupted:
		return "interrupted", nil
	case serve.StatusDone, serve.StatusStopped, serve.StatusError:
		return "done", nil
	}
	return string(row.Status), nil
}

func (a *trrAdapter) threadIs(states ...string) bool {
	st, err := a.threadState()
	return err == nil && slices.Contains(states, st)
}

// closedSeq is the seq of the entry that closed a session's last turn,
// 0 while it is open or has none: the key report() records.
func (a *trrAdapter) closedSeq(id string) int64 {
	es, err := history.Read(a.histPath(id))
	if err != nil {
		return 0
	}
	var seq int64
	open, last := false, ""
	for _, e := range es {
		switch e.Kind {
		case "input":
			open, last, seq = true, "", 0
		case "done", "cancelled":
			if e.Kind == "done" && !open && last == "cancelled" {
				continue
			}
			open, last, seq = false, e.Kind, e.Seq
		}
	}
	return seq
}

// disk says meta.json's Reported covers the last thread's closed turn.
func (a *trrAdapter) disk() (bool, error) {
	if a.lastThread == "" {
		return false, nil
	}
	seq := a.closedSeq(a.lastThread)
	if seq == 0 {
		return false, nil
	}
	m, err := a.meta()
	if err != nil {
		return false, err
	}
	return m.Sessions[a.lastThread].Reported >= seq, nil
}

// landed counts the current turn's report in the conversation main's
// recorded id holds: a stored notice not yet delivered, the input it
// woke main with, or the note of one that came mid-turn.
func (a *trrAdapter) landed(main string) int {
	if a.threadTurn == "" {
		return 0
	}
	es, err := history.Read(a.histPath(main))
	if err != nil {
		return 0
	}
	want := trrReply(a.threadTurn)
	delivered := map[string]bool{}
	for _, e := range es {
		if e.Kind == "notice-delivered" {
			id, _ := e.Data["id"].(string)
			delivered[id] = true
		}
	}
	n := 0
	for _, e := range es {
		text, _ := e.Data["text"].(string)
		switch {
		case e.Kind == "notice":
			if id, _ := e.Data["id"].(string); delivered[id] {
				continue
			}
		case e.Kind == "input" && e.Data["reason"] == "notice", e.Kind == "job":
		default:
			continue
		}
		n += strings.Count(text, want)
	}
	return n
}

func trrReply(turn string) string { return "reply-" + turn }

func (a *trrAdapter) gen(id string) int {
	if i := slices.Index(a.mains, id); i >= 0 {
		return i
	}
	a.mains = append(a.mains, id)
	return len(a.mains) - 1
}

// GetState is the Project role's state.
func (a *trrAdapter) GetState() (map[string]any, error) {
	main, mainID, err := a.mainState()
	if err != nil {
		return nil, err
	}
	gen := a.gen(mainID)
	m, err := a.meta()
	if err != nil {
		return nil, err
	}
	thread, err := a.threadState()
	if err != nil {
		return nil, err
	}
	disk, err := a.disk()
	if err != nil {
		return nil, err
	}
	mem := disk || a.memOnly
	if thread != "none" {
		row, _, err := a.threadRow()
		if err != nil {
			return nil, err
		}
		a.tparent = slices.Index(a.mains, row.SpawnedBy)
		a.dropped = mem && !a.hasHistory(row.SpawnedBy)
	}
	landed := 0
	if main != "gone" {
		landed = a.landed(mainID)
	}
	woke := false
	if m.Sessions[mainID].Archived && main != "down" && main != "gone" {
		woke = true
	}
	slot := false
	if a.slot != "" {
		row, err := a.row(a.slot)
		if err != nil {
			return nil, err
		}
		slot = row.Status == serve.StatusRunning
	}
	return map[string]any{
		"main": main, "archived": a.archived(), "gen": gen, "tparent": a.tparent,
		"thread": thread, "landed": landed, "dropped": a.dropped,
		"mem": mem, "disk": disk, "slot": slot, "woke": woke,
	}, nil
}

// --- requests and waits

// api is one JSON call as the page makes it.
func (a *trrAdapter) api(method, path string, body, out any) error {
	ctx, cancel := actionCtx()
	defer cancel()
	var rd io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return err
		}
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
		return fmt.Errorf("%s %s: %w", method, path, err)
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	if resp.StatusCode/100 != 2 {
		return fmt.Errorf("%s %s: %d %s", method, path, resp.StatusCode, raw)
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(raw, out)
}

// until polls ok until it holds or the step's time is up.
func trrUntil(what string, ok func() bool) error {
	deadline := time.Now().Add(actionTimeout)
	for !ok() {
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for %s: not after %s", what, actionTimeout)
		}
		time.Sleep(20 * time.Millisecond)
	}
	return nil
}

func (a *trrAdapter) nextTurn() string {
	a.turn++
	return fmt.Sprintf("r%05d", a.turn)
}

func (a *trrAdapter) nextMessage() string {
	a.msg++
	return fmt.Sprintf("message %d of walk %d", a.msg, a.walk)
}

// queueBlock queues a turn held until it is released, answering with
// the reply that names it.
func (a *trrAdapter) queueBlock() string {
	name := a.nextTurn()
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: trrReply(name)})
	return name
}

func (a *trrAdapter) waitTaken(name string) error {
	return trrUntil("turn "+name+" to be taken", func() bool {
		_, err := os.Stat(filepath.Join(a.dir, name+".taken"))
		return err == nil
	})
}

// mainQuiet waits for main to settle idle: its turn closed, and still
// closed a moment later, since a notice that came mid-turn follows the
// release with a request of its own.
func (a *trrAdapter) mainQuiet() error {
	quiet := 0
	return trrUntil("main to settle", func() bool {
		if a.mainIs("turn") {
			quiet = 0
			return false
		}
		quiet++
		time.Sleep(50 * time.Millisecond)
		return quiet >= 4
	})
}

func (a *trrAdapter) waitThread(want string) error {
	return trrUntil("the thread to read "+want, func() bool { return a.threadIs(want) })
}

// --- actions

func (a *trrAdapter) MessageProject() error {
	if !a.gate.pass(a.mainIs("idle", "down", "gone") && !(a.archived() && a.mainIs("idle"))) {
		return nil
	}
	name := a.queueBlock()
	var out map[string]any
	if err := a.api(http.MethodPost, "/api/projects/"+a.slug+"/message", map[string]any{"text": a.nextMessage()}, &out); err != nil {
		return err
	}
	if err := a.waitTaken(name); err != nil {
		return err
	}
	a.mainHeld = name
	return trrUntil("main in a turn", func() bool { return a.mainIs("turn") })
}

func (a *trrAdapter) MainTurnEnd() error {
	if !a.gate.pass(a.mainIs("turn")) {
		return nil
	}
	if a.mainHeld == "" {
		return fmt.Errorf("main is in a turn the adapter did not hold")
	}
	control.Release(a.t, a.dir, a.mainHeld)
	a.mainHeld = ""
	return a.mainQuiet()
}

func (a *trrAdapter) ArchiveMain() error {
	if !a.gate.pass(a.mainIs("idle", "turn", "down") && !a.archived()) {
		return nil
	}
	if a.mainIs("down") || a.archiveNow {
		return a.archiveMain()
	}
	// SetArchived saves meta.json with the flag, before its kill: an ack
	// of main is that save without the flag, which the spec reads only
	// as disk catching up with a Reported a failed save left in memory.
	_, id, err := a.mainState()
	if err != nil {
		return err
	}
	if err := a.api(http.MethodPost, "/api/sessions/"+url.PathEscape(id)+"/ack", nil, nil); err != nil {
		return err
	}
	a.archiving = true
	return nil
}

// archiveMain is SetArchived: the flag, then the kill.
func (a *trrAdapter) archiveMain() error {
	_, id, err := a.mainState()
	if err != nil {
		return err
	}
	a.archiving = false
	if err := a.api(http.MethodPost, "/api/sessions/"+url.PathEscape(id)+"/archive", nil, nil); err != nil {
		return err
	}
	a.mainHeld = ""
	return trrUntil("main down", func() bool { return a.mainIs("down") })
}

func (a *trrAdapter) ArchiveKill() error {
	if !a.gate.pass(a.archived() && a.mainIs("idle", "turn")) {
		return nil
	}
	return a.archiveMain()
}

// LoseMainHistory deletes main's transcript, the way a person cleaning
// ~/.bough/history would. A live process runs on, writing to nothing.
func (a *trrAdapter) LoseMainHistory() error {
	st, id, err := a.mainState()
	if err != nil {
		return err
	}
	if !a.gate.pass((st == "idle" || st == "down") && !a.archived() && a.thread == "" && a.gen(id) < 1) {
		return nil
	}
	return os.Remove(a.histPath(id))
}

func (a *trrAdapter) slotHeldNow() bool {
	if a.slot == "" {
		return false
	}
	row, err := a.row(a.slot)
	return err == nil && row.Status == serve.StatusRunning
}

func (a *trrAdapter) StartThread() error {
	if !a.gate.pass(a.thread == "" && a.mainIs("idle", "turn") && !a.archived()) {
		return nil
	}
	queued := a.slotHeldNow()
	name := ""
	if !queued {
		name = a.queueBlock()
	} else {
		name = a.nextTurn()
	}
	var out struct {
		Session serve.Row `json:"session"`
		Queued  bool      `json:"queued"`
	}
	if err := a.api(http.MethodPost, "/api/sessions", map[string]any{"mode": "project", "project": a.slug, "prompt": "task " + name, "maxRunning": 1}, &out); err != nil {
		return err
	}
	if out.Session.ID == "" {
		return fmt.Errorf("new thread: no session in the reply")
	}
	a.thread, a.lastThread, a.threadTurn = out.Session.ID, out.Session.ID, name
	a.kids = append(a.kids, a.thread)
	a.memOnly, a.dropped = false, false
	if queued {
		if !out.Queued {
			return fmt.Errorf("thread %s started beside the agent holding the one slot", a.thread)
		}
		return a.waitThread("queued")
	}
	if err := a.waitTaken(name); err != nil {
		return err
	}
	a.threadHel = name
	return a.waitThread("running")
}

func (a *trrAdapter) ThreadMessage() error {
	if !a.gate.pass(a.threadIs("done", "interrupted") && !a.slotHeldNow()) {
		return nil
	}
	name := a.queueBlock()
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.thread, "turn "+name); err != nil {
		return err
	}
	a.threadTurn, a.memOnly = name, false
	if err := a.waitTaken(name); err != nil {
		return err
	}
	a.threadHel = name
	return a.waitThread("running")
}

func (a *trrAdapter) ThreadFinish() error {
	if !a.gate.pass(a.threadIs("running")) {
		return nil
	}
	a.finishing = true
	return nil
}

// report lets the held turn close, which is when serve reports it, and
// waits for the report to land. A report that wakes an idle main takes
// a turn of its own, held like any other.
func (a *trrAdapter) report(saveFails bool) error {
	wake := ""
	if a.mainIs("idle") {
		wake = a.queueBlock()
	}
	if saveFails && !a.saveWorks {
		// serve's own save holds the name for a moment.
		if err := trrUntil("meta.json.tmp as a directory", func() bool { return os.Mkdir(a.metaTmp(), 0o755) == nil }); err != nil {
			return err
		}
		defer os.Remove(a.metaTmp())
	}
	control.Release(a.t, a.dir, a.threadHel)
	a.threadHel, a.finishing = "", false
	if err := trrUntil("the thread's turn to close", func() bool { return a.threadIs("done") }); err != nil {
		return err
	}
	_, id, err := a.mainState()
	if err != nil {
		return err
	}
	if err := trrUntil("the report in main", func() bool { return a.landed(id) > 0 }); err != nil {
		return err
	}
	if saveFails {
		a.memOnly = true
	} else if err := trrUntil("Reported saved", func() bool { d, _ := a.disk(); return d }); err != nil {
		return err
	}
	if wake == "" {
		return nil
	}
	if err := a.waitTaken(wake); err != nil {
		return err
	}
	a.mainHeld = wake
	return trrUntil("main woken into a turn", func() bool { return a.mainIs("turn") })
}

func (a *trrAdapter) Report() error {
	if !a.gate.pass(a.finishing && !(a.archived() && a.mainIs("idle"))) {
		return nil
	}
	return a.report(false)
}

func (a *trrAdapter) ReportSaveFails() error {
	st, id, err := a.mainState()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.finishing && !(a.archived() && (st == "idle" || st == "turn")) && a.threadParent() == id && st != "gone") {
		return nil
	}
	return a.report(true)
}

func (a *trrAdapter) threadParent() string {
	row, _, _ := a.threadRow()
	return row.SpawnedBy
}

// RespawnDies is a thread child that exits before it records an input:
// serve's "exit" for a closed turn it may already have reported. A live
// thread's process is killed first, then the respawn a message makes is
// held where it mounts and told to exit.
func (a *trrAdapter) RespawnDies() error {
	d, _ := a.disk()
	if !a.gate.pass(a.threadIs("done") && d) {
		return nil
	}
	row, _, err := a.threadRow()
	if err != nil {
		return err
	}
	if row.Live {
		st, err := orb.ReadState(a.s.Home, a.thread)
		if err != nil || st.PID == 0 {
			return fmt.Errorf("thread %s: no pid in its orb state: %v", a.thread, err)
		}
		if err := syscall.Kill(st.PID, syscall.SIGKILL); err != nil {
			return fmt.Errorf("thread %s: kill %d: %w", a.thread, st.PID, err)
		}
		if err := trrUntil("the thread's process gone", func() bool { r, _, _ := a.threadRow(); return !r.Live }); err != nil {
			return err
		}
	}
	control.HoldStart(a.t, a.dir)
	defer control.ReleaseStart(a.t, a.dir)
	defer os.Remove(filepath.Join(a.dir, "start.exit"))
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.thread, "a message the respawn never reads"); err != nil {
		return err
	}
	control.WaitHeld(a.t, a.dir, actionTimeout)
	control.ExitStart(a.t, a.dir)
	if err := trrUntil("the respawn to exit", func() bool { r, _, _ := a.threadRow(); return !r.Live }); err != nil {
		return err
	}
	// report() waits for nothing on an exit with the turn closed; give
	// a duplicate the moment it would need to show.
	time.Sleep(200 * time.Millisecond)
	return nil
}

func (a *trrAdapter) ArchiveThread() error {
	if !a.gate.pass(a.threadIs("done", "interrupted")) {
		return nil
	}
	if err := a.api(http.MethodPost, "/api/sessions/"+url.PathEscape(a.thread)+"/archive", nil, nil); err != nil {
		return err
	}
	a.thread = ""
	return nil
}

func (a *trrAdapter) SlotTake() error {
	if !a.gate.pass(!a.slotHeldNow() && !a.threadIs("queued")) {
		return nil
	}
	name := a.queueBlock()
	ctx, cancel := actionCtx()
	defer cancel()
	row, queued, err := a.s.CreateAgent(ctx, a.slotPar, "slot task "+name, 2, 0)
	if err != nil {
		return err
	}
	if queued {
		return fmt.Errorf("the other agent %s was queued", row.ID)
	}
	a.slot = row.ID
	if err := a.waitTaken(name); err != nil {
		return err
	}
	a.slotHeld = name
	return trrUntil("the other agent running", a.slotHeldNow)
}

func (a *trrAdapter) SlotFree() error {
	if !a.gate.pass(a.slotHeldNow()) {
		return nil
	}
	drain := ""
	if a.threadIs("queued") {
		drain = a.threadTurn
		control.Queue(a.t, a.dir, drain, control.Turn{Mode: "block", Text: trrReply(drain)})
	}
	control.Release(a.t, a.dir, a.slotHeld)
	a.slotHeld = ""
	if err := trrUntil("the other agent's turn to close", func() bool { return !a.slotHeldNow() }); err != nil {
		return err
	}
	// Its report saves meta.json: wait for it, or the save lands during
	// a later step.
	slot := a.slot
	if err := trrUntil("the other agent's report saved", func() bool {
		seq := a.closedSeq(slot)
		m, err := a.meta()
		return err == nil && seq > 0 && m.Sessions[slot].Reported >= seq
	}); err != nil {
		return err
	}
	if drain == "" {
		return nil
	}
	if err := a.waitTaken(drain); err != nil {
		return err
	}
	a.threadHel = drain
	return a.waitThread("running")
}

// ServeRestart stops serve with SIGTERM, as launchd or `bough update`
// does, and starts it again on the same HOME.
func (a *trrAdapter) ServeRestart() error {
	if !a.gate.pass(!a.finishing) {
		return nil
	}
	if a.archiving {
		if err := a.archiveMain(); err != nil {
			return err
		}
	}
	drain := ""
	if a.threadIs("queued") {
		drain = a.threadTurn
		control.Queue(a.t, a.dir, drain, control.Turn{Mode: "block", Text: trrReply(drain)})
	}
	a.s.Shutdown()
	if err := a.s.Resume(""); err != nil {
		return err
	}
	a.mainHeld, a.slotHeld, a.threadHel, a.memOnly = "", "", "", false
	if drain == "" {
		return nil
	}
	if err := a.waitTaken(drain); err != nil {
		return err
	}
	a.threadHel = drain
	return a.waitThread("running")
}

// trrAction logs each step a walk actually takes (the gate still open):
// when a run fails, the log is the walk.
func trrAction(name string, f func(*trrAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*trrAdapter)
		if a.gate.off {
			return nil, f(a)
		}
		start := time.Now()
		err := f(a)
		if !a.gate.off {
			a.t.Logf("walk %d: %s (%s) err=%v", a.walk, name, time.Since(start).Round(time.Millisecond), err)
		}
		return nil, err
	}
}

var trrActions = map[string]map[string]fmbt.ActionFunc{"Project": {
	"MessageProject":  trrAction("MessageProject", (*trrAdapter).MessageProject),
	"MainTurnEnd":     trrAction("MainTurnEnd", (*trrAdapter).MainTurnEnd),
	"ArchiveMain":     trrAction("ArchiveMain", (*trrAdapter).ArchiveMain),
	"ArchiveKill":     trrAction("ArchiveKill", (*trrAdapter).ArchiveKill),
	"LoseMainHistory": trrAction("LoseMainHistory", (*trrAdapter).LoseMainHistory),
	"StartThread":     trrAction("StartThread", (*trrAdapter).StartThread),
	"ThreadMessage":   trrAction("ThreadMessage", (*trrAdapter).ThreadMessage),
	"ThreadFinish":    trrAction("ThreadFinish", (*trrAdapter).ThreadFinish),
	"Report":          trrAction("Report", (*trrAdapter).Report),
	"ReportSaveFails": trrAction("ReportSaveFails", (*trrAdapter).ReportSaveFails),
	"RespawnDies":     trrAction("RespawnDies", (*trrAdapter).RespawnDies),
	"ArchiveThread":   trrAction("ArchiveThread", (*trrAdapter).ArchiveThread),
	"SlotTake":        trrAction("SlotTake", (*trrAdapter).SlotTake),
	"SlotFree":        trrAction("SlotFree", (*trrAdapter).SlotFree),
	"ServeRestart":    trrAction("ServeRestart", (*trrAdapter).ServeRestart),
}}

// threadReportRoutingHistory reads a thread's transcript as a path
// through the spec. A thread's file sees neither main nor the other
// agent, so only the thread's own steps are named: its first input is
// the page's 'Start thread', each close of a turn is its finish and
// the report, each later input a message in it, and an input over a
// turn still open is a serve restart that left it interrupted.
func threadReportRoutingHistory(entries []history.Entry) []tracecheck.Step {
	step := func(action, thread string) tracecheck.Step {
		return tracecheck.Step{Action: "Project#0." + action, State: map[string]any{"Project#0.thread": thread}}
	}
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Project#0.thread": "none"}}}
	started, open, last := false, false, ""
	for _, e := range entries {
		switch e.Kind {
		case "input":
			switch {
			case !started:
				steps = append(steps, step("StartThread", "running"))
				started = true
			case open:
				steps = append(steps, step("ServeRestart", "interrupted"), step("ThreadMessage", "running"))
			default:
				steps = append(steps, step("ThreadMessage", "running"))
			}
			open, last = true, ""
		case "done", "cancelled":
			if e.Kind == "done" && !open && last == "cancelled" {
				continue
			}
			if open {
				steps = append(steps, step("ThreadFinish", "pending"), step("Report", "done"))
			}
			open, last = false, e.Kind
		}
	}
	return steps
}

func init() { historyProjections["thread_report_routing"] = threadReportRoutingHistory }

func trrOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 12, "max-parallel-runs": 0}
}

func TestThreadReportRouting(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newTRRAdapter(t)
	if err := runMBT(t, "thread_report_routing", a, trrActions, trrOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	a.checkTraces(t)
}

// checkTraces replays every thread transcript the walks wrote.
func (a *trrAdapter) checkTraces(t *testing.T) int {
	t.Helper()
	g, err := tracecheck.Load(fizzCheck(t, "thread_report_routing"))
	if err != nil {
		t.Fatal(err)
	}
	checked := 0
	for _, id := range a.kids {
		if a.hasHistory(id) {
			checkHistory(t, g, sessionHistory(t, a.s.Home, id), threadReportRoutingHistory)
			checked++
		}
	}
	return checked
}

// walkPath drives one generated path through the adapter and compares
// the state it reads off serve with the path's after every step.
func (a *trrAdapter) walkPath(trace []tracecheck.Step) (err error) {
	if err := a.Init(); err != nil {
		return err
	}
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	for i, s := range trace {
		name := strings.TrimPrefix(s.Action, "Project#0.")
		if i > 0 {
			f, ok := trrActions["Project"][name]
			if !ok {
				return fmt.Errorf("step %d: no action %q", i, name)
			}
			if _, err := f(a, nil); err != nil {
				return fmt.Errorf("step %d (%s): %w", i, name, err)
			}
			if a.gate.off {
				return fmt.Errorf("step %d (%s): the adapter reads it as not enabled", i, name)
			}
		}
		have, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w", i, name, err)
		}
		if diff := pmtDiff(s.State, have); diff != "" {
			return fmt.Errorf("step %d (%s): state differs from the spec's:%s", i, name, diff)
		}
	}
	return nil
}

func trrPaths(t *testing.T) [][]tracecheck.Step {
	t.Helper()
	b, err := pathsJSON("thread_report_routing")
	if err != nil {
		t.Fatal(err)
	}
	var file struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &file); err != nil {
		t.Fatal(err)
	}
	var out [][]tracecheck.Step
	for _, p := range file.Paths {
		out = append(out, p.Trace)
	}
	return out
}

// TestThreadReportRoutingPaths walks every generated path (every
// settled state, or every link under MODEL_COVER=transitions) through
// the server adapter, four serves sharing the paths, then replays each
// thread's transcript on the graph.
func TestThreadReportRoutingPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	paths := trrPaths(t)
	const shards = 4
	for n := range shards {
		t.Run(fmt.Sprintf("serve%d", n), func(t *testing.T) {
			t.Parallel()
			a := newTRRAdapter(t)
			for i := n; i < len(paths); i += shards {
				if err := a.walkPath(paths[i]); err != nil {
					t.Errorf("path %d: %v", i, err)
				}
			}
			if a.checkTraces(t) == 0 {
				t.Error("no transcripts for the trace check")
			}
		})
	}
}

// A failed Reported save that the adapter lets succeed is the slip the
// spec's disk field exists for: the report is on disk where the spec
// says it is only in memory. The state after it is reached only through
// ReportSaveFails, so the state walks reach it.
func TestThreadReportRoutingPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newTRRAdapter(t)
	a.saveWorks = true
	for i, p := range trrPaths(t) {
		if err := a.walkPath(p); err != nil {
			t.Logf("path %d caught it: %v", i, err)
			return
		}
	}
	t.Fatal("every path passed with a ReportSaveFails whose save worked; the walk is not checking state")
}

// The random runs must fail on a wrong adapter too: ArchiveMain that
// kills at once leaves main down where the spec has it alive.
func TestThreadReportRoutingCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newTRRAdapter(t)
	a.archiveNow = true
	if err := runMBT(t, "thread_report_routing", a, trrActions, trrOptions()); err == nil {
		t.Fatal("a run whose ArchiveMain killed at once passed; the runner is not checking state")
	}
}
