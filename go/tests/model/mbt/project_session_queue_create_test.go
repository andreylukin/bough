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

// specs/project_session_queue_create.fizz against a real serve: one
// project per walk, its main thread started in Init, one thread a person
// creates over POST /api/sessions {mode: project} (what the palette, the
// overview and the projects page all send) and main's one background
// agent (the POST tools.spawn sends).
//
// Every POST carries the spec's caps (maxRunning 1, maxPerSession 1), as
// callers reading one bough.yml would. llm-control's hold_boot keeps each
// new process before its history file until the adapter releases it:
// that is the spec's "booting" for the thread; the agent is released
// inside the step that starts it, since the spec has it running at once.
// Orbs run on the fake container runtime.
//
// prompted, opened, spawn, verb and restarts are the client's own: what
// it posted, whether the page has the thread open, and what serve last
// answered. Everything else is read off serve: the thread from the list,
// the agent from main's children, the view from GET /api/sessions/{id}
// (404 is 'Session not found'; a queued row is 'queued'; a row with no
// history entries is 'starting'), lastat and the name from the thread's
// psqcListed row.
const psqConfig = "- id: llm\n  plugin: llm-control\n  config:\n    hold_boot: true\n" +
	"- id: orb\n  plugin: orb\n  config:\n    runtime: fake\n"

type psqAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	walk int
	slug string
	main string

	thread, agent         string // ids, "" before the POST
	refused               bool   // the thread's POST answered 429
	prompted, opened      bool
	spawn, verb           string
	restarts              int
	postStart, postEnd    time.Time
	renameTo              string // the name Rename sent, "" before
	renamed               bool   // the last name serve psqcListed matched it
	threadTurn, agentTurn string // held llm-control turns
	turn                  int
	kids                  []string // every thread id, for the trace check

	// stopArchives is the deliberate wiring bug the wrong-adapter test
	// injects: the Work dialog's Stop sends archive instead.
	stopArchives bool
}

