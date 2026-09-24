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
	"net/http/httptest"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/archive_parent_with_children.fizz against serve: a project's
// main thread (the parent), its two background agents a and b, and a
// person's thread that lands while Stop and archive is ending the
// children it listed.
//
// serve runs in process, the Supervisor and API `bough serve` builds
// behind an httptest server, for the orb: the spec's a has a container
// whose stop can fail, and a `bough serve` process only ever asks its
// own runtime, which a test cannot reach. Here serve's runtime is a
// container.Fake the adapter holds (apcRT): BootA starts a's container
// in it, EndAOrbFails makes its stops fail, and the orb field is read
// off it. Every session is a real bough child on llm-control, whose own
// orb row runs the fake runtime too (its container is its own).
//
// The request's steps (SetArchived, EndA, EndB) are windows serve
// crosses in microseconds, so serve's test-only hold (Options.HoldDir)
// parks Stop and archive before it ends each child: the adapter makes
// archive-end-<a> and archive-end-<b> before it sends the request and
// takes them away one step at a time. The request's answer is the
// page's: listed and done change when the adapter collects it at the
// step the spec answers in, even when serve answered earlier because
// nothing was left to hold.
//
// hold_boot keeps each fresh child before its history file: the spec's
// booting for a and b (BootA, BootB release it); a thread the spec
// starts at once is released by the adapter in the same step. Turns
// are llm-control "block" turns released by Finish*. A report to a live
// parent wakes a turn, which takes whatever is queued, so a turn is
// queued only once every report of the step has landed.
const apcConfig = "- id: llm\n  plugin: llm-control\n  config:\n    hold_boot: true\n" +
	"- id: orb\n  plugin: orb\n  config:\n    runtime: fake\n"

// apcRT is serve's runtime: the fake, with stops that fail on demand.
type apcRT struct {
	*container.Fake
	mu   sync.Mutex
	fail map[string]bool
}

func (r *apcRT) failStop(name string, on bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.fail[name] = on
}

func (r *apcRT) Stop(ctx context.Context, name string) error {
	r.mu.Lock()
	fail := r.fail[name]
	r.mu.Unlock()
	if fail {
		return fmt.Errorf("container: fake: stop %s: timed out", name)
	}
	return r.Fake.Stop(ctx, name)
}

// apcState is the Parent role as the adapter reads it.
type apcState struct {
	live, archived, listed, stop, unread bool
	tsnap                                bool
	dialog, req, done                    string
	a, orb, b, thread                    string
	agents                               serve.AgentCount
}

func (s apcState) fields() map[string]any {
	return map[string]any{
		"live": s.live, "archived": s.archived, "listed": s.listed, "dialog": s.dialog,
		"req": s.req, "stop": s.stop, "done": s.done, "a": s.a, "orb": s.orb,
		"b": s.b, "thread": s.thread, "tsnap": s.tsnap, "unread": s.unread,
	}
}

type apcResp struct {
	status int
	body   string
	err    error
}

type apcAdapter struct {
	t     *testing.T
	home  string
	hist  string
	dir   string // llm-control's queue
	holds string // serve's HoldDir
	rt    *apcRT
	sup   *serve.Supervisor
	api   *serve.API
	srv   *httptest.Server
	gate  gate

	walk, turn   int
	slug, parent string
	a, b, thread string // ids, "" before the spawn
	// Held turns by llm-control name, "" when none.
	aTurn, bTurn, tTurn string

	// The page's side: the spec's listed, dialog, req, stop and done,
	// and tsnap, whether the request in flight listed the thread (it
	// made the thread's hold).
	listed            bool
	dialog, req, done string
	stop, tsnap       bool
	inflight          chan apcResp // the archive request, nil when none
	answer            *apcResp     // its answer, once collected
	aIDs              []string     // every a, for the trace check
	ran               map[string]int

	// endAReleasesB is TestArchiveParentWithChildrenCatchesWrongAdapter's
	// bug: EndA lifts b's hold too, so the loop ends b in the same step.
	endAReleasesB bool
}

