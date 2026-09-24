//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/archive_cross_tab_and_create_race.fizz against a real serve:
// tab A's create (POST /api/sessions with a first message) racing the
// archive, unarchive and send any tab can make once the session's
// history file lists its row.
//
// Every step the spec names happens for real, held open where the
// product would run through it in one breath:
//
//   - llm-control's start hold parks Create's child before its history
//     file (CreateStart), and lets it go (HistoryFileAppears) or exit
//     (Timeout: the child dies before the file, the create's 500).
//   - serve's claim hold (BOUGH_SERVE_TEST_HOLD, "claim") keeps Create
//     between seeing the file and claiming the child, the window the
//     race lives in; Claim removes it and reads the POST's answer.
//   - serve's reap hold ("reap") keeps a SIGKILLed lease holder's lease
//     until KillReaped: archive's Kill blocks there, as it does in the
//     real window between the SIGKILL and the reap, so the other tab
//     can unarchive and send meanwhile.
//   - "sent" is a prompt the child has not read: the child is
//     SIGSTOPped before the prompt goes (a `-r` child ensure spawns is
//     parked at the start hold instead), and Record lets it read.
//
// c and r are read off the processes: Create's child is the one parked
// at CreateStart, a `-r` child the one parked at Send; alive and
// unclaimed while the create waits, leased while the row is live,
// dying once reaped while the row is still live (the lease not yet
// dropped). turn is read off the transcript for the adapter's last
// prompt; a prompt the archive's kill ended is the spec's "none".
//
// The spec disables Send where serve must refuse it (archived, Create
// holding the id unclaimed, the lease holder dying): after every step
// the adapter makes that request anyway, as the other tab would, and
// requires a 409. The spec's lost is then always false.

const acrConfig = controlConfig + "- id: orb\n  plugin: orb\n  config:\n    runtime: fake\n"

type acrResult struct {
	row serve.Row
	err error
}

type acrAdapter struct {
	t     *testing.T
	s     *servetest.Server
	dir   string // llm-control's dir
	hist  string
	holds string // BOUGH_SERVE_TEST_HOLD
	work  string
	gate  gate

	// This walk's session.
	before   map[string]bool // history ids before CreateStart
	id       string          // learned from the listing once the file exists
	create   string          // the spec's create: the POST as tab A sees it
	resp     chan acrResult  // the create's answer until read
	cpid     int             // Create's child
	rpid     int             // the -r child Send spawned
	holder   int             // the pid holding the lease, as far as the adapter drove it
	stopped  int             // a SIGSTOPped child, 0 when none
	pending  string          // the last prompt 200'd (or Create's, once claimed); "" once it ended
	name     string          // the llm turn pending takes
	archives []chan error    // archive POSTs blocked in Kill
	n        int             // turn names, unique across walks
	ids      []string        // sessions whose file appeared, for the trace check
	ran      map[string]int  // steps run per action
	probes   map[string]int  // refusals checked, by state

	// reapAtOnce is the wrong-adapter bug: ArchiveB leaves the reap
	// hold off, so the lease holder is gone at once, not dying.
	reapAtOnce bool
}

func newACRAdapter(t *testing.T) *acrAdapter {
	holds := t.TempDir()
	s := servetest.Start(t, servetest.Options{Config: acrConfig, Env: []string{"BOUGH_SERVE_TEST_HOLD=" + holds}})
	a := &acrAdapter{
		t: t, s: s, dir: control.Dir(s.Home), holds: holds,
		hist: filepath.Join(s.Home, ".bough", "history"),
		work: s.Dir(t, "work"),
	}
	// A hold left behind would park serve's shutdown for a minute.
	t.Cleanup(func() {
		os.Remove(a.holdPath("claim"))
		os.Remove(a.holdPath("reap"))
	})
	return a
}

func (a *acrAdapter) holdPath(point string) string { return filepath.Join(a.holds, point) }

func (a *acrAdapter) setHold(point string) error {
	return os.WriteFile(a.holdPath(point), nil, 0o644)
}

