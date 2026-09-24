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

// specs/project_archive_live.fizz against a real serve: one project per
// walk, archived (POST /api/projects/{slug}/archive) while its threads
// run, their reports travel, its main thread boots, and a message or a
// 'New thread' arrives.
//
// The spec's steps are windows that last microseconds in serve, so the
// serve runs with BOUGH_SERVE_TEST_HOLD (serve.Options.HoldDir) and the
// adapter parks it at each one while <hold>/<point> exists:
//
//   - archive-ending, archive-killing, archive-flagging: the archive
//     handler after its snapshot, before EndProject's Kill(main), and
//     before the SetArchived loop (ArchiveProject, EndThreads, KillMain,
//     FlagAll). serve writes <point>.at while it is parked there.
//   - report-<thread>: a thread's report goroutine before it tells main
//     (rep1/repx until Report1/ReportX).
//   - send-<main>: a line Send accepted (200) and main has not read yet
//     (msg "sent" until TakeMessage, or "lost" when main dies first).
//
// llm-control's hold_boot keeps main at its boot (main "boot") and every
// thread before its first turn; a turn the spec calls running is a
// "block" turn the adapter releases. Every child runs on the fake orb
// runtime. main's own tools.spawn is the POST it sends (spawnedBy main),
// as background_agents drives it.
const palConfig = "- id: llm\n  plugin: llm-control\n  config:\n    hold_boot: true\n" +
	"- id: orb\n  plugin: orb\n  config:\n    runtime: fake\n"

type palAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	hold string // serve's HoldDir
	gate gate

	walk, turn int
	slug       string

	// The spec's fields the adapter keeps: what the client did, and
	// what the archive handler has been let through.
	setup, arch, msgIn, lostBy, wakeIn, left, view string
	mid, xSnap, rep1, repx, reopened               bool
	xBy                                            string

	main, t1, x    string // session ids, "" before they exist
	mainTurn       string // main's held block turn
	t1Turn, xTurn  string // each thread's held block turn
	msgText        string // the one message, "" before it is sent
	msgBoot        bool   // the message is the request booting main
	msgReq, thrReq *palReq
	archReq        *palReq

	mains, kids []string // every transcript the walks wrote

	// wrongKill is the deliberate wiring bug the CatchesWrongAdapter
	// tests inject: EndThreads lets the handler through the Kill(main)
	// hold too, so main dies a step early.
	wrongKill bool
	// wrongStart: StartActive's thread answers at once instead of holding
	// its turn, so t1 is idle where the spec has it running. StartActive
	// is enabled at Init, so the runner's short random walks reach it.
	wrongStart bool
}

type palReq struct {
	done chan palResult
}

type palResult struct {
	body map[string]any
	err  error
}

func newPALAdapter(t *testing.T) *palAdapter {
	hold := t.TempDir()
	s := servetest.Start(t, servetest.Options{Config: palConfig, Env: []string{"BOUGH_SERVE_TEST_HOLD=" + hold}})
	return &palAdapter{t: t, s: s, dir: control.Dir(s.Home), hold: hold}
}

func (a *palAdapter) Init() error {
	a.walk++
	var r struct {
		Project serve.Project `json:"project"`
	}
	if err := a.api(http.MethodPost, "/api/projects", map[string]string{"name": fmt.Sprintf("Walk %d", a.walk)}, &r); err != nil {
		return err
	}
	a.slug = r.Project.Slug
	a.setup, a.arch, a.msgIn, a.lostBy, a.wakeIn, a.left, a.view = "start", "idle", "", "", "", "", "home"
	a.mid, a.xSnap, a.rep1, a.repx, a.reopened, a.xBy = false, false, false, false, false, ""
	a.main, a.t1, a.x, a.mainTurn, a.t1Turn, a.xTurn, a.msgText, a.msgBoot = "", "", "", "", "", "", "", false
	a.msgReq, a.thrReq, a.archReq = nil, nil, nil
	a.gate.reset()
	return nil
}