func newAPCAdapter(t *testing.T) *apcAdapter {
	// A short root under the system temp dir, as servetest's: sessions
	// open unix sockets under HOME, capped at 104 bytes on macOS.
	root, err := os.MkdirTemp("", "bapc-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(root) })
	root, _ = filepath.EvalSymlinks(root)
	a := &apcAdapter{
		t: t, home: filepath.Join(root, "home"), holds: filepath.Join(root, "holds"),
		rt: &apcRT{Fake: container.NewFake(), fail: map[string]bool{}}, ran: map[string]int{},
	}
	a.hist = filepath.Join(a.home, ".bough", "history")
	a.dir = control.Dir(a.home)
	for _, d := range []string{a.hist, a.holds, a.dir} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.WriteFile(filepath.Join(a.home, ".bough", "bough.yml"), []byte(apcConfig), 0o644); err != nil {
		t.Fatal(err)
	}
	a.rt.AddImage("img")
	a.sup, err = serve.NewSupervisor(serve.Options{
		Exe:      servetest.Binary(t),
		HistDir:  a.hist,
		MetaPath: filepath.Join(a.home, ".bough", "serve", "meta.json"),
		Home:     a.home,
		Env:      apcEnv(a.home),
		Runtime:  a.rt,
		HoldDir:  a.holds,
	})
	if err != nil {
		t.Fatal(err)
	}
	a.api = serve.NewAPI(a.sup)
	a.srv = httptest.NewServer(a.api)
	t.Cleanup(func() {
		// Nothing may stay parked when serve closes.
		a.clearHolds()
		a.srv.Close()
		a.sup.Close()
	})
	return a
}

// apcEnv is the children's environment the way servetest builds a
// serve's: HOME moved, provider keys dropped, the page server off the
// person's port.
func apcEnv(home string) []string {
	var env []string
	for _, kv := range os.Environ() {
		k, _, _ := strings.Cut(kv, "=")
		if k == "HOME" || k == "BOUGH_WEB_ADDR" || k == "BOUGH_BIN" || strings.HasSuffix(k, "_API_KEY") || strings.HasSuffix(k, "_TOKEN") {
			continue
		}
		env = append(env, kv)
	}
	return append(env, "HOME="+home, "BOUGH_WEB_ADDR=127.0.0.1:0")
}

// call is one API request; a non-2xx is the status, not an error.
func (a *apcAdapter) call(method, path string, body, out any) (int, string, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	var rd io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return 0, "", err
		}
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, a.srv.URL+path, rd)
	if err != nil {
		return 0, "", err
	}
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, "", err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return resp.StatusCode, "", err
	}
	if resp.StatusCode/100 == 2 && out != nil {
		if err := json.Unmarshal(raw, out); err != nil {
			return resp.StatusCode, string(raw), fmt.Errorf("%s %s: decode: %w", method, path, err)
		}
	}
	return resp.StatusCode, string(raw), nil
}

// ok is call with anything but 2xx an error.
func (a *apcAdapter) ok(method, path string, body, out any) error {
	status, raw, err := a.call(method, path, body, out)
	if err == nil && status/100 != 2 {
		err = fmt.Errorf("%s %s: %d %s", method, path, status, raw)
	}
	return err
}

func (a *apcAdapter) next(prefix string) string {
	a.turn++
	return fmt.Sprintf("%s%05d", prefix, a.turn)
}

// apcWait polls cond until it holds or actionTimeout passes; cond
// says what it last saw.
func apcWait(what string, cond func() (bool, string)) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		ok, last := cond()
		if ok {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for %s: %s", what, last)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// Init makes a fresh project and its main thread, with one turn
// answered (main exists only once it has been talked to), then ends
// main's process the way the page would (archive, unarchive): the spec
// starts from a parent with no process and no agents.
func (a *apcAdapter) Init() error {
	a.walk++
	var p struct {
		Project serve.Project `json:"project"`
	}
	if err := a.ok(http.MethodPost, "/api/projects", map[string]string{"name": fmt.Sprintf("Walk %d", a.walk)}, &p); err != nil {
		return err
	}
	a.slug = p.Project.Slug
	name := a.next("i")
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "ok", Text: "hello back"})
	done := make(chan error, 1)
	var r struct {
		Main string `json:"main"`
	}
	go func() {
		done <- a.ok(http.MethodPost, "/api/projects/"+a.slug+"/message", map[string]string{"text": "hello " + name}, &r)
	}()
	if err := apcWait("main held at boot", func() (bool, string) {
		for id, role := range control.Booting(a.dir) {
			if role == "main" {
				control.ReleaseBoot(a.t, a.dir, id)
				return true, ""
			}
		}
		return false, fmt.Sprint(control.Booting(a.dir))
	}); err != nil {
		return err
	}
	if err := <-done; err != nil {
		return err
	}
	a.parent = r.Main
	if err := a.waitAnswered(a.parent, "hello "+name); err != nil {
		return err
	}
	if err := a.ok(http.MethodPost, "/api/sessions/"+a.parent+"/archive", nil, nil); err != nil {
		return err
	}
	if err := a.ok(http.MethodPost, "/api/sessions/"+a.parent+"/unarchive", nil, nil); err != nil {
		return err
	}
	a.a, a.b, a.thread = "", "", ""
	a.aTurn, a.bTurn, a.tTurn = "", "", ""
	a.listed, a.dialog, a.req, a.done, a.stop, a.tsnap = true, "none", "none", "none", false, false
	a.inflight, a.answer = nil, nil
	a.gate.reset()
	return nil
}

// Cleanup lets everything the walk left parked go on and ends every
// process it started, so the next walk has every slot and an empty
// queue.
func (a *apcAdapter) Cleanup() error {
	var errs []error
	a.clearHolds()
	a.rt.failStop(container.OrbName(a.a), false)
	if a.inflight != nil {
		if _, err := a.collect(); err != nil {
			errs = append(errs, err)
		}
	}
	for id := range control.Booting(a.dir) {
		control.ReleaseBoot(a.t, a.dir, id)
	}
	for _, name := range []string{a.aTurn, a.bTurn, a.tTurn} {
		if name != "" && os.Remove(filepath.Join(a.dir, name+".json")) != nil {
			control.Release(a.t, a.dir, name)
		}
	}
	a.aTurn, a.bTurn, a.tTurn = "", "", ""
	if a.parent != "" {
		if err := a.ok(http.MethodPost, "/api/sessions/"+a.parent+"/archive", map[string]bool{"stopChildren": true}, nil); err != nil {
			errs = append(errs, err)
		}
		if err := apcWait("the walk's processes to end", func() (bool, string) {
			st, err := a.state()
			if err != nil {
				return false, err.Error()
			}
			for _, id := range []string{a.parent, a.a, a.b, a.thread} {
				if id != "" && a.sup.Live(id) {
					return false, id + " is live"
				}
			}
			return st.agents.Running == 0 && st.agents.Queued == 0, fmt.Sprintf("agents %+v", st.agents)
		}); err != nil {
			errs = append(errs, err)
		}
	}
	return errors.Join(errs...)
}