func (a *acrAdapter) clearHold(point string) error {
	if err := os.Remove(a.holdPath(point)); err != nil && !os.IsNotExist(err) {
		return err
	}
	return nil
}

func (a *acrAdapter) next() string {
	a.n++
	return fmt.Sprintf("t%05d", a.n)
}

func (a *acrAdapter) Init() error {
	a.before, a.id, a.create, a.resp = nil, "", "none", nil
	a.cpid, a.rpid, a.holder, a.stopped = 0, 0, 0, 0
	a.pending, a.name, a.archives = "", "", nil
	a.gate.reset()
	return nil
}

// Cleanup ends whatever the walk left: holds off, the create answered,
// every archive returned, the session archived (its lease holder
// killed), and any process of the walk that is still alive killed.
func (a *acrAdapter) Cleanup() error {
	var errs []error
	a.clearHold("reap")
	if a.stopped != 0 {
		syscall.Kill(a.stopped, syscall.SIGCONT)
		a.stopped = 0
	}
	if a.resp != nil {
		if a.id == "" {
			control.ExitStart(a.t, a.dir)
		}
		a.clearHold("claim")
		select {
		case r := <-a.resp:
			if r.err == nil && a.id == "" {
				a.id = r.row.ID
			}
		case <-time.After(actionTimeout):
			errs = append(errs, errors.New("cleanup: the create's POST did not answer"))
		}
		a.resp = nil
	}
	a.clearHold("claim")
	control.ReleaseStart(a.t, a.dir)
	os.Remove(filepath.Join(a.dir, "start.exit"))
	for _, ch := range a.archives {
		select {
		case <-ch:
		case <-time.After(actionTimeout):
			errs = append(errs, errors.New("cleanup: an archive did not return"))
		}
	}
	a.archives = nil
	if a.name != "" {
		// Untaken, it would answer the next walk's request; taken and
		// held, it is released so the child is not stuck in it.
		os.Remove(filepath.Join(a.dir, a.name+".json"))
		control.Release(a.t, a.dir, a.name)
		a.name = ""
	}
	if a.id != "" {
		ctx, cancel := actionCtx()
		if _, err := a.s.Archive(ctx, a.id); err != nil && !isStatus(err, http.StatusNotFound) {
			errs = append(errs, fmt.Errorf("cleanup: archive: %w", err))
		}
		cancel()
	}
	for _, pid := range []int{a.cpid, a.rpid} {
		if pid != 0 && alive(pid) {
			syscall.Kill(pid, syscall.SIGKILL)
		}
	}
	return errors.Join(errs...)
}