// Cleanup lets everything the walk parked go and archives the project,
// so no process of this walk takes the next walk's turns.
func (a *palAdapter) Cleanup() error {
	var errs []error
	for range 3 {
		a.unholdAll()
		for id := range control.Booting(a.dir) {
			control.ReleaseBoot(a.t, a.dir, id)
		}
		a.releaseTaken()
		for _, p := range []**palReq{&a.archReq, &a.msgReq, &a.thrReq} {
			if *p == nil {
				continue
			}
			select {
			case r := <-(*p).done:
				*p = nil
				if r.err != nil {
					errs = append(errs, r.err)
				}
			case <-time.After(2 * time.Second):
			}
		}
	}
	for _, p := range []*palReq{a.archReq, a.msgReq, a.thrReq} {
		if p != nil {
			errs = append(errs, errors.New("a request of the walk never returned"))
		}
	}
	if a.slug != "" {
		errs = append(errs, a.api(http.MethodPost, "/api/projects/"+a.slug+"/archive", nil, nil))
	}
	a.unholdAll()
	a.releaseTaken()
	// Turns queued and never taken would answer the next walk.
	queued, _ := filepath.Glob(filepath.Join(a.dir, "*.json"))
	for _, q := range queued {
		os.Remove(q)
	}
	return errors.Join(errs...)
}

// releaseTaken lets every held block turn reply.
func (a *palAdapter) releaseTaken() {
	taken, _ := filepath.Glob(filepath.Join(a.dir, "*.taken"))
	for _, f := range taken {
		rel := strings.TrimSuffix(f, ".taken") + ".release"
		if _, err := os.Stat(rel); err != nil {
			os.WriteFile(rel, nil, 0o644)
		}
	}
}

func (a *palAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Project", Index: 0}: a}, nil
}

// --- holds

func (a *palAdapter) holdOn(point string) error {
	return os.WriteFile(filepath.Join(a.hold, point), nil, 0o644)
}

func (a *palAdapter) holdOff(point string) {
	os.Remove(filepath.Join(a.hold, point))
}

func (a *palAdapter) unholdAll() {
	ents, _ := os.ReadDir(a.hold)
	for _, e := range ents {
		if !strings.HasSuffix(e.Name(), ".at") {
			os.Remove(filepath.Join(a.hold, e.Name()))
		}
	}
}

// parkedAt waits for serve to say it is parked at point.
func (a *palAdapter) parkedAt(point string) error {
	return waitUntil("the archive parked at "+point, func() bool {
		_, err := os.Stat(filepath.Join(a.hold, point+".at"))
		return err == nil
	})
}

// --- reading serve

type palMeta struct {
	Sessions map[string]serve.SessionMeta `json:"sessions"`
	Mains    map[string]string            `json:"mains"`
}

func (a *palAdapter) meta() (palMeta, error) {
	var m palMeta
	b, err := os.ReadFile(filepath.Join(a.s.Home, ".bough", "serve", "meta.json"))
	if errors.Is(err, os.ErrNotExist) {
		return m, nil
	}
	if err != nil {
		return m, err
	}
	return m, json.Unmarshal(b, &m)
}

func (a *palAdapter) histPath(id string) string {
	return filepath.Join(a.s.Home, ".bough", "history", id+".jsonl")
}

func (a *palAdapter) hasHistory(id string) bool {
	if id == "" {
		return false
	}
	_, err := os.Stat(a.histPath(id))
	return err == nil
}

func (a *palAdapter) entries(id string) []history.Entry {
	es, _ := history.Read(a.histPath(id))
	return es
}

func (a *palAdapter) rows() (map[string]serve.Row, error) {
	var r struct {
		Sessions []serve.Row `json:"sessions"`
	}
	if err := a.api(http.MethodGet, "/api/sessions?all=1", nil, &r); err != nil {
		return nil, err
	}
	out := map[string]serve.Row{}
	for _, row := range r.Sessions {
		out[row.ID] = row
	}
	return out, nil
}

// palState is the role's state as the adapter reads it.
type palState struct {
	main, t1, x, msg   string
	mainArch, mainHist bool
	t1Arch, xArch      bool
	mainID             string
	mainLive, xLive    bool
}