func (a *apcAdapter) clearHolds() {
	ents, _ := os.ReadDir(a.holds)
	for _, e := range ents {
		if !strings.HasSuffix(e.Name(), ".at") {
			os.Remove(filepath.Join(a.holds, e.Name()))
		}
	}
}

func (a *apcAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Parent", Index: 0}: a}, nil
}

func (a *apcAdapter) GetState() (map[string]any, error) {
	st, err := a.state()
	if err != nil {
		return nil, err
	}
	return st.fields(), nil
}

// state reads the role: live and archived off the parent's row, a, b
// and thread off its children listing (booting is a child parked at
// hold_boot), orb off serve's runtime, unread off the parent's file (a
// stored notice not yet marked delivered). listed, dialog, req, stop
// and done are the page's own.
func (a *apcAdapter) state() (apcState, error) {
	st := apcState{listed: a.listed, dialog: a.dialog, req: a.req, stop: a.stop, done: a.done, tsnap: a.tsnap}
	var r struct {
		Session serve.Row `json:"session"`
	}
	if err := a.ok(http.MethodGet, "/api/sessions/"+a.parent+"?limit=1", nil, &r); err != nil {
		return st, err
	}
	st.live, st.archived = r.Session.Live, r.Session.Archived
	if r.Session.Agents != nil {
		st.agents = *r.Session.Agents
	}
	var c struct {
		Children []serve.Row `json:"children"`
	}
	if err := a.ok(http.MethodGet, "/api/sessions/"+a.parent+"/children", nil, &c); err != nil {
		return st, err
	}
	booting := control.Booting(a.dir)
	child := func(id string) string {
		if id == "" {
			return "none"
		}
		i := slices.IndexFunc(c.Children, func(r serve.Row) bool { return r.ID == id })
		switch {
		case i < 0:
			return "none"
		case c.Children[i].Queued:
			return "queued"
		case !c.Children[i].Live:
			return "ended"
		}
		if _, held := booting[id]; held {
			return "booting"
		}
		if c.Children[i].Status == serve.StatusRunning {
			return "running"
		}
		return "idle"
	}
	st.a, st.b, st.thread = child(a.a), child(a.b), child(a.thread)
	st.orb = "none"
	if a.a != "" {
		ctx, cancel := actionCtx()
		cs, err := a.rt.Inspect(ctx, container.OrbName(a.a))
		cancel()
		if err != nil {
			return st, err
		}
		switch {
		case cs == container.StateRunning && st.a == "ended":
			st.orb = "leaked"
		case cs == container.StateRunning:
			st.orb = "up"
		case cs == container.StateStopped:
			st.orb = "down"
		}
	}
	st.unread = a.unread()
	return st, nil
}

func (a *apcAdapter) entries(id string) []history.Entry {
	entries, _ := history.ReadFile(filepath.Join(a.hist, id+".jsonl"))
	return entries
}

// unread: a notice stored in the parent's file that its loop has not
// marked delivered.
func (a *apcAdapter) unread() bool {
	entries := a.entries(a.parent)
	delivered := map[any]bool{}
	for _, e := range entries {
		if e.Kind == "notice-delivered" {
			delivered[e.Data["id"]] = true
		}
	}
	for _, e := range entries {
		if e.Kind == "notice" && e.Data["to"] == a.parent && e.Data["id"] != nil && !delivered[e.Data["id"]] {
			return true
		}
	}
	return false
}

// enabled reads the spec's require off the adapter's view.
func (a *apcAdapter) enabled(require func(apcState) bool) (apcState, bool) {
	if a.gate.off {
		return apcState{}, false
	}
	st, err := a.state()
	if err != nil {
		a.gate.off = true
		return st, false
	}
	return st, a.gate.pass(require(st))
}

// waitAnswered waits until id's input containing text has been
// answered and the session is idle with its process up.
func (a *apcAdapter) waitAnswered(id, text string) error {
	return apcWait(fmt.Sprintf("%q to be answered in %s", text, id), func() (bool, string) {
		in, closed := -1, false
		for i, e := range a.entries(id) {
			t, _ := e.Data["text"].(string)
			if e.Kind == "input" && strings.Contains(t, text) {
				in = i
			}
			if in >= 0 && i > in && (e.Kind == "done" || e.Kind == "cancelled") {
				closed = true
			}
		}
		return in >= 0 && closed && a.idle(id) && a.sup.Live(id), fmt.Sprintf("input %v closed %v", in >= 0, closed)
	})
}

// idle: id's row does not say running.
func (a *apcAdapter) idle(id string) bool {
	var r struct {
		Session serve.Row `json:"session"`
	}
	if a.ok(http.MethodGet, "/api/sessions/"+id+"?limit=1", nil, &r) != nil {
		return false
	}
	return r.Session.Status != serve.StatusRunning
}