func (a *acrAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

type acrState struct {
	create, c, r, turn string
	file, archived     bool
}

func (s acrState) m() map[string]any {
	return map[string]any{
		"create": s.create, "file": s.file, "c": s.c, "r": s.r,
		"archived": s.archived, "turn": s.turn, "lost": false,
	}
}

func (a *acrAdapter) GetState() (map[string]any, error) {
	st, err := a.state()
	if err != nil {
		return nil, err
	}
	return st.m(), nil
}

func (a *acrAdapter) state() (acrState, error) {
	st := acrState{create: a.create, c: "none", r: "none", turn: "none", file: a.fileExists()}
	var row serve.Row
	var lines []serve.Line
	if a.id != "" {
		ctx, cancel := actionCtx()
		defer cancel()
		var err error
		row, lines, err = a.s.GetSession(ctx, a.id)
		if err != nil {
			return st, err
		}
		st.archived = row.Archived
	}
	st.c = a.proc(a.cpid, row.Live, func() string {
		switch a.create {
		case "waiting":
			return "unclaimed"
		case "ok":
			return "leased"
		}
		return "orphan" // a failed create's child is still running
	})
	st.r = a.proc(a.rpid, row.Live, func() string { return "leased" })
	if a.pending != "" {
		st.turn = "sent"
		for i, l := range lines {
			if l.Kind == "input" && strings.Contains(l.Text, a.pending) {
				st.turn = "running"
				for _, m := range lines[i+1:] {
					if m.Kind == "done" || m.Kind == "cancelled" {
						st.turn = "none"
					}
				}
			}
		}
	}
	return st, nil
}

// proc is one child's spec value: none before it exists, what live says
// while it runs, dying once reaped while its lease is still held.
func (a *acrAdapter) proc(pid int, live bool, running func() string) string {
	switch {
	case pid == 0:
		return "none"
	case alive(pid):
		if v := running(); v != "leased" || live {
			return v
		}
		return "unleased" // running, but serve has no lease for it
	case pid == a.holder && live:
		return "dying"
	}
	return "none"
}

// fileExists is a new history file since CreateStart: the session's.
func (a *acrAdapter) fileExists() bool {
	if a.before == nil {
		return false
	}
	return a.newID() != ""
}

func (a *acrAdapter) newID() string {
	files, _ := filepath.Glob(filepath.Join(a.hist, "*.jsonl"))
	for _, f := range files {
		if id := strings.TrimSuffix(filepath.Base(f), ".jsonl"); !a.before[id] {
			return id
		}
	}
	return ""
}

// enabled reads a require off the server's view; a failed read closes
// the gate.
func (a *acrAdapter) enabled(require func(acrState) bool) (acrState, bool) {
	if a.gate.off {
		return acrState{}, false
	}
	st, err := a.state()
	if err != nil {
		a.gate.off = true
		return st, false
	}
	return st, a.gate.pass(require(st))
}

// CreateStart is tab A's New session with a first message: the POST
// stays in flight, its child parked before its history file, and the
// claim hold set for when the file lands.
func (a *acrAdapter) CreateStart() error {
	if _, ok := a.enabled(func(s acrState) bool { return s.create == "none" }); !ok {
		return nil
	}
	control.HoldStart(a.t, a.dir)
	if err := a.setHold("claim"); err != nil {
		return err
	}
	a.before = map[string]bool{}
	files, _ := filepath.Glob(filepath.Join(a.hist, "*.jsonl"))
	for _, f := range files {
		a.before[strings.TrimSuffix(filepath.Base(f), ".jsonl")] = true
	}
	a.name = a.next()
	a.resp = make(chan acrResult, 1)
	go func(resp chan acrResult, prompt string) {
		ctx, cancel := context.WithTimeout(context.Background(), 2*actionTimeout)
		defer cancel()
		row, err := a.s.CreateSession(ctx, a.work, prompt)
		resp <- acrResult{row, err}
	}(a.resp, "first "+a.name)
	a.create = "waiting"
	deadline := time.Now().Add(actionTimeout)
	for a.cpid = control.Held(a.dir); a.cpid == 0; a.cpid = control.Held(a.dir) {
		if time.Now().After(deadline) {
			return errors.New("CreateStart: no child parked at the start hold")
		}
		time.Sleep(10 * time.Millisecond)
	}
	return nil
}

// HistoryFileAppears lets the child write its file (and its meta line,
// which Create's answer waits for anyway); tab B's list shows the row.
func (a *acrAdapter) HistoryFileAppears() error {
	if _, ok := a.enabled(func(s acrState) bool { return s.c == "unclaimed" && !s.file }); !ok {
		return nil
	}
	control.ReleaseStart(a.t, a.dir)
	deadline := time.Now().Add(actionTimeout)
	for {
		if id := a.newID(); id != "" {
			b, _ := os.ReadFile(filepath.Join(a.hist, id+".jsonl"))
			if strings.Contains(string(b), "\n") && a.listed(id) {
				a.id = id
				a.ids = append(a.ids, id)
				return nil
			}
		}
		if time.Now().After(deadline) {
			return errors.New("HistoryFileAppears: no history file with its meta, listed")
		}
		time.Sleep(10 * time.Millisecond)
	}
}

// listed is tab B's list poll (?all=1, as an archived row is a row too).
func (a *acrAdapter) listed(id string) bool {
	ctx, cancel := actionCtx()
	defer cancel()
	rows, err := a.s.ListSessions(ctx, true)
	return err == nil && slices.ContainsFunc(rows, func(r serve.Row) bool { return r.ID == id })
}

// Timeout is the child dying before its file: Create answers 500.
func (a *acrAdapter) Timeout() error {
	if _, ok := a.enabled(func(s acrState) bool { return s.c == "unclaimed" && !s.file }); !ok {
		return nil
	}
	control.ExitStart(a.t, a.dir)
	defer os.Remove(filepath.Join(a.dir, "start.exit"))
	if err := a.answer(); err != nil {
		return err
	}
	a.dropTurn()
	return a.waitGone(a.cpid, "the timed-out child")
}

// Claim lets Create claim: the child is paused first, so the first
// message is written and not yet read ("sent").
func (a *acrAdapter) Claim() error {
	if _, ok := a.enabled(func(s acrState) bool { return s.c == "unclaimed" && s.file }); !ok {
		return nil
	}
	control.Queue(a.t, a.dir, a.name, control.Turn{Mode: "block", Text: "answered " + a.name})
	if err := a.pause(a.cpid); err != nil {
		return err
	}
	if err := a.clearHold("claim"); err != nil {
		return err
	}
	if err := a.answer(); err != nil {
		return err
	}
	if a.create == "ok" {
		a.holder, a.pending = a.cpid, "first "+a.name
		return nil
	}
	// Refused: nothing will take the queued turn, and the child must go.
	a.dropTurn()
	a.stopped = 0
	return a.waitGone(a.cpid, "the refused create's child")
}

// answer reads the create's POST: 201 is ok, any refusal failed.
func (a *acrAdapter) answer() error {
	var r acrResult
	select {
	case r = <-a.resp:
	case <-time.After(actionTimeout):
		return errors.New("the create's POST did not answer")
	}
	a.resp = nil
	if r.err != nil {
		if ae := (*servetest.APIError)(nil); !errors.As(r.err, &ae) {
			return r.err
		}
		a.create = "failed"
		return nil
	}
	if r.row.ID != a.id {
		return fmt.Errorf("the create answered %s, the listing showed %s", r.row.ID, a.id)
	}
	a.create = "ok"
	return nil
}

// dropTurn unqueues a turn nothing will take.
func (a *acrAdapter) dropTurn() {
	if a.name != "" {
		os.Remove(filepath.Join(a.dir, a.name+".json"))
		a.name = ""
	}
}

func (a *acrAdapter) pause(pid int) error {
	if err := syscall.Kill(pid, syscall.SIGSTOP); err != nil {
		return fmt.Errorf("pause child %d: %w", pid, err)
	}
	a.stopped = pid
	return nil
}

func (a *acrAdapter) waitGone(pid int, what string) error {
	for deadline := time.Now().Add(actionTimeout); alive(pid); {
		if time.Now().After(deadline) {
			return fmt.Errorf("%s (%d) is still alive", what, pid)
		}
		time.Sleep(10 * time.Millisecond)
	}
	return nil
}

// Send is POST /prompt from whichever tab. A live holder is paused
// first; with none, ensure spawns `-r <id>`, which parks at the start
// hold with the line unread.
func (a *acrAdapter) Send() error {
	st, ok := a.enabled(func(s acrState) bool {
		return s.file && !s.archived && s.turn == "none" && s.c != "unclaimed" && s.c != "dying" && s.r != "dying"
	})
	if !ok {
		return nil
	}
	a.name = a.next()
	control.Queue(a.t, a.dir, a.name, control.Turn{Mode: "block", Text: "answered " + a.name})
	spawn := st.c != "leased" && st.r != "leased"
	if spawn {
		control.HoldStart(a.t, a.dir)
	} else if err := a.pause(a.holder); err != nil {
		return err
	}
	text := "send " + a.name
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, text); err != nil {
		return err
	}
	a.pending = text
	if !spawn {
		return nil
	}
	deadline := time.Now().Add(actionTimeout)
	for a.rpid = control.Held(a.dir); a.rpid == 0; a.rpid = control.Held(a.dir) {
		if time.Now().After(deadline) {
			return errors.New("Send: no -r child parked at the start hold")
		}
		time.Sleep(10 * time.Millisecond)
	}
	a.holder = a.rpid
	return nil
}