func (a *palAdapter) observe() (palState, error) {
	var st palState
	m, err := a.meta()
	if err != nil {
		return st, err
	}
	rows, err := a.rows()
	if err != nil {
		return st, err
	}
	booting := control.Booting(a.dir)
	st.mainID = m.Mains[a.slug]
	if a.main == "" && st.mainID != "" {
		a.main = st.mainID
	}
	st.main = "none"
	if st.mainID != "" {
		row := rows[st.mainID]
		switch _, boot := booting[st.mainID]; {
		case boot:
			st.main = "boot"
		case row.Live && row.Status == serve.StatusRunning:
			st.main = "turn"
		case row.Live:
			st.main = "idle"
		}
		st.mainLive = row.Live
		st.mainHist = a.hasHistory(st.mainID)
		st.mainArch = m.Sessions[st.mainID].Archived
	}
	thread := func(id string) (string, bool, bool) {
		if id == "" {
			return "none", false, false
		}
		row, ok := rows[id]
		switch {
		case !ok:
			return "starting", false, m.Sessions[id].Archived
		case row.Live && row.Status == serve.StatusRunning:
			return "running", true, m.Sessions[id].Archived
		case row.Live:
			return "idle", true, m.Sessions[id].Archived
		}
		return "dead", false, m.Sessions[id].Archived
	}
	st.t1, _, st.t1Arch = thread(a.t1)
	st.x, st.xLive, st.xArch = thread(a.x)
	switch {
	case a.msgText == "":
		st.msg = "none"
	case a.msgBoot:
		st.msg = "boot"
	case a.recorded(st.mainID):
		st.msg = "recorded"
	case st.mainLive:
		st.msg = "sent"
	default:
		st.msg = "lost"
	}
	return st, nil
}

// recorded says whether main took the message: an input (a turn of its
// own, or a steer into the turn in flight) carrying its text.
func (a *palAdapter) recorded(main string) bool {
	if main == "" {
		return false
	}
	for _, e := range a.entries(main) {
		if e.Kind == "input" && (e.Data["text"] == a.msgText || e.Data["typed"] == a.msgText) {
			return true
		}
	}
	return false
}

func (a *palAdapter) GetState() (map[string]any, error) {
	st, err := a.observe()
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"setup": a.setup, "arch": a.arch, "mid": a.mid,
		"main": st.main, "main_arch": st.mainArch, "main_hist": st.mainHist,
		"t1": st.t1, "t1_arch": st.t1Arch,
		"x": st.x, "x_arch": st.xArch, "x_by": a.xBy, "x_snap": a.xSnap,
		"rep1": a.rep1, "repx": a.repx,
		"msg": st.msg, "msg_in": a.msgIn, "reopened": a.reopened, "lost_by": a.lostBy,
		"wake_in": a.wakeIn, "left": a.left, "view": a.view,
	}, nil
}

// --- requests