// quiet waits for the parent to settle: every stored notice delivered
// when it is live, and no turn open for a few polls in a row (a
// delivered notice queues its wake turn a moment after the mark).
func (a *apcAdapter) quiet() error {
	calm := 0
	return apcWait("the parent to settle", func() (bool, string) {
		live := a.sup.Live(a.parent)
		if live && (a.unread() || !a.idle(a.parent)) {
			calm = 0
			return false, "a notice or a turn in flight"
		}
		calm++
		return calm >= 5, ""
	})
}

// reported waits for child's report in the parent's file (stored, or
// the input a live parent woke on), then for the parent to settle.
func (a *apcAdapter) reported(child string) error {
	if err := apcWait("the report of "+child, func() (bool, string) {
		for _, e := range a.entries(a.parent) {
			t, _ := e.Data["text"].(string)
			if m := agentReport.FindStringSubmatch(t); m != nil && m[1] == child {
				return true, ""
			}
		}
		return false, "none in the parent's file"
	}); err != nil {
		return err
	}
	return a.quiet()
}

// settle waits for serve's own follow-ups of a step: a freed slot
// drained (a queued child is started, and a started one reaches the
// boot hold), then, with start, starts a drained thread's turn, which
// the spec does in the same step.
func (a *apcAdapter) settle(start bool) error {
	if err := apcWait("the queue to drain", func() (bool, string) {
		st, err := a.state()
		if err != nil {
			return false, err.Error()
		}
		if st.agents.Queued > 0 && st.agents.Running == 0 {
			return false, fmt.Sprintf("agents %+v", st.agents)
		}
		booting := control.Booting(a.dir)
		for _, id := range []string{a.a, a.b, a.thread} {
			if _, held := booting[id]; id == "" || held || !a.sup.Live(id) {
				continue
			}
			if _, err := os.Stat(filepath.Join(a.hist, id+".jsonl")); err != nil {
				return false, id + " is starting"
			}
		}
		return true, ""
	}); err != nil {
		return err
	}
	if _, held := control.Booting(a.dir)[a.thread]; start && a.thread != "" && held {
		return a.startThread()
	}
	return nil
}

// boot releases a child held at boot onto a held turn and waits for
// the turn to be taken and the row to say running.
func (a *apcAdapter) boot(id, prefix string) (string, error) {
	name := a.next(prefix)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	control.ReleaseBoot(a.t, a.dir, id)
	if err := apcWait("turn "+name+" taken", func() (bool, string) {
		_, err := os.Stat(filepath.Join(a.dir, name+".taken"))
		return err == nil, "not taken"
	}); err != nil {
		return name, err
	}
	return name, a.waitChild(id, "running", func(s string) bool { return s == "running" })
}

func (a *apcAdapter) startThread() error {
	name, err := a.boot(a.thread, "t")
	a.tTurn = name
	return err
}

func (a *apcAdapter) waitChild(id, what string, ok func(string) bool) error {
	return apcWait(what+" of "+id, func() (bool, string) {
		st, err := a.state()
		if err != nil {
			return false, err.Error()
		}
		s := map[string]string{a.a: st.a, a.b: st.b, a.thread: st.thread}[id]
		return ok(s), s
	})
}

// spawn is the POST tools.spawn({background}) sends for the parent.
func (a *apcAdapter) spawn(prompt string) (int, string, bool, error) {
	var r struct {
		Session serve.Row `json:"session"`
		Queued  bool      `json:"queued"`
	}
	status, _, err := a.call(http.MethodPost, "/api/sessions", map[string]any{
		"prompt": prompt, "spawnedBy": a.parent, "maxRunning": 1,
	}, &r)
	return status, r.Session.ID, r.Queued, err
}

func (a *apcAdapter) Prompt() error {
	if _, ok := a.enabled(func(s apcState) bool { return !s.archived && s.dialog == "none" && s.req == "none" }); !ok {
		return nil
	}
	name := a.next("p")
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "ok", Text: "answered " + name})
	if err := a.ok(http.MethodPost, "/api/sessions/"+a.parent+"/prompt", map[string]string{"text": "prompt " + name}, nil); err != nil {
		return err
	}
	if err := a.waitAnswered(a.parent, "prompt "+name); err != nil {
		return err
	}
	// A delivered notice's wake can take the queued turn first.
	os.Remove(filepath.Join(a.dir, name+".json"))
	return a.quiet()
}

func (a *apcAdapter) SpawnA() error {
	if _, ok := a.enabled(func(s apcState) bool { return s.live && s.a == "none" && s.b == "none" }); !ok {
		return nil
	}
	status, id, queued, err := a.spawn("task a")
	if err != nil {
		return err
	}
	if status != http.StatusCreated || queued {
		return fmt.Errorf("spawn of a answered %d queued %v, want 201 started", status, queued)
	}
	a.a = id
	a.aIDs = append(a.aIDs, id)
	return a.waitChild(id, "booting", func(s string) bool { return s == "booting" })
}

func (a *apcAdapter) SpawnB() error {
	if _, ok := a.enabled(func(s apcState) bool {
		return s.live && s.b == "none" && (s.a == "booting" || s.a == "running")
	}); !ok {
		return nil
	}
	status, id, queued, err := a.spawn("task b")
	if err != nil {
		return err
	}
	if status != http.StatusCreated || !queued {
		return fmt.Errorf("spawn of b answered %d queued %v, want 201 queued", status, queued)
	}
	a.b = id
	return nil
}