// Record lets the child read the prompt: it records its input and the
// model request is held in its turn.
func (a *acrAdapter) Record() error {
	if _, ok := a.enabled(func(s acrState) bool {
		return s.turn == "sent" && (s.c == "leased" || s.r == "leased")
	}); !ok {
		return nil
	}
	if a.stopped != 0 {
		if err := syscall.Kill(a.stopped, syscall.SIGCONT); err != nil {
			return err
		}
		a.stopped = 0
	} else {
		control.ReleaseStart(a.t, a.dir)
	}
	control.WaitTaken(a.t, a.dir, a.name, actionTimeout)
	return a.waitTurn("running")
}

func (a *acrAdapter) Finish() error {
	if _, ok := a.enabled(func(s acrState) bool {
		return s.turn == "running" && (s.c == "leased" || s.r == "leased")
	}); !ok {
		return nil
	}
	control.Release(a.t, a.dir, a.name)
	if err := a.waitTurn("none"); err != nil {
		return err
	}
	if _, err := waitRow(a.s, a.id, "the turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning }); err != nil {
		return err
	}
	a.pending, a.name = "", ""
	return nil
}

func (a *acrAdapter) waitTurn(want string) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		st, err := a.state()
		if err == nil && st.turn == want {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for turn %q: %+v %v", want, st, err)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// ArchiveB is POST /archive. With a live lease holder the reap hold is
// set first, so the POST blocks in Kill with the child killed and its
// lease held (dying) until KillReaped; so does an archive of a session
// whose holder is already dying.
func (a *acrAdapter) ArchiveB() error {
	st, ok := a.enabled(func(s acrState) bool { return s.file && !s.archived })
	if !ok {
		return nil
	}
	leased := st.c == "leased" || st.r == "leased"
	blocks := leased || st.c == "dying" || st.r == "dying"
	if leased && !a.reapAtOnce {
		if err := a.setHold("reap"); err != nil {
			return err
		}
	}
	done := make(chan error, 1)
	go func(id string) {
		ctx, cancel := context.WithTimeout(context.Background(), 2*actionTimeout)
		defer cancel()
		_, err := a.s.Archive(ctx, id)
		done <- err
	}(a.id)
	if !blocks || a.reapAtOnce {
		select {
		case err := <-done:
			if err != nil {
				return err
			}
		case <-time.After(actionTimeout):
			return errors.New("ArchiveB: the archive did not return")
		}
	} else {
		a.archives = append(a.archives, done)
	}
	if leased {
		if err := a.waitGone(a.holder, "the archived session's child"); err != nil {
			return err
		}
		// The prompt ended with the process; so did a start hold it
		// was parked at.
		a.stopped, a.pending = 0, ""
		os.Remove(filepath.Join(a.dir, a.name+".json"))
		a.name = ""
		control.ReleaseStart(a.t, a.dir)
		if a.reapAtOnce {
			a.holder = 0
		}
	}
	_, err := waitRow(a.s, a.id, "the archive flag", func(r serve.Row) bool { return r.Archived })
	return err
}

func (a *acrAdapter) UnarchiveB() error {
	if _, ok := a.enabled(func(s acrState) bool { return s.archived }); !ok {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Unarchive(ctx, a.id)
	return err
}

// KillReaped lets the reap drop the lease; every archive blocked in
// Kill returns.
func (a *acrAdapter) KillReaped() error {
	if _, ok := a.enabled(func(s acrState) bool { return s.c == "dying" || s.r == "dying" }); !ok {
		return nil
	}
	if err := a.clearHold("reap"); err != nil {
		return err
	}
	for _, ch := range a.archives {
		select {
		case err := <-ch:
			if err != nil {
				return fmt.Errorf("KillReaped: an archive answered %w", err)
			}
		case <-time.After(actionTimeout):
			return errors.New("KillReaped: an archive did not return")
		}
	}
	a.archives = nil
	if _, err := waitRow(a.s, a.id, "the lease dropped", func(r serve.Row) bool { return !r.Live }); err != nil {
		return err
	}
	a.holder = 0
	return nil
}

// probe is the other tab's Send where the spec has none: while serve
// must refuse one (archived, Create holding the id unclaimed, the lease
// holder dying) it has to answer 409, or a prompt lands where nothing
// will read it or beside a second child.
func (a *acrAdapter) probe() error {
	st, err := a.state()
	if err != nil {
		return err
	}
	if !st.file || st.turn != "none" || !(st.archived || st.c == "unclaimed" || st.c == "dying" || st.r == "dying") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	err = a.s.Prompt(ctx, a.id, "probe "+a.next())
	if isStatus(err, http.StatusConflict) {
		key := fmt.Sprintf("c=%s r=%s archived=%v", st.c, st.r, st.archived)
		if a.probes == nil {
			a.probes = map[string]int{}
		}
		a.probes[key]++
		return nil
	}
	return fmt.Errorf("a Send with c=%s r=%s archived=%v answered %v; serve must refuse it (409)", st.c, st.r, st.archived, errOr200(err))
}

func errOr200(err error) any {
	if err == nil {
		return "200"
	}
	return err
}

// acrAction counts the steps that ran and checks the refusals of the
// state each one reached.
func acrAction(name string, f func(*acrAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*acrAdapter)
		if err := f(a); err != nil {
			return nil, fmt.Errorf("%s: %w", name, err)
		}
		if a.gate.off {
			return nil, nil
		}
		if a.ran == nil {
			a.ran = map[string]int{}
		}
		a.ran[name]++
		if err := a.probe(); err != nil {
			return nil, fmt.Errorf("after %s: %w", name, err)
		}
		return nil, nil
	}
}

var acrActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"CreateStart":        acrAction("CreateStart", (*acrAdapter).CreateStart),
	"HistoryFileAppears": acrAction("HistoryFileAppears", (*acrAdapter).HistoryFileAppears),
	"Timeout":            acrAction("Timeout", (*acrAdapter).Timeout),
	"Claim":              acrAction("Claim", (*acrAdapter).Claim),
	"Send":               acrAction("Send", (*acrAdapter).Send),
	"Record":             acrAction("Record", (*acrAdapter).Record),
	"Finish":             acrAction("Finish", (*acrAdapter).Finish),
	"ArchiveB":           acrAction("ArchiveB", (*acrAdapter).ArchiveB),
	"UnarchiveB":         acrAction("UnarchiveB", (*acrAdapter).UnarchiveB),
	"KillReaped":         acrAction("KillReaped", (*acrAdapter).KillReaped),
}, "": {
	// deadlock_detection is off, so the runner offers a role-less "end";
	// it matches no link, so it declines like any disabled pick.
	"end": func(m any, _ []fmbt.Arg) (any, error) {
		m.(*acrAdapter).gate.pass(false)
		return nil, errDisabled
	},
}}

