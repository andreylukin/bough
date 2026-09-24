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

// specs/project_main_threads.fizz against a real serve: one project per
// walk, its main thread, one thread and the report the thread's turn
// leaves in main. The adapter plays the project page (a) and a second
// tab (b).
//
// Every child runs on the fake container runtime (the orb row's
// `runtime: fake`, which serve follows too), and llm-control's
// hold_boot keeps each new session before its history file until the
// adapter releases it: that is the spec's "starting", for main
// (MainReady) and for a 'New thread' thread (ThreadBoot).
//
// The spec's request fields (a, b, lock) are the adapter's own: a POST
// is "wait" until the adapter sends it, and "held" once serve is inside
// Main's lock, which the adapter sees as main's boot being held. A
// request that would not block (main already live) is sent at its Done
// step, since serve would otherwise finish it before the spec does.
// Everything else is read off the server: main from meta.json's mains
// and the history dir, mains from every project session with no parent,
// the thread from the project page's list, its report from main's
// transcript.
const pmtConfig = "- id: llm\n  plugin: llm-control\n  config:\n    hold_boot: true\n" +
	"- id: orb\n  plugin: orb\n  config:\n    runtime: fake\n"

type pmtAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	walk int
	slug string

	a, b, lock string
	asked      bool
	view       string
	pa, pb     *pmtReq // sent, not yet collected

	thread string // the thread on the page, "" for none
	held   string // the thread's turn in flight
	turn   int
	msg    int
	mains  []string // every main id a walk made, for the trace check
	kids   []string // every thread id

	// The deliberate wiring bugs the CatchesWrongAdapter tests inject:
	// Poll messages the project (a read that starts main), and 'New
	// thread' sends a task (the thread boots into a turn, not empty).
	pollSends, newThreadPrompt bool
}

// pmtReq is one POST in flight on its own goroutine.
type pmtReq struct {
	text string // a message's text, "" for a 'New thread'
	done chan pmtResult
}

type pmtResult struct {
	body map[string]any
	err  error
}