// SpawnRefused is a tools.spawn the parent sent before the kill that
// serve reads after the flag.
func (a *apcAdapter) SpawnRefused() error {
	if _, ok := a.enabled(func(s apcState) bool { return s.archived && s.req != "none" }); !ok {
		return nil
	}
	status, id, _, err := a.spawn("too late")
	if err != nil {
		return err
	}
	if status != http.StatusConflict {
		return fmt.Errorf("spawn from an archived parent answered %d (%s), want 409", status, id)
	}
	return nil
}

// ThreadLate is the project page's 'New thread' with a task, sent while
// the request is parked in its loop. The running cap is the spec's 1.
func (a *apcAdapter) ThreadLate() error {
	if _, ok := a.enabled(func(s apcState) bool {
		return (s.req == "loop_a" || s.req == "loop_b") && s.thread == "none" && s.a != "none"
	}); !ok {
		return nil
	}
	var r struct {
		Session serve.Row `json:"session"`
		Queued  bool      `json:"queued"`
	}
	if err := a.ok(http.MethodPost, "/api/sessions", map[string]any{
		"mode": "project", "project": a.slug, "prompt": "thread task", "maxRunning": 1,
	}, &r); err != nil {
		return err
	}
	a.thread = r.Session.ID
	if r.Queued {
		return nil
	}
	if err := a.waitChild(a.thread, "booting", func(s string) bool { return s == "booting" }); err != nil {
		return err
	}
	return a.startThread()
}