func acrOptions() map[string]any {
	return map[string]any{"max-seq-runs": 200, "max-actions": 8, "max-parallel-runs": 0}
}

// acrHistory reads the trace off a session's transcript. The file
// appearing is the create's child having started; its first input is
// the claimed first message (or, when the claim was refused, the first
// Send: either way Claim then Record is a path to it); a done ends a
// turn. Archive writes nothing, but a turn it killed mid-run is closed
// by the next child as an interrupted cancel: that is the archive, the
// reap and the unarchive that let the next Send in.
func acrHistory(entries []history.Entry) []tracecheck.Step {
	f := func(k string, v any) map[string]any { return map[string]any{"Session#0." + k: v} }
	steps := []tracecheck.Step{
		{Action: "Init", State: f("create", "none")},
		{Action: "Session#0.CreateStart", State: f("create", "waiting")},
		{Action: "Session#0.HistoryFileAppears", State: f("file", true)},
	}
	first, open := true, false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			if first {
				steps = append(steps, tracecheck.Step{Action: "Session#0.Claim", State: f("turn", "sent")})
			} else {
				steps = append(steps, tracecheck.Step{Action: "Session#0.Send", State: f("turn", "sent")})
			}
			steps = append(steps, tracecheck.Step{Action: "Session#0.Record", State: f("turn", "running")})
			first, open = false, true
		case "done":
			if open {
				steps = append(steps, tracecheck.Step{Action: "Session#0.Finish", State: f("turn", "none")})
			}
			open = false
		case "cancelled":
			if open {
				steps = append(steps,
					tracecheck.Step{Action: "Session#0.ArchiveB", State: f("archived", true)},
					tracecheck.Step{Action: "Session#0.KillReaped", State: f("turn", "none")},
					tracecheck.Step{Action: "Session#0.UnarchiveB", State: f("archived", false)})
			}
			open = false
		}
	}
	return steps
}