func newPMTAdapter(t *testing.T) *pmtAdapter {
	s := servetest.Start(t, servetest.Options{Config: pmtConfig})
	return &pmtAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

// Init starts each walk on a fresh project in the same serve.
func (a *pmtAdapter) Init() error {
	a.walk++
	var r struct {
		Project serve.Project `json:"project"`
	}
	if err := a.api(http.MethodPost, "/api/projects", map[string]string{"name": fmt.Sprintf("Walk %d", a.walk)}, &r); err != nil {
		return err
	}
	a.slug = r.Project.Slug
	a.a, a.b, a.lock, a.asked, a.view = "idle", "idle", "", false, "home"
	a.pa, a.pb, a.thread, a.held = nil, nil, "", ""
	a.gate.reset()
	return nil
}

// Cleanup lets everything the walk left held go and stops the project,
// so the next walk's turns are not taken by this one's processes.
func (a *pmtAdapter) Cleanup() error {
	for id := range control.Booting(a.dir) {
		control.ReleaseBoot(a.t, a.dir, id)
	}
	if a.held != "" {
		control.Release(a.t, a.dir, a.held)
		a.held = ""
	}
	var errs []error
	for _, p := range []*pmtReq{a.pa, a.pb} {
		if p != nil {
			if _, err := a.collect(p); err != nil {
				errs = append(errs, err)
			}
		}
	}
	a.pa, a.pb = nil, nil
	if a.slug != "" {
		errs = append(errs, a.api(http.MethodPost, "/api/projects/"+a.slug+"/archive", nil, nil))
	}
	return errors.Join(errs...)
}

func (a *pmtAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Project", Index: 0}: a}, nil
}

// pmtMeta is the part of serve's meta.json the adapter reads: the
// recorded main per slug, and each session's membership and flags.
type pmtMeta struct {
	Sessions map[string]serve.SessionMeta `json:"sessions"`
	Mains    map[string]string            `json:"mains"`
}

func (a *pmtAdapter) meta() (pmtMeta, error) {
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

func (a *pmtAdapter) histPath(id string) string {
	return filepath.Join(a.s.Home, ".bough", "history", id+".jsonl")
}

func (a *pmtAdapter) hasHistory(id string) bool {
	_, err := os.Stat(a.histPath(id))
	return err == nil
}

// mainState is the spec's main, read off serve: the recorded id, and
// whether its history file exists or its boot is being held.
func (a *pmtAdapter) mainState() (string, string, error) {
	m, err := a.meta()
	if err != nil {
		return "", "", err
	}
	id := m.Mains[a.slug]
	switch {
	case id == "":
		return "none", "", nil
	case a.hasHistory(id):
		return "live", id, nil
	}
	// The held process must know it is main (BOUGH_PROJECT_MAIN): serve's
	// first start of main once did not say so, and main ran as an
	// ordinary project session until its first restart.
	if role, held := control.Booting(a.dir)[id]; held {
		if role != "main" {
			return "", "", fmt.Errorf("main %s is starting as a %q, not as main", id, role)
		}
		return "starting", id, nil
	}
	return "gone", id, nil
}

// mainCount is the spec's mains: every session of the project with no
// parent whose history exists or whose boot is being held.
func (a *pmtAdapter) mainCount() (int, error) {
	m, err := a.meta()
	if err != nil {
		return 0, err
	}
	booting := control.Booting(a.dir)
	n := 0
	for id, sm := range m.Sessions {
		if sm.Project != a.slug || sm.SpawnedBy != "" {
			continue
		}
		if _, held := booting[id]; held || a.hasHistory(id) {
			n++
		}
	}
	return n, nil
}

type pmtDetail struct {
	Main         string      `json:"main"`
	MainArchived bool        `json:"mainArchived"`
	Threads      []serve.Row `json:"threads"`
}

func (a *pmtAdapter) detail() (pmtDetail, error) {
	var d pmtDetail
	err := a.api(http.MethodGet, "/api/projects/"+a.slug, nil, &d)
	return d, err
}

// GetState is the Project role's state.
func (a *pmtAdapter) GetState() (map[string]any, error) {
	main, mainID, err := a.mainState()
	if err != nil {
		return nil, err
	}
	mains, err := a.mainCount()
	if err != nil {
		return nil, err
	}
	d, err := a.detail()
	if err != nil {
		return nil, err
	}
	// The page reads the same main serve records, or none at all.
	if want := map[bool]string{true: mainID, false: ""}[main == "live"]; d.Main != want {
		return nil, fmt.Errorf("project page main = %q, meta.json records %q (%s)", d.Main, mainID, main)
	}
	// ListsAgree: the sidebar's group for the project is the page's list.
	rows, err := a.s.ListSessions(context.Background(), false)
	if err != nil {
		return nil, err
	}
	var page, side []string
	for _, r := range d.Threads {
		page = append(page, r.ID)
	}
	for _, r := range rows {
		if r.Project == a.slug && !r.Archived && r.ID != d.Main && r.ID != mainID {
			side = append(side, r.ID)
		}
	}
	slices.Sort(page)
	slices.Sort(side)
	if !slices.Equal(page, side) {
		return nil, fmt.Errorf("project page lists threads %v, the sidebar %v", page, side)
	}
	thread, parented, full := "none", false, false
	switch len(d.Threads) {
	case 0:
	case 1:
		row := d.Threads[0]
		if thread, err = a.threadState(row.ID, mainID); err != nil {
			return nil, err
		}
		m, err := a.meta()
		if err != nil {
			return nil, err
		}
		parented = row.SpawnedBy != "" && row.SpawnedBy == mainID
		full = m.Sessions[row.ID].Thread
	default:
		return nil, fmt.Errorf("project lists %d threads, the spec has at most one: %v", len(d.Threads), page)
	}
	return map[string]any{
		"main": main, "mains": mains, "archived": d.MainArchived,
		"lock": a.lock, "a": a.a, "b": a.b, "asked": a.asked, "view": a.view,
		"thread": thread, "parented": parented, "full": full,
	}, nil
}

// threadState reads a listed thread: "starting" before its history file
// exists, "empty" before its first input, "running" in a turn, and
// "reported" once every turn it closed has its notice in main. A closed
// turn with no notice reads "done", which the spec has no state for.
func (a *pmtAdapter) threadState(id, main string) (string, error) {
	if !a.hasHistory(id) {
		return "starting", nil
	}
	es, err := history.Read(a.histPath(id))
	if err != nil {
		return "", err
	}
	inputs, closed := 0, 0
	open := false
	for _, e := range es {
		switch e.Kind {
		case "input":
			inputs++
			open = true
		case "done", "cancelled":
			if open {
				closed++
			}
			open = false
		}
	}
	switch {
	case inputs == 0:
		return "empty", nil
	case open:
		return "running", nil
	case main != "" && a.notices(main, id) >= closed:
		return "reported", nil
	}
	return "done", nil
}

// notices counts the reports from thread in main's transcript: each is
// an input main woke on, naming the thread.
func (a *pmtAdapter) notices(main, thread string) int {
	es, err := history.Read(a.histPath(main))
	if err != nil {
		return 0
	}
	n := 0
	for _, e := range es {
		if e.Kind != "input" || e.Data["reason"] != "notice" {
			continue
		}
		if text, _ := e.Data["text"].(string); strings.Contains(text, thread) {
			n++
		}
	}
	return n
}

// --- requests

// api is one JSON call as the page makes it.
func (a *pmtAdapter) api(method, path string, body, out any) error {
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

// send starts a message (text != "") or a 'New thread' POST.
func (a *pmtAdapter) send(text string) *pmtReq {
	p := &pmtReq{text: text, done: make(chan pmtResult, 1)}
	path, body := "/api/projects/"+a.slug+"/message", map[string]any{"text": text}
	if text == "" {
		prompt := ""
		if a.newThreadPrompt {
			prompt = "a task nobody typed"
		}
		path, body = "/api/sessions", map[string]any{"mode": "project", "project": a.slug, "prompt": prompt}
	}
	go func() {
		var out map[string]any
		err := a.api(http.MethodPost, path, body, &out)
		p.done <- pmtResult{out, err}
	}()
	return p
}

// collect waits for a request and, for a message, for main's turn on
// it to close: an open turn on main would take the next queued turn.
func (a *pmtAdapter) collect(p *pmtReq) (map[string]any, error) {
	var r pmtResult
	select {
	case r = <-p.done:
	case <-time.After(actionTimeout):
		return nil, fmt.Errorf("request %q did not return in %s", p.text, actionTimeout)
	}
	if r.err != nil {
		return nil, r.err
	}
	if p.text != "" {
		main, _ := r.body["main"].(string)
		return r.body, a.mainQuiet(main, func(es []history.Entry) bool {
			for _, e := range es {
				if e.Kind == "input" && e.Data["text"] == p.text {
					return true
				}
			}
			return false
		})
	}
	return r.body, nil
}

// mainQuiet waits until main's transcript satisfies has and its last
// turn is closed.
func (a *pmtAdapter) mainQuiet(main string, has func([]history.Entry) bool) error {
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		es, err := history.Read(a.histPath(main))
		if err == nil && has(es) {
			if !turnOpen(es) {
				return nil
			}
		}
		time.Sleep(20 * time.Millisecond)
	}
	return fmt.Errorf("main %s: not settled after %s", main, actionTimeout)
}

// waitMainBoot waits for a main of this project to be held at boot:
// the request sent is inside Main's lock and in Create.
func (a *pmtAdapter) waitMainBoot() error {
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		st, _, err := a.mainState()
		if err != nil {
			return err
		}
		if st == "starting" {
			return nil
		}
		time.Sleep(10 * time.Millisecond)
	}
	return fmt.Errorf("project %s: no main held at boot after %s", a.slug, actionTimeout)
}

// settle gives a server that would wrongly mint a second main (a caller
// let past the lock during Create) the time to show it.
func settle() { time.Sleep(200 * time.Millisecond) }

func (a *pmtAdapter) nextMessage() string {
	a.msg++
	return fmt.Sprintf("message %d of walk %d", a.msg, a.walk)
}

func (a *pmtAdapter) mainNow() string {
	_, id, _ := a.mainState()
	return id
}

// --- actions

func (a *pmtAdapter) Poll() error {
	if !a.gate.pass(a.mainIs("none")) {
		return nil
	}
	if a.pollSends {
		a.pa = a.send(a.nextMessage())
		return a.waitMainBoot()
	}
	_, err := a.detail()
	return err
}

func (a *pmtAdapter) mainIs(want string) bool {
	st, _, err := a.mainState()
	return err == nil && st == want
}

func (a *pmtAdapter) Message() error {
	if !a.gate.pass(a.a == "idle" && (a.b == "idle" || a.mainIs("none"))) {
		return nil
	}
	a.a, a.asked = "msg-wait", true
	if a.lock == "b" {
		// The other tab is inside Main: this one queues on the lock.
		a.pa = a.send(a.nextMessage())
		settle()
	}
	return nil
}

func (a *pmtAdapter) NewThread() error {
	if !a.gate.pass(a.a == "idle" && a.b == "idle" && a.thread == "" && !a.archived()) {
		return nil
	}
	a.a, a.asked = "thread-wait", true
	return nil
}

func (a *pmtAdapter) archived() bool {
	d, err := a.detail()
	return err == nil && d.MainArchived
}

func (a *pmtAdapter) MessageB() error {
	if !a.gate.pass(a.b == "idle" && a.mainIs("none") && a.a == "idle") {
		return nil
	}
	a.b, a.asked = "wait", true
	return nil
}

func (a *pmtAdapter) EnterA() error {
	if !a.gate.pass((a.a == "msg-wait" || a.a == "thread-wait") && a.lock == "") {
		return nil
	}
	live := a.mainIs("live")
	a.lock = "a"
	if a.a == "msg-wait" {
		a.a = "msg-held"
	} else {
		a.a = "thread-held"
	}
	if live || a.pa != nil {
		// Main() returns at once (or already did, queued behind b):
		// the rest is DoneA's.
		return nil
	}
	text := ""
	if a.a == "msg-held" {
		text = a.nextMessage()
	}
	a.pa = a.send(text)
	if err := a.waitMainBoot(); err != nil {
		return err
	}
	if a.b == "wait" && a.pb == nil {
		a.pb = a.send(a.nextMessage())
	}
	settle()
	return nil
}

func (a *pmtAdapter) EnterB() error {
	if !a.gate.pass(a.b == "wait" && a.lock == "") {
		return nil
	}
	live := a.mainIs("live")
	a.lock, a.b = "b", "held"
	if live {
		return nil
	}
	a.pb = a.send(a.nextMessage())
	if err := a.waitMainBoot(); err != nil {
		return err
	}
	if a.a == "msg-wait" && a.pa == nil {
		a.pa = a.send(a.nextMessage())
	}
	settle()
	return nil
}

func (a *pmtAdapter) MainReady() error {
	st, id, err := a.mainState()
	if err != nil {
		return err
	}
	if !a.gate.pass(st == "starting") {
		return nil
	}
	control.ReleaseBoot(a.t, a.dir, id)
	deadline := time.Now().Add(actionTimeout)
	for !a.hasHistory(id) {
		if time.Now().After(deadline) {
			return fmt.Errorf("main %s: no history after its boot was released", id)
		}
		time.Sleep(10 * time.Millisecond)
	}
	a.mains = append(a.mains, id)
	// Every request queued on the lock goes through now; wait for each
	// so main's turns on them are closed before the next step.
	for _, p := range []**pmtReq{&a.pa, &a.pb} {
		if *p == nil {
			continue
		}
		body, err := a.collect(*p)
		if err != nil {
			return err
		}
		if (*p).text == "" {
			// A 'New thread' is created in the same handler: the page
			// opens it (the spec's fused DoneA).
			if err := a.opened(body); err != nil {
				return err
			}
			*p = nil
			a.lock, a.a, a.view = "", "idle", "thread"
			continue
		}
		*p = &pmtReq{text: (*p).text, done: doneWith(body)}
	}
	return nil
}

// doneWith is a request already collected, for its Done step.
func doneWith(body map[string]any) chan pmtResult {
	c := make(chan pmtResult, 1)
	c <- pmtResult{body: body}
	return c
}

// opened records the thread a 'New thread' POST created.
func (a *pmtAdapter) opened(body map[string]any) error {
	row, _ := body["session"].(map[string]any)
	id, _ := row["id"].(string)
	if id == "" {
		return fmt.Errorf("new thread: no session in %v", body)
	}
	a.thread = id
	a.kids = append(a.kids, id)
	return nil
}

func (a *pmtAdapter) DoneA() error {
	if !a.gate.pass((a.a == "msg-held" || a.a == "thread-held") && a.mainIs("live")) {
		return nil
	}
	if a.pa == nil {
		text := ""
		if a.a == "msg-held" {
			text = a.nextMessage()
		}
		a.pa = a.send(text)
	}
	body, err := a.collect(a.pa)
	a.pa = nil
	if err != nil {
		return err
	}
	a.lock = ""
	if a.a == "msg-held" {
		a.view = "main"
	} else {
		if err := a.opened(body); err != nil {
			return err
		}
		a.view = "thread"
	}
	a.a = "idle"
	return nil
}

func (a *pmtAdapter) DoneB() error {
	if !a.gate.pass(a.b == "held" && a.mainIs("live")) {
		return nil
	}
	if a.pb == nil {
		a.pb = a.send(a.nextMessage())
	}
	_, err := a.collect(a.pb)
	a.pb = nil
	a.lock, a.b = "", "idle"
	return err
}

func (a *pmtAdapter) StartThread() error {
	if !a.gate.pass(a.view == "main" && a.mainIs("live") && !a.archived() && a.a == "idle" && a.lock == "" && a.thread == "") {
		return nil
	}
	name := a.queueTurn()
	var out map[string]any
	if err := a.api(http.MethodPost, "/api/sessions", map[string]any{"mode": "project", "project": a.slug, "prompt": "task " + name}, &out); err != nil {
		return err
	}
	if err := a.opened(out); err != nil {
		return err
	}
	control.ReleaseBoot(a.t, a.dir, a.thread)
	return a.running(name)
}

// queueTurn queues the thread's next turn, held until ThreadFinish.
func (a *pmtAdapter) queueTurn() string {
	a.turn++
	name := fmt.Sprintf("p%04d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	return name
}

// running waits for the thread to take turn name and read running.
func (a *pmtAdapter) running(name string) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		if _, err := os.Stat(filepath.Join(a.dir, name+".taken")); err == nil {
			break
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("thread %s: turn %s not taken after %s", a.thread, name, actionTimeout)
		}
		time.Sleep(10 * time.Millisecond)
	}
	a.held = name
	_, err := waitRow(a.s, a.thread, "the thread running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
	return err
}

func (a *pmtAdapter) ThreadBoot() error {
	if !a.gate.pass(a.thread != "" && !a.hasHistory(a.thread)) {
		return nil
	}
	control.ReleaseBoot(a.t, a.dir, a.thread)
	deadline := time.Now().Add(actionTimeout)
	for !a.hasHistory(a.thread) {
		if time.Now().After(deadline) {
			return fmt.Errorf("thread %s: no history after its boot was released", a.thread)
		}
		time.Sleep(10 * time.Millisecond)
	}
	// launch writes a prompt, when there is one, right after the file
	// appears: give it the time to show, so "empty" is a fact.
	settle()
	return nil
}

func (a *pmtAdapter) threadIs(states ...string) bool {
	if a.thread == "" {
		return false
	}
	st, err := a.threadState(a.thread, a.mainNow())
	return err == nil && slices.Contains(states, st)
}

func (a *pmtAdapter) ThreadMessage() error {
	if !a.gate.pass(a.view == "thread" && a.threadIs("empty", "reported")) {
		return nil
	}
	name := a.queueTurn()
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.thread, "turn "+name); err != nil {
		return err
	}
	return a.running(name)
}