func (a *palAdapter) api(method, path string, body, out any) error {
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

func (a *palAdapter) post(path string, body any) *palReq {
	p := &palReq{done: make(chan palResult, 1)}
	go func() {
		var out map[string]any
		err := a.api(http.MethodPost, path, body, &out)
		p.done <- palResult{out, err}
	}()
	return p
}

func (a *palAdapter) collect(p *palReq) (map[string]any, error) {
	select {
	case r := <-p.done:
		return r.body, r.err
	case <-time.After(actionTimeout):
		return nil, fmt.Errorf("request did not return in %s", actionTimeout)
	}
}

// returned says whether a request in flight has answered, without
// waiting: a request serve should still be holding must not have.
func returned(p *palReq) (palResult, bool) {
	select {
	case r := <-p.done:
		return r, true
	default:
		return palResult{}, false
	}
}

func sessionID(body map[string]any) string {
	row, _ := body["session"].(map[string]any)
	id, _ := row["id"].(string)
	return id
}

// --- turns

func (a *palAdapter) queue() string {
	a.turn++
	name := fmt.Sprintf("w%05d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	return name
}

func (a *palAdapter) taken(name string) error {
	return waitUntil("turn "+name+" taken", func() bool {
		_, err := os.Stat(filepath.Join(a.dir, name+".taken"))
		return err == nil
	})
}

// running starts thread id on a queued turn: its report is held first,
// then its boot is let go, and it takes the turn.
func (a *palAdapter) startThread(id string) (string, error) {
	if err := a.holdOn("report-" + id); err != nil {
		return "", err
	}
	if err := waitUntil("thread "+id+" held at boot", func() bool { _, ok := control.Booting(a.dir)[id]; return ok }); err != nil {
		return "", err
	}
	name := a.queue()
	if a.wrongStart {
		control.Release(a.t, a.dir, name)
	}
	control.ReleaseBoot(a.t, a.dir, id)
	if err := a.taken(name); err != nil {
		return "", err
	}
	a.kids = append(a.kids, id)
	if a.wrongStart {
		return "", nil
	}
	_, err := waitRow(a.s, id, "thread "+id+" running", func(r serve.Row) bool { return r.Live && r.Status == serve.StatusRunning })
	return name, err
}

// finish releases a thread's turn and waits for it to close; its report
// stays parked.
func (a *palAdapter) finish(id, turn string) error {
	control.Release(a.t, a.dir, turn)
	_, err := waitRow(a.s, id, "thread "+id+" idle", func(r serve.Row) bool { return r.Status != serve.StatusRunning })
	return err
}

// mainSettled waits until main has no open turn and every notice stored
// for it has been delivered: a respawned main wakes on those at mount,
// and with nothing queued that turn closes at once.
func (a *palAdapter) mainSettled() error {
	quiet := 0
	return waitUntil("main "+a.main+" to settle", func() bool {
		es := a.entries(a.main)
		stored, delivered := 0, 0
		woken := true // the last delivery has its wake input
		for _, e := range es {
			switch e.Kind {
			case "notice":
				stored++
			case "notice-delivered":
				delivered++
				woken = false
			case "input":
				if e.Data["reason"] == "notice" {
					woken = true
				}
			}
		}
		// A turn main holds (a block the adapter has not released) stays
		// open; any other must have closed.
		if (a.mainTurn == "" && turnOpen(es)) || delivered < stored || !woken {
			quiet = 0
			return false
		}
		quiet++
		return quiet >= 10
	})
}

// transcript is a session's entries as kind:text, for a failure.
func (a *palAdapter) transcript(id string) string {
	var out []string
	for _, e := range a.entries(id) {
		text, _ := e.Data["text"].(string)
		if len(text) > 40 {
			text = text[:40]
		}
		out = append(out, e.Kind+":"+text)
	}
	return strings.Join(out, " | ")
}

// --- actions

func (a *palAdapter) StartActive() error {
	if !a.gate.pass(a.setup == "start") {
		return nil
	}
	// 'New thread' on a project with no main: main boots first.
	p := a.post("/api/sessions", map[string]any{"mode": "project", "project": a.slug, "prompt": "task t1"})
	if err := a.waitMainBoot(); err != nil {
		return err
	}
	if err := a.holdOn("send-" + a.main); err != nil {
		return err
	}
	control.ReleaseBoot(a.t, a.dir, a.main)
	body, err := a.collect(p)
	if err != nil {
		return err
	}
	a.mains = append(a.mains, a.main)
	a.t1 = sessionID(body)
	if a.t1 == "" {
		return fmt.Errorf("new thread: no session in %v", body)
	}
	if a.t1Turn, err = a.startThread(a.t1); err != nil {
		return err
	}
	a.setup = "active"
	return a.mainSettled()
}

// waitMainBoot waits for the project's main to be held at its boot.
func (a *palAdapter) waitMainBoot() error {
	return waitUntil("main held at boot", func() bool {
		m, err := a.meta()
		if err != nil {
			return false
		}
		id := m.Mains[a.slug]
		if role, ok := control.Booting(a.dir)[id]; ok && id != "" && role == "main" {
			a.main = id
			return true
		}
		return false
	})
}

func (a *palAdapter) StartFresh() error {
	if a.gate.pass(a.setup == "start") {
		a.setup = "fresh"
	}
	return nil
}

func (a *palAdapter) ThreadFinish() error {
	st, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(st.t1 == "running" && a.arch == "idle") {
		return nil
	}
	if err := a.finish(a.t1, a.t1Turn); err != nil {
		return err
	}
	a.t1Turn, a.rep1 = "", true
	return nil
}

func (a *palAdapter) XFinish() error {
	st, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(st.x == "running") {
		return nil
	}
	if err := a.finish(a.x, a.xTurn); err != nil {
		return err
	}
	a.xTurn, a.repx = "", true
	return nil
}

func (a *palAdapter) Report1() error {
	if !a.gate.pass(a.rep1) {
		return nil
	}
	a.rep1 = false
	return a.deliver(a.t1)
}

func (a *palAdapter) ReportX() error {
	if !a.gate.pass(a.repx) {
		return nil
	}
	a.repx = false
	return a.deliver(a.x)
}

// deliver lets a thread's report go and waits for it to land: a turn
// main wakes into when it is live and idle (queued first, so it holds),
// a stored notice when it has no process.
func (a *palAdapter) deliver(thread string) error {
	st, err := a.observe()
	if err != nil {
		return err
	}
	m, err := a.meta()
	if err != nil {
		return err
	}
	before := m.Sessions[thread].Reported
	name := ""
	if st.main == "idle" {
		name = a.queue()
	}
	a.holdOff("report-" + thread)
	if err := waitUntil("thread "+thread+"'s report", func() bool {
		m, err := a.meta()
		return err == nil && m.Sessions[thread].Reported > before
	}); err != nil {
		return err
	}
	// Any later report of the thread (its exit) waits again.
	if err := a.holdOn("report-" + thread); err != nil {
		return err
	}
	switch st.main {
	case "idle":
		if err := a.taken(name); err != nil {
			return err
		}
		a.mainTurn = name
		if a.arch != "idle" && a.arch != "done" && a.wakeIn == "" {
			a.wakeIn = a.arch
		}
		_, err := waitRow(a.s, a.main, "main woken", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
		return err
	case "none":
		return waitUntil("the stored notice in main", func() bool {
			for _, e := range a.entries(a.main) {
				if e.Kind == "notice" && e.Data["from"] == thread {
					return true
				}
			}
			return false
		})
	}
	time.Sleep(100 * time.Millisecond)
	return nil
}

func (a *palAdapter) MainTurnEnds() error {
	st, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(st.main == "turn") {
		return nil
	}
	control.Release(a.t, a.dir, a.mainTurn)
	a.mainTurn = ""
	return a.mainSettled()
}

// MainSpawnsThread is main's tools.spawn, as the POST it makes.
func (a *palAdapter) MainSpawnsThread() error {
	st, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(st.main == "turn" && !st.mainArch && a.x == "" && slices.Contains([]string{"idle", "ending", "killing"}, a.arch)) {
		return nil
	}
	var out map[string]any
	if err := a.api(http.MethodPost, "/api/sessions", map[string]any{"prompt": "task x", "slug": a.slug, "spawnedBy": a.main}, &out); err != nil {
		return err
	}
	a.x = sessionID(out)
	if a.x == "" {
		return fmt.Errorf("spawn: no session in %v", out)
	}
	a.xBy, a.xSnap = "main", false
	a.xTurn, err = a.startThread(a.x)
	return err
}

func (a *palAdapter) MainBooted() error {
	st, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(st.main == "boot") {
		return nil
	}
	if err := a.holdOn("send-" + a.main); err != nil {
		return err
	}
	control.ReleaseBoot(a.t, a.dir, a.main)
	a.mains = append(a.mains, a.main)
	if a.msgBoot {
		if _, err := a.collect(a.msgReq); err != nil {
			return err
		}
		a.msgReq, a.msgBoot = nil, false
		if st.mainArch {
			a.reopened = true
		}
		return a.mainSettled()
	}
	body, err := a.collect(a.thrReq)
	a.thrReq = nil
	if err != nil {
		return err
	}
	a.x = sessionID(body)
	if a.x == "" {
		return fmt.Errorf("new thread: no session in %v", body)
	}
	a.xBy, a.xSnap = "page", false
	if a.xTurn, err = a.startThread(a.x); err != nil {
		return err
	}
	return a.mainSettled()
}

func (a *palAdapter) TakeMessage() error {
	st, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(st.msg == "sent" && (st.main == "idle" || st.main == "turn")) {
		return nil
	}
	name := ""
	if st.main == "idle" {
		name = a.queue()
	}
	a.holdOff("send-" + a.main)
	if err := waitUntil("main to take the message", func() bool { return a.recorded(a.main) }); err != nil {
		return err
	}
	if name != "" {
		if err := a.taken(name); err != nil {
			return err
		}
		a.mainTurn = name
	}
	_, err = waitRow(a.s, a.main, "main in a turn", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
	return err
}

func (a *palAdapter) MessageProject() error {
	st, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.setup != "start" && st.msg == "none" && a.view == "home" && st.main != "boot" &&
		(a.arch != "idle" || a.setup == "fresh") && a.xBy != "page") {
		return nil
	}
	a.msgIn = a.arch
	a.msgText = fmt.Sprintf("message of walk %d", a.walk)
	path, body := "/api/projects/"+a.slug+"/message", map[string]any{"text": a.msgText}
	if !st.mainHist {
		a.msgBoot = true
		a.msgReq = a.post(path, body)
		return a.waitMainBoot()
	}
	if err := a.api(http.MethodPost, path, body, nil); err != nil {
		return err
	}
	if st.mainArch {
		a.reopened = true
	}
	return a.mainSettled()
}

func (a *palAdapter) NewThread() error {
	st, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.setup != "start" && a.x == "" && a.view == "home" && st.main != "boot" && st.msg == "none" &&
		(a.arch != "idle" || a.setup == "fresh")) {
		return nil
	}
	body := map[string]any{"mode": "project", "project": a.slug, "prompt": "task x"}
	if !st.mainHist {
		a.thrReq = a.post("/api/sessions", body)
		return a.waitMainBoot()
	}
	var out map[string]any
	if err := a.api(http.MethodPost, "/api/sessions", body, &out); err != nil {
		return err
	}
	a.x = sessionID(out)
	if a.x == "" {
		return fmt.Errorf("new thread: no session in %v", out)
	}
	a.xBy, a.xSnap = "page", false
	a.xTurn, err = a.startThread(a.x)
	return err
}

func (a *palAdapter) OpenMain() error {
	st, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.view == "home" && st.mainHist && st.mainArch && a.arch == "done" && !a.rep1 && !a.repx && st.msg == "none") {
		return nil
	}
	// The page opens main's conversation: the session and its lines.
	if err := a.api(http.MethodGet, "/api/sessions/"+a.main, nil, nil); err != nil {
		return err
	}
	a.view = "main"
	return nil
}

func (a *palAdapter) Back() error {
	if a.gate.pass(a.view == "main") {
		a.view = "home"
	}
	return nil
}

func (a *palAdapter) ArchiveProject() error {
	st, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.setup != "start" && a.arch == "idle" && (a.setup == "active" || st.main == "boot")) {
		return nil
	}
	for _, p := range []string{"archive-ending", "archive-killing", "archive-flagging"} {
		if err := a.holdOn(p); err != nil {
			return err
		}
	}
	a.mid, a.xSnap = st.mainHist, a.x != ""
	a.archReq = a.post("/api/projects/"+a.slug+"/archive", nil)
	a.arch = "ending"
	return a.parkedAt("archive-ending")
}