func init() { historyProjections["archive_cross_tab_and_create_race"] = acrHistory }

func TestArchiveCrossTabAndCreateRace(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newACRAdapter(t)
	if err := runMBT(t, "archive_cross_tab_and_create_race", a, acrActions, acrOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("steps run: %v; refusals checked: %v", a.ran, a.probes)
	a.checkHistories(t)
}

// Every transition (MODEL_COVER=transitions) or every state walked in
// order against one serve, the state compared after each step.
func TestArchiveCrossTabAndCreateRacePaths(t *testing.T) {
	t.Parallel()
	a := newACRAdapter(t)
	if err := a.walk(pathsJSON("archive_cross_tab_and_create_race")); err != nil {
		t.Fatal(err)
	}
	t.Logf("steps run: %v; refusals checked: %v", a.ran, a.probes)
	a.checkHistories(t)
}

// A kill reaped at once where the spec has the holder dying shows on
// the ArchiveB links out of a leased state: the walk over every link
// must fail on it, or its green proves nothing.
func TestArchiveCrossTabAndCreateRacePathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newACRAdapter(t)
	a.reapAtOnce = true
	if err := a.walk(pathsJSONCover("archive_cross_tab_and_create_race", tracecheck.CoverTransitions)); err == nil {
		t.Fatal("a walk whose archive reaps the child at once passed; the walk is not checking state")
	} else {
		t.Logf("caught: %v", err)
	}
}