func (a *pmtAdapter) ThreadFinish() error {
	if !a.gate.pass(a.held != "") {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	if _, err := waitRow(a.s, a.thread, "the thread's turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning }); err != nil {
		return err
	}
	// The report wakes main; its turn on the notice must close before a
	// later step queues a turn it would take.
	es, err := history.Read(a.histPath(a.thread))
	if err != nil {
		return err
	}
	closed := 0
	for _, e := range es {
		if e.Kind == "done" {
			closed++
		}
	}
	main := a.mainNow()
	thread := a.thread
	if err := a.mainQuiet(main, func([]history.Entry) bool { return a.notices(main, thread) >= closed }); err != nil {
		return fmt.Errorf("thread %s finished; its report is not in main %s: %w", thread, main, err)
	}
	return nil
}

func (a *pmtAdapter) ArchiveThread() error {
	if !a.gate.pass(a.threadIs("empty", "reported")) {
		return nil
	}
	if err := a.api(http.MethodPost, "/api/sessions/"+url.PathEscape(a.thread)+"/archive", nil, nil); err != nil {
		return err
	}
	a.thread = ""
	if a.view == "thread" {
		a.view = "home"
	}
	return nil
}

func (a *pmtAdapter) ArchiveProject() error {
	if !a.gate.pass(a.mainIs("live") && !a.archived() && a.a == "idle" && a.b == "idle" && a.thread == "") {
		return nil
	}
	return a.api(http.MethodPost, "/api/projects/"+a.slug+"/archive", nil, nil)
}

func (a *pmtAdapter) OpenMain() error {
	if a.gate.pass(a.view != "main" && a.mainIs("live") && a.a == "idle" && a.b == "idle") {
		a.view = "main"
	}
	return nil
}

func (a *pmtAdapter) OpenThread() error {
	if a.gate.pass(a.view != "thread" && a.thread != "" && a.a == "idle") {
		a.view = "thread"
	}
	return nil
}

func (a *pmtAdapter) Back() error {
	if a.gate.pass(a.view != "home" && a.a == "idle" && a.b == "idle") {
		a.view = "home"
	}
	return nil
}

// LoseMainHistory deletes main's transcript, the way a person cleaning
// ~/.bough/history would. Its process runs on, writing to nothing.
func (a *pmtAdapter) LoseMainHistory() error {
	st, id, err := a.mainState()
	if err != nil {
		return err
	}
	if !a.gate.pass(st == "live" && !a.archived() && a.a == "idle" && a.b == "idle" && a.thread == "") {
		return nil
	}
	if err := os.Remove(a.histPath(id)); err != nil {
		return err
	}
	if a.view == "main" {
		a.view = "home"
	}
	return nil
}

// pmtAction logs each step a walk actually takes (the gate still open),
// with its time: when a run fails, the log is the walk.
func pmtAction(name string, f func(*pmtAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*pmtAdapter)
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

var pmtActions = map[string]map[string]fmbt.ActionFunc{"Project": {
	"Poll":            pmtAction("Poll", (*pmtAdapter).Poll),
	"Message":         pmtAction("Message", (*pmtAdapter).Message),
	"NewThread":       pmtAction("NewThread", (*pmtAdapter).NewThread),
	"MessageB":        pmtAction("MessageB", (*pmtAdapter).MessageB),
	"EnterA":          pmtAction("EnterA", (*pmtAdapter).EnterA),
	"EnterB":          pmtAction("EnterB", (*pmtAdapter).EnterB),
	"MainReady":       pmtAction("MainReady", (*pmtAdapter).MainReady),
	"DoneA":           pmtAction("DoneA", (*pmtAdapter).DoneA),
	"DoneB":           pmtAction("DoneB", (*pmtAdapter).DoneB),
	"StartThread":     pmtAction("StartThread", (*pmtAdapter).StartThread),
	"ThreadBoot":      pmtAction("ThreadBoot", (*pmtAdapter).ThreadBoot),
	"ThreadMessage":   pmtAction("ThreadMessage", (*pmtAdapter).ThreadMessage),
	"ThreadFinish":    pmtAction("ThreadFinish", (*pmtAdapter).ThreadFinish),
	"ArchiveThread":   pmtAction("ArchiveThread", (*pmtAdapter).ArchiveThread),
	"ArchiveProject":  pmtAction("ArchiveProject", (*pmtAdapter).ArchiveProject),
	"OpenMain":        pmtAction("OpenMain", (*pmtAdapter).OpenMain),
	"OpenThread":      pmtAction("OpenThread", (*pmtAdapter).OpenThread),
	"Back":            pmtAction("Back", (*pmtAdapter).Back),
	"LoseMainHistory": pmtAction("LoseMainHistory", (*pmtAdapter).LoseMainHistory),
}}

// turnOpen says whether a transcript's last input has no close yet.
func turnOpen(es []history.Entry) bool {
	open := false
	for _, e := range es {
		switch e.Kind {
		case "input":
			open = true
		case "done", "cancelled":
			open = false
		}
	}
	return open
}

// projectMainThreadsHistory reads a main thread's or a thread's
// transcript as a path through the spec. One transcript sees neither
// the page nor the other session, so the steps it cannot see are filled
// in the one way that fits, and only what it records is checked:
//
//   - main: its meta entry is a first message that booted it (Message,
//     EnterA, MainReady), its first person input closes that request
//     (DoneA) and each later one is a message of its own; each notice
//     is a thread started from main that finished and reported
//     (StartThread, ThreadFinish), then left (ArchiveThread).
//   - a thread (meta names a parent): main exists, a 'New thread'
//     created it and it booted empty; each input is a message in it
//     (ThreadMessage), each close of a turn its report (ThreadFinish).
//
// A transcript that is not a path (a main with no meta, an input in a
// thread before it booted) fails the check.
func projectMainThreadsHistory(entries []history.Entry) []tracecheck.Step {
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
	steps := []tracecheck.Step{{Action: "Init", State: st("main", "none", "thread", "none")}}
	boot := []tracecheck.Step{
		step("Message", "a", "msg-wait"),
		step("EnterA", "main", "starting", "mains", 1),
		step("MainReady", "main", "live"),
	}
	isThread := false
	pending := false // main's first message is not closed yet
	open := false
	for _, e := range entries {
		switch e.Kind {
		case "meta":
			steps = append(steps, boot...)
			if by, _ := e.Data["spawned_by"].(string); by != "" {
				isThread = true
				steps = append(steps,
					step("DoneA", "a", "idle", "view", "main"),
					step("NewThread", "a", "thread-wait"),
					step("EnterA", "a", "thread-held"),
					step("DoneA", "thread", "starting", "view", "thread"),
					step("ThreadBoot", "thread", "empty"))
			} else {
				pending = true
			}
		case "input":
			open = true
			switch {
			case isThread:
				steps = append(steps, step("ThreadMessage", "thread", "running"))
			case e.Data["reason"] == "notice":
				if pending {
					steps = append(steps, step("DoneA", "a", "idle", "view", "main"))
					pending = false
				}
				steps = append(steps,
					step("StartThread", "thread", "running"),
					step("ThreadFinish", "thread", "reported"),
					step("ArchiveThread", "thread", "none"))
			case pending:
				steps = append(steps, step("DoneA", "a", "idle", "view", "main", "main", "live"))
				pending = false
			default:
				steps = append(steps,
					step("Message", "a", "msg-wait"),
					step("EnterA", "a", "msg-held"),
					step("DoneA", "a", "idle", "view", "main", "main", "live"))
			}
		case "done", "cancelled":
			if isThread && open {
				steps = append(steps, step("ThreadFinish", "thread", "reported"))
			}
			open = false
		}
	}
	return steps
}

func init() { historyProjections["project_main_threads"] = projectMainThreadsHistory }

func pmtOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 12, "max-parallel-runs": 0}
}