func (a *palAdapter) EndThreads() error {
	if !a.gate.pass(a.arch == "ending") {
		return nil
	}
	st, err := a.observe()
	if err != nil {
		return err
	}
	a.holdOff("archive-ending")
	if a.wrongKill {
		a.holdOff("archive-killing")
		if err := a.parkedAt("archive-flagging"); err != nil {
			return err
		}
	} else if err := a.parkedAt("archive-killing"); err != nil {
		return err
	}
	if st.t1 == "running" {
		a.rep1, a.t1Turn = true, ""
	}
	if a.xSnap && st.x == "running" {
		a.repx, a.xTurn = true, ""
	}
	a.arch = "killing"
	// A main with no history at the snapshot is ended with the threads.
	return a.afterKill(st, "kill")
}

func (a *palAdapter) KillMain() error {
	if !a.gate.pass(a.arch == "killing") {
		return nil
	}
	before, err := a.observe()
	if err != nil {
		return err
	}
	a.holdOff("archive-killing")
	if err := a.parkedAt("archive-flagging"); err != nil {
		return err
	}
	a.arch = "flagging"
	return a.afterKill(before, "kill")
}

// afterKill notes a message the kill lost, and drops main's held turn.
func (a *palAdapter) afterKill(before palState, by string) error {
	after, err := a.observe()
	if err != nil {
		return err
	}
	if !after.mainLive {
		a.mainTurn = ""
	}
	if before.msg == "sent" && after.msg == "lost" {
		a.lostBy = by
	}
	return nil
}