func (a *acrAdapter) checkHistories(t *testing.T) {
	g, err := tracecheck.Load(filepath.Join("..", "testdata", "archive_cross_tab_and_create_race"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), acrHistory)
	}
}

func (a *acrAdapter) walk(b []byte, err error) error {
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
	for pi, p := range doc.Paths {
		if err := a.Init(); err != nil {
			return fmt.Errorf("path %d: Init: %w", pi, err)
		}
		var names []string
		var failed error
		for si, step := range p.Trace {
			name := strings.TrimPrefix(step.Action, "Session#0.")
			names = append(names, name)
			if si > 0 {
				if name == "end" {
					continue
				}
				if _, err := acrActions["Session"][name](a, nil); err != nil {
					failed = fmt.Errorf("path %d %v: %w", pi, names, err)
					break
				}
				if a.gate.off {
					failed = fmt.Errorf("path %d %v: the adapter found %s not enabled", pi, names, name)
					break
				}
			}
			got, err := a.GetState()
			if err != nil {
				failed = fmt.Errorf("path %d %v: %w", pi, names, err)
				break
			}
			for k, v := range step.State {
				field, ok := strings.CutPrefix(k, "Session#0.")
				if ok && got[field] != v {
					failed = fmt.Errorf("path %d %v: %s is %v, the spec says %v (state %v)", pi, names, field, got[field], v, got)
					break
				}
			}
			if failed != nil {
				break
			}
		}
		if err := a.Cleanup(); err != nil && failed == nil {
			failed = fmt.Errorf("path %d %v: Cleanup: %w", pi, names, err)
		}
		if failed != nil {
			return failed
		}
	}
	return nil
}