// BootA lets a's process on to its turn; its container comes up in
// serve's runtime with it.
func (a *apcAdapter) BootA() error {
	if _, ok := a.enabled(func(s apcState) bool { return s.a == "booting" }); !ok {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.rt.Start(ctx, container.RunSpec{Name: container.OrbName(a.a), Image: "img"}); err != nil {
		return err
	}
	name, err := a.boot(a.a, "a")
	a.aTurn = name
	if err != nil {
		return err
	}
	// The reaper finds a container by its session's state.json, which
	// a's own orb row writes once it has started.
	return apcWait("a's state.json", func() (bool, string) {
		st, err := orb.ReadState(a.home, a.a)
		return err == nil && st.Status == orb.StatusRunning, fmt.Sprint(st.Status, err)
	})
}

func (a *apcAdapter) BootB() error {
	if _, ok := a.enabled(func(s apcState) bool { return s.b == "booting" }); !ok {
		return nil
	}
	name, err := a.boot(a.b, "b")
	a.bTurn = name
	return err
}

// finish releases id's held turn and waits for the row, the report and
// whatever the freed slot starts.
func (a *apcAdapter) finish(id string, turn *string) error {
	control.Release(a.t, a.dir, *turn)
	*turn = ""
	if err := a.waitChild(id, "idle", func(s string) bool { return s == "idle" }); err != nil {
		return err
	}
	if err := a.reported(id); err != nil {
		return err
	}
	return a.settle(true)
}

func (a *apcAdapter) FinishA() error {
	if _, ok := a.enabled(func(s apcState) bool { return s.a == "running" }); !ok {
		return nil
	}
	return a.finish(a.a, &a.aTurn)
}

func (a *apcAdapter) FinishB() error {
	if _, ok := a.enabled(func(s apcState) bool { return s.b == "running" }); !ok {
		return nil
	}
	return a.finish(a.b, &a.bTurn)
}

func (a *apcAdapter) FinishThread() error {
	if _, ok := a.enabled(func(s apcState) bool { return s.thread == "running" }); !ok {
		return nil
	}
	return a.finish(a.thread, &a.tTurn)
}

// Archive is archiveRow: the choice dialog when the row counts a
// running or queued agent, else the plain confirm.
func (a *apcAdapter) Archive() error {
	st, ok := a.enabled(func(s apcState) bool { return !s.archived && s.dialog == "none" && s.req == "none" })
	if !ok {
		return nil
	}
	if st.agents.Running+st.agents.Queued > 0 {
		a.dialog = "choice"
	} else {
		a.dialog = "confirm"
	}
	return nil
}

func (a *apcAdapter) Cancel() error {
	if _, ok := a.enabled(func(s apcState) bool { return s.dialog != "none" }); ok {
		a.dialog = "none"
	}
	return nil
}

func (a *apcAdapter) choose(from string, stop bool) error {
	if _, ok := a.enabled(func(s apcState) bool { return s.dialog == from }); ok {
		a.dialog, a.req, a.stop, a.done = "none", "sent", stop, "none"
	}
	return nil
}

func (a *apcAdapter) Confirm() error        { return a.choose("confirm", false) }
func (a *apcAdapter) StopAndArchive() error { return a.choose("choice", true) }
func (a *apcAdapter) ArchiveOnly() error    { return a.choose("choice", false) }

func (a *apcAdapter) Retry() error {
	if _, ok := a.enabled(func(s apcState) bool { return s.done == "failed" && s.req == "none" && s.archived }); ok {
		a.req, a.stop, a.done = "sent", true, "none"
	}
	return nil
}

func (a *apcAdapter) holdPoint(id string) string { return filepath.Join(a.holds, "archive-end-"+id) }

// parked says whether the request is held before ending id.
func (a *apcAdapter) parked(id string) bool {
	if id == "" {
		return false
	}
	_, err := os.Stat(a.holdPoint(id) + ".at")
	return err == nil
}

// lift takes id's hold away and waits for the request to leave it.
func (a *apcAdapter) lift(id string) error {
	os.Remove(a.holdPoint(id))
	return apcWait("the request to leave the hold before "+id, func() (bool, string) { return !a.parked(id), "parked" })
}

// runTo waits until the request is parked before one of ids or has
// answered.
func (a *apcAdapter) runTo(ids ...string) error {
	return apcWait("the archive request to park or answer", func() (bool, string) {
		if a.answer != nil {
			return true, ""
		}
		select {
		case r := <-a.inflight:
			a.answer = &r
			return true, ""
		default:
		}
		for _, id := range ids {
			if a.parked(id) {
				return true, ""
			}
		}
		return false, "running"
	})
}

// collect is the page getting the answer: act refreshes the list on
// success and on failure alike.
func (a *apcAdapter) collect() (apcResp, error) {
	if a.answer == nil {
		select {
		case r := <-a.inflight:
			a.answer = &r
		case <-time.After(actionTimeout):
			return apcResp{}, errors.New("the archive request did not answer")
		}
	}
	r := *a.answer
	a.inflight, a.answer = nil, nil
	if r.err != nil {
		return r, r.err
	}
	listed, err := a.inList()
	a.listed = listed
	return r, err
}

func (a *apcAdapter) inList() (bool, error) {
	var r struct {
		Sessions []serve.Row `json:"sessions"`
	}
	if err := a.ok(http.MethodGet, "/api/sessions", nil, &r); err != nil {
		return false, err
	}
	return slices.ContainsFunc(r.Sessions, func(s serve.Row) bool { return s.ID == a.parent }), nil
}

// SetArchived sends the request the dialog chose. With stopChildren it
// parks before its first child, so this step is SetArchived alone.
func (a *apcAdapter) SetArchived() error {
	if _, ok := a.enabled(func(s apcState) bool { return s.req == "sent" }); !ok {
		return nil
	}
	a.clearHolds()
	var body any
	if a.stop {
		body = map[string]bool{"stopChildren": true}
		for _, id := range []string{a.a, a.b, a.thread} {
			if id != "" {
				if err := os.WriteFile(a.holdPoint(id), nil, 0o644); err != nil {
					return err
				}
			}
		}
	}
	ch := make(chan apcResp, 1)
	a.inflight, a.answer = ch, nil
	go func() {
		status, raw, err := a.call(http.MethodPost, "/api/sessions/"+a.parent+"/archive", body, nil)
		ch <- apcResp{status, raw, err}
	}()
	if err := a.runTo(a.a, a.b, a.thread); err != nil {
		return err
	}
	if err := apcWait("the parent's process to end", func() (bool, string) { return !a.sup.Live(a.parent), "live" }); err != nil {
		return err
	}
	if !a.stop {
		r, err := a.collect()
		if err != nil {
			return err
		}
		a.req, a.done = "none", "ok_only"
		if r.status != http.StatusOK {
			a.done = "failed"
		}
		return nil
	}
	a.req, a.stop, a.tsnap = "loop_a", false, a.thread != ""
	return nil
}

// ended waits for what ending a child that was was leaves: its process
// gone (a queued one's row gone), its report when it was mid-turn, and
// the queue's drain (settle).
func (a *apcAdapter) ended(id, was string, start bool) error {
	if id == "" || was == "none" {
		return nil
	}
	if err := a.waitChild(id, "the end", func(s string) bool { return s == "ended" || s == "none" }); err != nil {
		return err
	}
	if was == "running" {
		if err := a.reported(id); err != nil {
			return err
		}
	}
	return a.settle(start)
}

func (a *apcAdapter) EndA() error {
	st, ok := a.enabled(func(s apcState) bool { return s.req == "loop_a" })
	if !ok {
		return nil
	}
	if a.parked(a.a) {
		if a.endAReleasesB {
			os.Remove(a.holdPoint(a.b))
		}
		if err := a.lift(a.a); err != nil {
			return err
		}
		if err := a.runTo(a.b, a.thread); err != nil {
			return err
		}
	}
	if err := a.ended(a.a, st.a, true); err != nil {
		return err
	}
	a.req = "loop_b"
	return nil
}

// EndAOrbFails is the same EndChild(a) with a container that will not
// stop: the loop answers 500 before it reaches b.
func (a *apcAdapter) EndAOrbFails() error {
	st, ok := a.enabled(func(s apcState) bool {
		return s.req == "loop_a" && s.a != "none" && (s.orb == "up" || s.orb == "leaked")
	})
	if !ok {
		return nil
	}
	name := container.OrbName(a.a)
	a.rt.failStop(name, true)
	defer a.rt.failStop(name, false)
	if err := a.lift(a.a); err != nil {
		return err
	}
	r, err := a.collect()
	if err != nil {
		return err
	}
	a.clearHolds()
	a.req, a.done, a.tsnap = "none", "failed", false
	if r.status/100 == 2 {
		a.done = "ok_stop"
	}
	return a.ended(a.a, st.a, true)
}

func (a *apcAdapter) EndB() error {
	st, ok := a.enabled(func(s apcState) bool { return s.req == "loop_b" })
	if !ok {
		return nil
	}
	if a.parked(a.b) {
		if err := a.lift(a.b); err != nil {
			return err
		}
	}
	// A thread in the snapshot is ended after b, once b's slot has
	// drained it (then it is killed at its boot hold, never started).
	inSnap := a.tsnap
	a.tsnap = false
	if inSnap {
		if err := a.runTo(a.thread); err != nil {
			return err
		}
		if err := a.ended(a.b, st.b, false); err != nil {
			return err
		}
		if a.parked(a.thread) {
			if err := a.lift(a.thread); err != nil {
				return err
			}
		}
	}
	r, err := a.collect()
	if err != nil {
		return err
	}
	a.req, a.done = "none", "ok_stop"
	if r.status/100 != 2 {
		a.done = "failed"
	}
	if inSnap {
		return a.ended(a.thread, st.thread, false)
	}
	return a.ended(a.b, st.b, true)
}

// Reap is a reaper tick long after everything went quiet.
func (a *apcAdapter) Reap() error {
	if _, ok := a.enabled(func(s apcState) bool { return s.orb == "leaked" }); !ok {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	a.api.ReapIdleOrbs(ctx, time.Hour, time.Now().Add(24*time.Hour))
	return nil
}

func (a *apcAdapter) Unarchive() error {
	if _, ok := a.enabled(func(s apcState) bool { return s.archived && s.req == "none" }); !ok {
		return nil
	}
	if err := a.ok(http.MethodPost, "/api/sessions/"+a.parent+"/unarchive", nil, nil); err != nil {
		return err
	}
	listed, err := a.inList()
	a.listed = listed
	return err
}

func apcAction(name string, f func(*apcAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*apcAdapter)
		err := f(a)
		if !a.gate.off {
			a.ran[name]++
		}
		return nil, err
	}
}

var archiveParentWithChildrenActions = map[string]map[string]fmbt.ActionFunc{"Parent": {
	"Prompt":         apcAction("Prompt", (*apcAdapter).Prompt),
	"SpawnA":         apcAction("SpawnA", (*apcAdapter).SpawnA),
	"SpawnB":         apcAction("SpawnB", (*apcAdapter).SpawnB),
	"SpawnRefused":   apcAction("SpawnRefused", (*apcAdapter).SpawnRefused),
	"ThreadLate":     apcAction("ThreadLate", (*apcAdapter).ThreadLate),
	"BootA":          apcAction("BootA", (*apcAdapter).BootA),
	"BootB":          apcAction("BootB", (*apcAdapter).BootB),
	"FinishA":        apcAction("FinishA", (*apcAdapter).FinishA),
	"FinishB":        apcAction("FinishB", (*apcAdapter).FinishB),
	"FinishThread":   apcAction("FinishThread", (*apcAdapter).FinishThread),
	"Archive":        apcAction("Archive", (*apcAdapter).Archive),
	"Cancel":         apcAction("Cancel", (*apcAdapter).Cancel),
	"Confirm":        apcAction("Confirm", (*apcAdapter).Confirm),
	"StopAndArchive": apcAction("StopAndArchive", (*apcAdapter).StopAndArchive),
	"ArchiveOnly":    apcAction("ArchiveOnly", (*apcAdapter).ArchiveOnly),
	"SetArchived":    apcAction("SetArchived", (*apcAdapter).SetArchived),
	"EndA":           apcAction("EndA", (*apcAdapter).EndA),
	"EndAOrbFails":   apcAction("EndAOrbFails", (*apcAdapter).EndAOrbFails),
	"EndB":           apcAction("EndB", (*apcAdapter).EndB),
	"Retry":          apcAction("Retry", (*apcAdapter).Retry),
	"Reap":           apcAction("Reap", (*apcAdapter).Reap),
	"Unarchive":      apcAction("Unarchive", (*apcAdapter).Unarchive),
}}

// archiveParentWithChildrenHistory reads a's own transcript as the
// steps that lead there: its first input is Prompt, SpawnA and BootA
// (a turn needs a live parent, a spawn and a boot), a done that closes
// it is FinishA, and a cancelled one is Stop and archive ending it
// mid-turn (a cancel reaches a only from EndChild). A killed process
// that never wrote a cancel is the same end. Any later input is a
// second turn, which no path has.
func archiveParentWithChildrenHistory(entries []history.Entry) []tracecheck.Step {
	a := func(v string) map[string]any { return map[string]any{"Parent#0.a": v} }
	steps := []tracecheck.Step{{Action: "Init", State: a("none")}}
	open := false
	for _, e := range entries {
		switch {
		case e.Kind == "input" && len(steps) == 1:
			steps = append(steps,
				tracecheck.Step{Action: "Parent#0.Prompt", State: map[string]any{"Parent#0.live": true}},
				tracecheck.Step{Action: "Parent#0.SpawnA", State: a("booting")},
				tracecheck.Step{Action: "Parent#0.BootA", State: a("running")})
			open = true
		case e.Kind == "input":
			// a gets one task: a second turn is not in the model, and
			// the check must say so rather than skip it.
			steps = append(steps, tracecheck.Step{Action: "Parent#0.BootA", State: a("running")})
			open = true
		case e.Kind == "done" && open:
			steps = append(steps, tracecheck.Step{Action: "Parent#0.FinishA", State: a("idle")})
			open = false
		case e.Kind == "cancelled" && open:
			open = false
			steps = append(steps, apcStopSteps()...)
		}
	}
	if open {
		steps = append(steps, apcStopSteps()...)
	}
	return steps
}

func apcStopSteps() []tracecheck.Step {
	return []tracecheck.Step{
		{Action: "Parent#0.Archive", State: map[string]any{"Parent#0.dialog": "choice"}},
		{Action: "Parent#0.StopAndArchive"},
		{Action: "Parent#0.SetArchived", State: map[string]any{"Parent#0.req": "loop_a", "Parent#0.archived": true}},
		{Action: "Parent#0.EndA", State: map[string]any{"Parent#0.a": "ended", "Parent#0.unread": true}},
	}
}

func init() { historyProjections["archive_parent_with_children"] = archiveParentWithChildrenHistory }

// TestArchiveParentWithChildrenPaths walks the paths derived from the
// spec's graph (every state; every link under MODEL_COVER=transitions)
// on one serve, comparing the adapter's state with the path's after
// every step, then replays every a's transcript on the graph.
//
// A walk boots several real sessions, and one serve walking all of
// them took nine minutes (its history dir, which every row read lists,
// only grows), so the walks are dealt out to apcShards serves.
func TestArchiveParentWithChildrenPaths(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(filepath.Join("..", "testdata", "archive_parent_with_children"))
	if err != nil {
		t.Fatal(err)
	}
	for shard := range apcShards {
		t.Run(fmt.Sprint("shard", shard), func(t *testing.T) {
			t.Parallel()
			a := newAPCAdapter(t)
			if err := walkAPCPaths(t, a, envCover(), shard, apcShards); err != nil {
				t.Fatal(err)
			}
			t.Logf("steps run: %v", a.ran)
			checked := 0
			for _, id := range a.aIDs {
				if entries := a.entries(id); len(entries) > 0 {
					checkHistory(t, g, entries, archiveParentWithChildrenHistory)
					checked++
				}
			}
			if checked == 0 {
				t.Fatal("no a wrote a transcript; the trace check checked nothing")
			}
		})
	}
}

const apcShards = 4

// A walk that ends b in EndA (its hold lifted with a's) must fail, or
// the holds prove nothing about the request's steps.
func TestArchiveParentWithChildrenCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newAPCAdapter(t)
	a.endAReleasesB = true
	err := walkAPCPaths(t, a, envCover(), 0, 1)
	if err == nil {
		t.Fatal("a walk whose EndA also ends b passed")
	}
	t.Logf("caught as expected: %v", err)
}