func (a *palAdapter) FlagAll() error {
	if !a.gate.pass(a.arch == "flagging") {
		return nil
	}
	before, err := a.observe()
	if err != nil {
		return err
	}
	a.holdOff("archive-flagging")
	_, err = a.collect(a.archReq)
	a.archReq = nil
	if err != nil {
		return err
	}
	a.arch = "done"
	after, err := a.observe()
	if err != nil {
		return err
	}
	switch {
	case after.xLive && !after.xArch:
		a.left = "thread"
	case after.main == "boot":
		a.left = "main"
	}
	return a.afterKill(before, "flag")
}

// palAction logs each step a walk takes (the gate still open).
func palAction(name string, f func(*palAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*palAdapter)
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

var palActions = map[string]map[string]fmbt.ActionFunc{"Project": {
	"StartActive":      palAction("StartActive", (*palAdapter).StartActive),
	"StartFresh":       palAction("StartFresh", (*palAdapter).StartFresh),
	"ThreadFinish":     palAction("ThreadFinish", (*palAdapter).ThreadFinish),
	"XFinish":          palAction("XFinish", (*palAdapter).XFinish),
	"Report1":          palAction("Report1", (*palAdapter).Report1),
	"ReportX":          palAction("ReportX", (*palAdapter).ReportX),
	"MainTurnEnds":     palAction("MainTurnEnds", (*palAdapter).MainTurnEnds),
	"MainSpawnsThread": palAction("MainSpawnsThread", (*palAdapter).MainSpawnsThread),
	"MainBooted":       palAction("MainBooted", (*palAdapter).MainBooted),
	"TakeMessage":      palAction("TakeMessage", (*palAdapter).TakeMessage),
	"MessageProject":   palAction("MessageProject", (*palAdapter).MessageProject),
	"NewThread":        palAction("NewThread", (*palAdapter).NewThread),
	"OpenMain":         palAction("OpenMain", (*palAdapter).OpenMain),
	"Back":             palAction("Back", (*palAdapter).Back),
	"ArchiveProject":   palAction("ArchiveProject", (*palAdapter).ArchiveProject),
	"EndThreads":       palAction("EndThreads", (*palAdapter).EndThreads),
	"KillMain":         palAction("KillMain", (*palAdapter).KillMain),
	"FlagAll":          palAction("FlagAll", (*palAdapter).FlagAll),
}, "": {
	// fizz links a state with nothing enabled to itself as "end" (an
	// archived project with nothing left to do); nothing happens, and the
	// state is read again.
	"end": func(any, []fmbt.Arg) (any, error) { return nil, nil },
}}

// projectArchiveLiveHistory reads one transcript of the flow as a path
// through the spec. A transcript sees only its own session, so the steps
// it cannot see are filled in the one way that fits:
//
//   - a thread (meta names a parent): the active project's t1, its task
//     running from the start; a closed turn is its ThreadFinish. A
//     second input in a thread is not a path.
//   - main: an active project's main. Its first notice wake is t1's
//     report (ThreadFinish before the archive, EndThreads after it), and
//     main spawns x in the turn it wakes into; the second is x's. A person's line
//     (an input or a steer) is the project's one message, sent during an
//     archive; a close ends main's turn. A third notice, or a second
//     message, is not a path.
func projectArchiveLiveHistory(entries []history.Entry) []tracecheck.Step {
	st := func(kv ...any) map[string]any {
		m := map[string]any{}
		for i := 0; i+1 < len(kv); i += 2 {
			m["Project#0."+kv[i].(string)] = kv[i+1]
		}
		return m
	}
	step := func(action string, kv ...any) tracecheck.Step {
		return tracecheck.Step{Action: "Project#0." + action, State: st(kv...)}
	}
	steps := []tracecheck.Step{{Action: "Init", State: st("setup", "start", "main", "none")}}
	thread, archived, open := false, false, false
	notices, messages, inputs := 0, 0, 0
	for _, e := range entries {
		switch e.Kind {
		case "meta":
			thread = e.Data["spawned_by"] != nil && e.Data["spawned_by"] != ""
			steps = append(steps, step("StartActive", "main", "idle", "t1", "running"))
		case "input":
			inputs++
			if thread {
				if inputs > 1 {
					steps = append(steps, step("ThreadMessage"))
				}
				open = true
				continue
			}
			if e.Data["reason"] == "notice" {
				notices++
				switch {
				case notices == 1 && !archived:
					steps = append(steps, step("ThreadFinish", "rep1", true), step("Report1", "main", "turn"),
						step("MainSpawnsThread", "x", "running", "x_by", "main"))
				case notices == 1:
					steps = append(steps, step("EndThreads", "rep1", true), step("Report1"),
						step("MainSpawnsThread", "x", "running", "x_by", "main"))
				case notices == 2:
					steps = append(steps, step("XFinish", "repx", true), step("ReportX", "main", "turn"))
				default:
					steps = append(steps, step("ReportX"))
				}
				open = true
				continue
			}
			messages++
			if messages == 1 && !archived {
				archived = true
				steps = append(steps, step("ArchiveProject", "arch", "ending"))
			}
			steps = append(steps, step("MessageProject", "msg", "sent"), step("TakeMessage", "msg", "recorded", "main", "turn"))
			open = true
		case "done", "cancelled":
			if !open {
				continue
			}
			open = false
			if thread {
				steps = append(steps, step("ThreadFinish", "t1", "idle", "rep1", true))
			} else {
				steps = append(steps, step("MainTurnEnds", "main", "idle"))
			}
		}
	}
	return steps
}

func init() { historyProjections["project_archive_live"] = projectArchiveLiveHistory }

func palOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 12, "max-parallel-runs": 0}
}