func newPSQAdapter(t *testing.T) *psqAdapter {
	s := servetest.Start(t, servetest.Options{Config: psqConfig})
	return &psqAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

// psqCaps are the spec's MAX_RUNNING and MAX_PER_SESSION.
const psqMaxRunning, psqMaxPerSession = 1, 1

// Init starts each walk on a fresh project whose main thread is live and
// idle: its first message boots it, and the turn on it answers from an
// empty queue at once.
func (a *psqAdapter) Init() error {
	a.walk++
	var r struct {
		Project serve.Project `json:"project"`
	}
	if _, err := a.api(http.MethodPost, "/api/projects", map[string]string{"name": fmt.Sprintf("Queue %d", a.walk)}, &r); err != nil {
		return err
	}
	a.slug = r.Project.Slug
	done := make(chan error, 1)
	go func() {
		var out struct {
			Main string `json:"main"`
		}
		_, err := a.api(http.MethodPost, "/api/projects/"+a.slug+"/message", map[string]string{"text": "hello main"}, &out)
		a.main = out.Main
		done <- err
	}()
	var main string
	if err := psqcWaitFor("main held at boot", func() (bool, error) {
		for id, role := range control.Booting(a.dir) {
			if role == "main" {
				main = id
				return true, nil
			}
		}
		return false, nil
	}); err != nil {
		return err
	}
	control.ReleaseBoot(a.t, a.dir, main)
	select {
	case err := <-done:
		if err != nil {
			return err
		}
	case <-time.After(actionTimeout):
		return errors.New("init: message to main did not return")
	}
	// Until main's input is on disk its turn cannot be seen open, and it
	// would take the next turn a step queues.
	if err := a.mainQuiet(func(es []history.Entry) bool { return inputs(es) > 0 }); err != nil {
		return err
	}
	a.thread, a.agent, a.refused = "", "", false
	a.prompted, a.opened = false, false
	a.spawn, a.verb, a.restarts = "", "", 0
	a.renameTo, a.renamed = "", false
	a.threadTurn, a.agentTurn = "", ""
	a.gate.reset()
	return nil
}

// Cleanup lets everything the walk left held or queued go, waits for no
// child to hold a slot, and archives the project (which ends its
// processes), so the next walk starts with the cap free.
func (a *psqAdapter) Cleanup() error {
	var errs []error
	if st, err := a.state(); err == nil && st.thread == "queued" {
		if _, err := a.s.Stop(context.Background(), a.thread); err != nil {
			errs = append(errs, err)
		}
	}
	deadline := time.Now().Add(actionTimeout)
	for {
		for id := range control.Booting(a.dir) {
			control.ReleaseBoot(a.t, a.dir, id)
		}
		for _, name := range []*string{&a.threadTurn, &a.agentTurn} {
			if *name != "" {
				control.Release(a.t, a.dir, *name)
				*name = ""
			}
		}
		busy, err := a.busy()
		if err != nil {
			errs = append(errs, err)
			break
		}
		if !busy {
			break
		}
		if time.Now().After(deadline) {
			errs = append(errs, fmt.Errorf("walk %d: children still busy after %s", a.walk, actionTimeout))
			break
		}
		time.Sleep(50 * time.Millisecond)
	}
	if a.main != "" {
		errs = append(errs, a.mainQuiet(func([]history.Entry) bool { return true }))
	}
	if a.slug != "" {
		_, err := a.api(http.MethodPost, "/api/projects/"+a.slug+"/archive", nil, nil)
		errs = append(errs, err)
	}
	return errors.Join(errs...)
}

// busy says whether any child of main is queued, booting or mid-turn.
func (a *psqAdapter) busy() (bool, error) {
	rows, err := a.children()
	if err != nil {
		return false, err
	}
	for _, r := range rows {
		if r.Queued || r.Status == serve.StatusRunning {
			return true, nil
		}
	}
	return len(control.Booting(a.dir)) > 0, nil
}

func (a *psqAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Queue", Index: 0}: a}, nil
}

// psqState is the role's state as read off serve (and the client).
type psqState struct {
	thread, agent, view, lastat, verb, spawn string
	prompted, opened, renamed                bool
	restarts                                 int
}

func (st psqState) fields() map[string]any {
	return map[string]any{
		"thread": st.thread, "prompted": st.prompted, "agent": st.agent,
		"spawn": st.spawn, "opened": st.opened, "view": st.view,
		"lastat": st.lastat, "renamed": st.renamed, "verb": st.verb,
		"restarts": st.restarts,
	}
}

func (a *psqAdapter) GetState() (map[string]any, error) {
	st, err := a.state()
	if err != nil {
		return nil, err
	}
	return st.fields(), nil
}

func (a *psqAdapter) state() (psqState, error) {
	st := psqState{
		prompted: a.prompted, opened: a.opened, spawn: a.spawn,
		verb: a.verb, restarts: a.restarts, renamed: a.renamed,
	}
	ctx, cancel := actionCtx()
	defer cancel()
	rows, err := a.s.ListSessions(ctx, false)
	if err != nil {
		return st, err
	}
	var row *serve.Row
	for i := range rows {
		if a.thread != "" && rows[i].ID == a.thread {
			row = &rows[i]
		}
	}
	switch {
	case a.refused:
		st.thread = "refused"
	case a.thread == "":
		st.thread = "none"
	case row == nil:
		st.thread = "gone"
	case row.Queued:
		st.thread = "queued"
	case !a.hasHistory(a.thread):
		st.thread = "booting"
	case row.Status == serve.StatusRunning:
		st.thread = "running"
	case inputs(a.entries(a.thread)) == 0:
		st.thread = "empty"
	default:
		st.thread = "idle"
	}
	// LastAt: a pending row's must be the create's, however often it is
	// read; once the file exists it is the file's.
	switch {
	case a.thread == "" && !a.refused:
		st.lastat = ""
	case row == nil:
		st.lastat = "post"
	case a.hasHistory(a.thread):
		st.lastat = "history"
	case !row.LastAt.Before(a.postStart.Truncate(time.Millisecond)) && !row.LastAt.After(a.postEnd):
		st.lastat = "post"
	default:
		st.lastat = "read at " + row.LastAt.Format(time.RFC3339Nano)
	}
	if row != nil && a.renameTo != "" {
		a.renamed = row.Title == a.renameTo
		st.renamed = a.renamed
	}
	st.agent = "none"
	if a.agent != "" {
		kids, err := a.children()
		if err != nil {
			return st, err
		}
		st.agent = "gone"
		for _, k := range kids {
			if k.ID != a.agent {
				continue
			}
			switch {
			case k.Queued:
				st.agent = "queued"
			case k.Status == serve.StatusRunning:
				st.agent = "running"
			default:
				st.agent = "done"
			}
		}
	}
	st.view = "closed"
	if a.opened {
		st.view = "missing"
		if a.thread != "" {
			r, _, err := a.s.GetSession(ctx, a.thread)
			var apiErr *servetest.APIError
			switch {
			case errors.As(err, &apiErr) && apiErr.Status == http.StatusNotFound:
			case err != nil:
				return st, err
			case r.Queued:
				st.view = "queued"
			case r.Entries == 0:
				st.view = "starting"
			default:
				st.view = "transcript"
			}
		}
	}
	return st, nil
}

func (a *psqAdapter) children() ([]serve.Row, error) {
	var r struct {
		Children []serve.Row `json:"children"`
	}
	_, err := a.api(http.MethodGet, "/api/sessions/"+url.PathEscape(a.main)+"/children", nil, &r)
	return r.Children, err
}

func (a *psqAdapter) histPath(id string) string {
	return filepath.Join(a.s.Home, ".bough", "history", id+".jsonl")
}

func (a *psqAdapter) hasHistory(id string) bool {
	_, err := os.Stat(a.histPath(id))
	return err == nil
}

func (a *psqAdapter) entries(id string) []history.Entry {
	es, _ := history.Read(a.histPath(id))
	return es
}

func inputs(es []history.Entry) int {
	n := 0
	for _, e := range es {
		if e.Kind == "input" {
			n++
		}
	}
	return n
}

// api is one JSON call; it returns the status, and an error for any
// answer that is not 2xx.
func (a *psqAdapter) api(method, path string, body, out any) (int, error) {
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
		return 0, fmt.Errorf("%s %s: %w", method, path, err)
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	if resp.StatusCode/100 != 2 {
		return resp.StatusCode, fmt.Errorf("%s %s: %d %s", method, path, resp.StatusCode, raw)
	}
	if out != nil {
		return resp.StatusCode, json.Unmarshal(raw, out)
	}
	return resp.StatusCode, nil
}

// verbWord is the spec's verb for a header verb's answer: ok, 404, or
// refused for any other 4xx (an error that says why).
func verbWord(code int, err error) (string, error) {
	switch {
	case err == nil:
		return "ok", nil
	case code == http.StatusNotFound:
		return "404", nil
	case code/100 == 4:
		return "refused", nil
	}
	return "", err
}

// psqcWaitFor polls cond until it holds, for at most actionTimeout.
func psqcWaitFor(what string, cond func() (bool, error)) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		ok, err := cond()
		if err != nil {
			return fmt.Errorf("waiting for %s: %w", what, err)
		}
		if ok {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for %s: not after %s", what, actionTimeout)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// mainQuiet waits until main's transcript satisfies has and its last
// turn is closed: a turn main takes on a notice must not take a turn the
// next step queues.
func (a *psqAdapter) mainQuiet(has func([]history.Entry) bool) error {
	return psqcWaitFor("main "+a.main+" quiet", func() (bool, error) {
		es, err := history.Read(a.histPath(a.main))
		return err == nil && has(es) && !turnOpen(es), nil
	})
}

// reported waits for main to have taken n reports from child. A main
// with no process (serve restarted under it) takes none: the report is
// kept for its next start, and nothing here starts it.
func (a *psqAdapter) reported(child string, n int) error {
	ctx, cancel := actionCtx()
	defer cancel()
	if row, _, err := a.s.GetSession(ctx, a.main); err == nil && !row.Live {
		return nil
	}
	return a.mainQuiet(func(es []history.Entry) bool {
		got := 0
		for _, e := range es {
			if text, _ := e.Data["text"].(string); e.Kind == "input" && e.Data["reason"] == "notice" && strings.Contains(text, child) {
				got++
			}
		}
		return got >= n
	})
}

func (a *psqAdapter) queueTurn(who string) string {
	a.turn++
	name := fmt.Sprintf("q%04d-%s", a.turn, who)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	return name
}

func (a *psqAdapter) taken(name string) error {
	return psqcWaitFor("turn "+name+" taken", func() (bool, error) {
		_, err := os.Stat(filepath.Join(a.dir, name+".taken"))
		return err == nil, nil
	})
}

func (a *psqAdapter) heldAtBoot(id string) error {
	return psqcWaitFor(id+" held at boot", func() (bool, error) {
		_, ok := control.Booting(a.dir)[id]
		return ok, nil
	})
}

// startAgent takes the agent from its held boot into its running turn.
func (a *psqAdapter) startAgent() error {
	if err := a.heldAtBoot(a.agent); err != nil {
		return err
	}
	name := a.queueTurn("agent")
	control.ReleaseBoot(a.t, a.dir, a.agent)
	if err := a.taken(name); err != nil {
		return err
	}
	a.agentTurn = name
	return a.waitState("the agent running", func(st psqState) bool { return st.agent == "running" })
}

func (a *psqAdapter) waitState(what string, ok func(psqState) bool) error {
	return psqcWaitFor(what, func() (bool, error) {
		st, err := a.state()
		return err == nil && ok(st), err
	})
}

func (a *psqAdapter) now() psqState {
	st, err := a.state()
	if err != nil {
		a.t.Logf("walk %d: reading state: %v", a.walk, err)
	}
	return st
}

func pending(thread string) bool { return thread == "queued" || thread == "booting" }

func psqcListed(thread string) bool {
	return slices.Contains([]string{"queued", "booting", "empty", "running", "idle"}, thread)
}

// --- actions

func (a *psqAdapter) post(prompted bool) error {
	if !a.gate.pass(a.now().thread == "none") {
		return nil
	}
	prompt := ""
	if prompted {
		prompt = fmt.Sprintf("task of walk %d", a.walk)
	}
	var out struct {
		Session serve.Row `json:"session"`
		Queued  bool      `json:"queued"`
	}
	a.postStart = time.Now()
	code, err := a.api(http.MethodPost, "/api/sessions", map[string]any{
		"mode": "project", "project": a.slug, "prompt": prompt,
		"maxRunning": psqMaxRunning, "maxPerSession": psqMaxPerSession,
	}, &out)
	a.postEnd = time.Now()
	// start() opens the new id at once.
	a.prompted, a.opened = prompted, true
	if code == http.StatusTooManyRequests {
		a.refused = true
		return nil
	}
	if err != nil {
		return err
	}
	a.thread = out.Session.ID
	a.kids = append(a.kids, a.thread)
	if out.Queued {
		return nil
	}
	return a.heldAtBoot(a.thread)
}

func (a *psqAdapter) PostPrompt() error { return a.post(true) }
func (a *psqAdapter) PostEmpty() error  { return a.post(false) }

func (a *psqAdapter) AgentSpawn() error {
	st := a.now()
	if !a.gate.pass(st.agent != "done" && a.spawn == "") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, queued, err := a.s.CreateAgent(ctx, a.main, fmt.Sprintf("agent task of walk %d", a.walk), psqMaxRunning, psqMaxPerSession)
	var apiErr *servetest.APIError
	if errors.As(err, &apiErr) && apiErr.Status == http.StatusTooManyRequests {
		a.spawn = "429"
		return nil
	}
	if err != nil {
		return err
	}
	a.agent = row.ID
	if queued {
		return nil
	}
	return a.startAgent()
}

func (a *psqAdapter) AgentFinish() error {
	st := a.now()
	if !a.gate.pass(st.agent == "running") {
		return nil
	}
	control.Release(a.t, a.dir, a.agentTurn)
	a.agentTurn = ""
	if err := a.waitState("the agent done", func(st psqState) bool { return st.agent == "done" }); err != nil {
		return err
	}
	a.spawn = ""
	if err := a.reported(a.agent, 1); err != nil {
		return err
	}
	if st.thread == "queued" {
		return a.heldAtBoot(a.thread)
	}
	return nil
}

func (a *psqAdapter) HistoryWritten() error {
	st := a.now()
	if !a.gate.pass(st.thread == "booting") {
		return nil
	}
	name := ""
	if a.prompted {
		name = a.queueTurn("thread")
	}
	control.ReleaseBoot(a.t, a.dir, a.thread)
	if err := psqcWaitFor("the thread's history", func() (bool, error) { return a.hasHistory(a.thread), nil }); err != nil {
		return err
	}
	if a.prompted {
		if err := a.taken(name); err != nil {
			return err
		}
		a.threadTurn = name
		return a.waitState("the thread running", func(st psqState) bool { return st.thread == "running" })
	}
	// launch writes a prompt, when there is one, right after the file
	// appears: give it the time to show, so "empty" is a fact.
	settle()
	if st.agent == "queued" {
		return a.startAgent()
	}
	return nil
}

func (a *psqAdapter) Finish() error {
	st := a.now()
	if !a.gate.pass(st.thread == "running") {
		return nil
	}
	control.Release(a.t, a.dir, a.threadTurn)
	a.threadTurn = ""
	if err := a.waitState("the thread's turn closed", func(st psqState) bool { return st.thread == "idle" }); err != nil {
		return err
	}
	if err := a.reported(a.thread, closedTurns(a.entries(a.thread))); err != nil {
		return err
	}
	if st.agent == "queued" {
		return a.startAgent()
	}
	return nil
}

func (a *psqAdapter) Poll() error {
	if !a.gate.pass(pending(a.now().thread)) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.ListSessions(ctx, false)
	return err
}

// StartingWindowExpires is the page's clock: serve sees nothing.
func (a *psqAdapter) StartingWindowExpires() error {
	a.gate.pass(a.opened && pending(a.now().thread))
	return nil
}

func (a *psqAdapter) Leave() error {
	if a.gate.pass(a.opened) {
		a.opened = false
	}
	return nil
}

func (a *psqAdapter) Open() error {
	if a.gate.pass(!a.opened && psqcListed(a.now().thread)) {
		a.opened = true
	}
	return nil
}

func (a *psqAdapter) Rename() error {
	if !a.gate.pass(pending(a.now().thread) && !a.renamed) {
		return nil
	}
	a.renameTo = fmt.Sprintf("named in walk %d", a.walk)
	code, err := a.api(http.MethodPost, "/api/sessions/"+url.PathEscape(a.thread)+"/rename", map[string]string{"title": a.renameTo}, nil)
	a.verb, err = verbWord(code, err)
	return err
}

func (a *psqAdapter) Archive() error {
	if !a.gate.pass(pending(a.now().thread)) {
		return nil
	}
	code, err := a.api(http.MethodPost, "/api/sessions/"+url.PathEscape(a.thread)+"/archive", nil, nil)
	a.verb, err = verbWord(code, err)
	return err
}

func (a *psqAdapter) Stop() error {
	if !a.gate.pass(a.now().thread == "queued") {
		return nil
	}
	path := "/api/sessions/" + url.PathEscape(a.thread) + "/stop"
	if a.stopArchives {
		path = "/api/sessions/" + url.PathEscape(a.thread) + "/archive"
	}
	code, err := a.api(http.MethodPost, path, map[string]string{"parent": a.main}, nil)
	a.verb, err = verbWord(code, err)
	return err
}

// ServeRestart stops serve the way launchd does and starts it again on
// the same HOME. Every child dies with it; what was queued or booting
// must boot again, and is held at boot again.
func (a *psqAdapter) ServeRestart() error {
	st := a.now()
	if !a.gate.pass(a.restarts == 0 && st.agent != "queued") {
		return nil
	}
	a.s.Shutdown()
	if err := a.s.Resume(""); err != nil {
		return err
	}
	a.restarts = 1
	a.threadTurn, a.agentTurn = "", ""
	if st.agent == "running" {
		a.spawn = ""
	}
	if pending(st.thread) {
		if err := a.heldAtBoot(a.thread); err != nil {
			return err
		}
	}
	settle()
	return a.mainQuiet(func([]history.Entry) bool { return true })
}

// psqAction logs each step a walk takes with the gate open.
func psqAction(name string, f func(*psqAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*psqAdapter)
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

var psqActions = map[string]map[string]fmbt.ActionFunc{"Queue": {
	"PostPrompt":            psqAction("PostPrompt", (*psqAdapter).PostPrompt),
	"PostEmpty":             psqAction("PostEmpty", (*psqAdapter).PostEmpty),
	"AgentSpawn":            psqAction("AgentSpawn", (*psqAdapter).AgentSpawn),
	"AgentFinish":           psqAction("AgentFinish", (*psqAdapter).AgentFinish),
	"HistoryWritten":        psqAction("HistoryWritten", (*psqAdapter).HistoryWritten),
	"Finish":                psqAction("Finish", (*psqAdapter).Finish),
	"Poll":                  psqAction("Poll", (*psqAdapter).Poll),
	"StartingWindowExpires": psqAction("StartingWindowExpires", (*psqAdapter).StartingWindowExpires),
	"Leave":                 psqAction("Leave", (*psqAdapter).Leave),
	"Open":                  psqAction("Open", (*psqAdapter).Open),
	"Rename":                psqAction("Rename", (*psqAdapter).Rename),
	"Archive":               psqAction("Archive", (*psqAdapter).Archive),
	"Stop":                  psqAction("Stop", (*psqAdapter).Stop),
	"ServeRestart":          psqAction("ServeRestart", (*psqAdapter).ServeRestart),
}}

// projectSessionQueueCreateHistory reads a thread's transcript as a path
// through the spec. The transcript sees neither the queue nor the page,
// so the steps it cannot see are taken the one way that fits: it was
// posted with nothing ahead of it (booting at once) and its file then
// appeared. A first input is the prompt it was posted with; the close
// of that turn is Finish; a turn left open (the process died with serve)
// closes by ServeRestart. A thread has no later inputs in this flow.
func projectSessionQueueCreateHistory(entries []history.Entry) []tracecheck.Step {
	st := func(kv ...any) map[string]any {
		m := map[string]any{}
		for i := 0; i+1 < len(kv); i += 2 {
			m["Queue#0."+kv[i].(string)] = kv[i+1]
		}
		return m
	}
	step := func(action string, kv ...any) tracecheck.Step {
		return tracecheck.Step{Action: "Queue#0." + action, State: st(kv...)}
	}
	steps := []tracecheck.Step{{Action: "Init", State: st("thread", "none")}}
	prompted := inputs(entries) > 0
	if prompted {
		steps = append(steps,
			step("PostPrompt", "thread", "booting", "prompted", true, "lastat", "post"),
			step("HistoryWritten", "thread", "running", "lastat", "history"))
	} else {
		steps = append(steps,
			step("PostEmpty", "thread", "booting", "prompted", false, "lastat", "post"),
			step("HistoryWritten", "thread", "empty", "lastat", "history"))
	}
	open := false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			open = true
		case "done", "cancelled":
			if open {
				steps = append(steps, step("Finish", "thread", "idle"))
			}
			open = false
		}
	}
	if open {
		steps = append(steps, step("ServeRestart", "thread", "idle", "restarts", 1))
	}
	return steps
}

func init() { historyProjections["project_session_queue_create"] = projectSessionQueueCreateHistory }

// walkPath drives one path through the adapter and compares the state
// it reads off serve with the path's after every step.
func (a *psqAdapter) walkPath(trace []tracecheck.Step) (err error) {
	if err := a.Init(); err != nil {
		return err
	}
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	for i, s := range trace {
		name := strings.TrimPrefix(s.Action, "Queue#0.")
		// fizz links a state with nothing enabled (the restart used, the
		// agent done, the thread settled and closed) to itself as "end":
		// nothing happens, and the state must still be the spec's.
		if i > 0 && name != "end" {
			f, ok := psqActions["Queue"][name]
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
		if diff := psqDiff(s.State, have); diff != "" {
			var walked []string
			for _, p := range trace[1 : i+1] {
				walked = append(walked, strings.TrimPrefix(p.Action, "Queue#0."))
			}
			return fmt.Errorf("step %d (%s) of %v: state differs from the spec's:%s", i, name, walked, diff)
		}
	}
	return nil
}

func psqDiff(want, have map[string]any) string {
	var out []string
	for k, w := range want {
		f, ok := strings.CutPrefix(k, "Queue#0.")
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

func psqPaths(t *testing.T, cover tracecheck.Cover) [][]tracecheck.Step {
	t.Helper()
	b, err := pathsJSONCover("project_session_queue_create", cover)
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

// TestProjectSessionQueueCreatePaths walks the spec's graph (every
// settled state, or every transition under MODEL_COVER=transitions)
// through the adapter on four serves, then replays every thread's
// transcript on the graph.
func TestProjectSessionQueueCreatePaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	paths := psqPaths(t, envCover())
	g, err := tracecheck.Load(fizzCheck(t, "project_session_queue_create"))
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("%d walks", len(paths))
	const shards = 4
	for n := range shards {
		t.Run(fmt.Sprintf("serve%d", n), func(t *testing.T) {
			t.Parallel()
			a := newPSQAdapter(t)
			for i := n; i < len(paths); i += shards {
				if err := a.walkPath(paths[i]); err != nil {
					t.Errorf("path %d: %v", i, err)
				}
			}
			checked := 0
			for _, id := range a.kids {
				if a.hasHistory(id) {
					checkHistory(t, g, sessionHistory(t, a.s.Home, id), projectSessionQueueCreateHistory)
					checked++
				}
			}
			if checked == 0 {
				t.Error("no thread transcripts for the trace check")
			}
			t.Logf("trace-checked %d transcripts", checked)
		})
	}
}

// A Stop that archives instead is the wiring slip the Work dialog could
// have: the queued thread stays queued and the answer is a refusal. It
// shows on the Stop transition, so the walk takes every transition and
// stops at the first path that says so.
func TestProjectSessionQueueCreatePathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPSQAdapter(t)
	a.stopArchives = true
	for i, p := range psqPaths(t, tracecheck.CoverTransitions) {
		if !slices.ContainsFunc(p, func(s tracecheck.Step) bool { return s.Action == "Queue#0.Stop" }) {
			continue
		}
		if err := a.walkPath(p); err != nil {
			t.Logf("path %d caught it: %v", i, err)
			return
		}
	}
	t.Fatal("every path passed with a Stop that archives; the walk is not checking state")
}