func TestProjectMainThreads(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPMTAdapter(t)
	if err := runMBT(t, "project_main_threads", a, pmtActions, pmtOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "project_main_threads"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range append(slices.Clone(a.mains), a.kids...) {
		if a.hasHistory(id) {
			checkHistory(t, g, sessionHistory(t, a.s.Home, id), projectMainThreadsHistory)
		}
	}
}

// walkPath drives one generated path through the adapter and compares
// the state it reads off serve with the path's after every step.
func (a *pmtAdapter) walkPath(trace []tracecheck.Step) (err error) {
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
			f, ok := pmtActions["Project"][name]
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

// pmtDiff lists the role's fields where have is not the spec's want.
func pmtDiff(want, have map[string]any) string {
	b, _ := json.Marshal(have)
	var got map[string]any
	json.Unmarshal(b, &got)
	var out []string
	for k, w := range want {
		f, ok := strings.CutPrefix(k, "Project#0.")
		if !ok {
			continue
		}
		if g := got[f]; fmt.Sprint(g) != fmt.Sprint(w) {
			out = append(out, fmt.Sprintf("\n  %s: spec %v, serve %v", f, w, g))
		}
	}
	slices.Sort(out)
	return strings.Join(out, "")
}

// pmtPaths is the generator's paths for the spec, as traces.
func pmtPaths(t *testing.T) [][]tracecheck.Step {
	t.Helper()
	b, err := pathsJSON("project_main_threads")
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

// TestProjectMainThreadsPaths walks every path the generator wrote for
// the spec (testdata/…/paths.json covers every transition) through the
// server adapter. The fizzbee-mbt runner picks each step at random from
// all nineteen actions, enabled or not, and stops checking a walk at its
// first disabled one, so on this spec its walks end within a step or
// two; the generated paths are what reach main's boot race, a thread's
// report and a lost main. Four serves share the paths.
func TestProjectMainThreadsPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	paths := pmtPaths(t)
	g, err := tracecheck.Load(fizzCheck(t, "project_main_threads"))
	if err != nil {
		t.Fatal(err)
	}
	const shards = 4
	for n := range shards {
		t.Run(fmt.Sprintf("serve%d", n), func(t *testing.T) {
			t.Parallel()
			a := newPMTAdapter(t)
			for i := n; i < len(paths); i += shards {
				if err := a.walkPath(paths[i]); err != nil {
					t.Errorf("path %d: %v", i, err)
				}
			}
			checked := 0
			for _, id := range append(slices.Clone(a.mains), a.kids...) {
				if a.hasHistory(id) {
					checkHistory(t, g, sessionHistory(t, a.s.Home, id), projectMainThreadsHistory)
					checked++
				}
			}
			if checked == 0 {
				t.Error("no transcripts for the trace check")
			}
			t.Logf("trace-checked %d transcripts", checked)
		})
	}
}

// A Poll that messages the project is the kind of wiring slip an
// adapter can have (and the bug ReadingNeverCreatesMain is about): main
// starts on a read, and the run must say so. Poll is enabled at Init,
// so the runner's short walks reach it.
func TestProjectMainThreadsCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPMTAdapter(t)
	a.pollSends = true
	if err := runMBT(t, "project_main_threads", a, pmtActions, pmtOptions()); err == nil {
		t.Fatal("a run whose Poll messages the project passed; the runner is not checking state")
	}
}

// The path walk must fail on a wrong adapter too: 'New thread' sending
// a task boots the thread into a turn, not empty. It stops at the first
// path that says so.
func TestProjectMainThreadsPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPMTAdapter(t)
	a.newThreadPrompt = true
	for i, p := range pmtPaths(t) {
		if err := a.walkPath(p); err != nil {
			t.Logf("path %d caught it: %v", i, err)
			return
		}
	}
	t.Fatal("every path passed with a 'New thread' that sends a task; the walk is not checking state")
}