func (a *palAdapter) checkTranscripts(t *testing.T, g *tracecheck.Graph) int {
	checked := 0
	for _, id := range append(slices.Clone(a.mains), a.kids...) {
		if a.hasHistory(id) {
			checkHistory(t, g, sessionHistory(t, a.s.Home, id), projectArchiveLiveHistory)
			checked++
		}
	}
	return checked
}

func TestProjectArchiveLive(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPALAdapter(t)
	if err := runMBT(t, "project_archive_live", a, palActions, palOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "project_archive_live"))
	if err != nil {
		t.Fatal(err)
	}
	a.checkTranscripts(t, g)
}

func TestProjectArchiveLiveCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPALAdapter(t)
	a.wrongStart = true
	if err := runMBT(t, "project_archive_live", a, palActions, palOptions()); err == nil {
		t.Fatal("a run whose StartActive leaves t1 idle passed; the runner is not checking state")
	}
}

// walkPath drives one generated path and compares the state read off
// serve with the path's after every step.
func (a *palAdapter) walkPath(trace []tracecheck.Step) (err error) {
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
		if i > 0 && name != "end" {
			f, ok := palActions["Project"][name]
			if !ok {
				return fmt.Errorf("step %d: no action %q", i, name)
			}
			if _, err := f(a, nil); err != nil {
				return fmt.Errorf("step %d (%s): %w\nmain's transcript: %s", i, name, err, a.transcript(a.main))
			}
			if a.gate.off {
				return fmt.Errorf("step %d (%s): the adapter reads it as not enabled", i, name)
			}
		}
		have, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w", i, name, err)
		}
		if diff := palDiff(s.State, have); diff != "" {
			return fmt.Errorf("step %d (%s): state differs from the spec's:%s", i, name, diff)
		}
	}
	return nil
}