// The trace check must refuse a transcript no path has: a second turn
// of a after it finished.
func TestArchiveParentWithChildrenHistoryRejects(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(filepath.Join("..", "testdata", "archive_parent_with_children"))
	if err != nil {
		t.Fatal(err)
	}
	entry := func(kind string) history.Entry {
		return history.Entry{Kind: kind, Data: map[string]any{"text": "task a"}}
	}
	good := []history.Entry{entry("input"), entry("done")}
	if v := g.Check(archiveParentWithChildrenHistory(good)); v != nil {
		t.Fatalf("a finished a is not a path: %v", v)
	}
	bad := append(good, entry("input"))
	v := g.Check(archiveParentWithChildrenHistory(bad))
	if v == nil {
		t.Fatal("a's second turn passed the trace check")
	}
	t.Logf("refused as expected: %v", v)
}

// walkAPCPaths walks every of-th path from shard on a's serve.
func walkAPCPaths(t *testing.T, a *apcAdapter, cover tracecheck.Cover, shard, of int) error {
	t.Helper()
	b, err := pathsJSONCover("archive_parent_with_children", cover)
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
	acts := archiveParentWithChildrenActions["Parent"]
	for pi, p := range doc.Paths {
		if pi%of != shard {
			continue
		}
		if err := a.Init(); err != nil {
			return fmt.Errorf("path %d: Init: %w", pi, err)
		}
		var names []string
		for si, step := range p.Trace {
			name := strings.TrimPrefix(step.Action, "Parent#0.")
			names = append(names, name)
			if si > 0 {
				if _, err := acts[name](a, nil); err != nil {
					a.Cleanup()
					return fmt.Errorf("path %d %v: %w", pi, names, err)
				}
				if a.gate.off {
					a.Cleanup()
					return fmt.Errorf("path %d %v: the adapter found %s not enabled", pi, names, name)
				}
			}
			got, err := a.GetState()
			if err != nil {
				a.Cleanup()
				return fmt.Errorf("path %d %v: %w", pi, names, err)
			}
			var diffs []string
			for k, v := range step.State {
				field, ok := strings.CutPrefix(k, "Parent#0.")
				if ok && got[field] != v {
					diffs = append(diffs, fmt.Sprintf("%s is %v, the spec says %v", field, got[field], v))
				}
			}
			if len(diffs) > 0 {
				slices.Sort(diffs)
				a.Cleanup()
				return fmt.Errorf("path %d %v: %s", pi, names, strings.Join(diffs, "; "))
			}
		}
		if err := a.Cleanup(); err != nil {
			return fmt.Errorf("path %d %v: Cleanup: %w", pi, names, err)
		}
	}
	return nil
}