func palDiff(want, have map[string]any) string {
	var out []string
	for k, w := range want {
		f, ok := strings.CutPrefix(k, "Project#0.")
		if !ok {
			continue
		}
		if g := have[f]; fmt.Sprint(g) != fmt.Sprint(w) {
			out = append(out, fmt.Sprintf("\n  %s: spec %v, serve %v", f, w, g))
		}
	}
	slices.Sort(out)
	return strings.Join(out, "")
}

func palPaths(t *testing.T, cover tracecheck.Cover) [][]tracecheck.Step {
	t.Helper()
	b, err := pathsJSONCover("project_archive_live", cover)
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

// TestProjectArchiveLivePaths walks every generated path (every settled
// state; every link under MODEL_COVER=transitions) against real serves,
// then replays every transcript the walks wrote on the graph.
func TestProjectArchiveLivePaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	paths := palPaths(t, envCover())
	g, err := tracecheck.Load(fizzCheck(t, "project_archive_live"))
	if err != nil {
		t.Fatal(err)
	}
	const shards = 4
	for n := range shards {
		t.Run(fmt.Sprintf("serve%d", n), func(t *testing.T) {
			t.Parallel()
			a := newPALAdapter(t)
			for i := n; i < len(paths); i += shards {
				if err := a.walkPath(paths[i]); err != nil {
					t.Errorf("path %d: %v", i, err)
				}
			}
			checked := a.checkTranscripts(t, g)
			if checked == 0 {
				t.Error("no transcripts for the trace check")
			}
			t.Logf("walked %d paths, trace-checked %d transcripts", (len(paths)-n+shards-1)/shards, checked)
		})
	}
}

// The walk must fail on a wrong adapter: one that lets the archive
// through EndProject's Kill(main) together with the thread ends kills
// main a step before the spec does. It stops at the first path that
// says so.
func TestProjectArchiveLivePathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPALAdapter(t)
	a.wrongKill = true
	for i, p := range palPaths(t, tracecheck.CoverStates) {
		if err := a.walkPath(p); err != nil {
			t.Logf("path %d caught it: %v", i, err)
			return
		}
	}
	t.Fatal("every path passed with EndThreads killing main; the walk is not checking state")
}
