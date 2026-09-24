//go:build !windows

package mbt

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/queue_vs_refusal.fizz: the composer's queue draining into a
// refusal (the session archived here or elsewhere, serve unreachable, a
// question armed while the flushed POST was on its way).
//
// The adapter plays one tab (web/src/app.tsx Thread and App's
// deliverTo) against a real serve. The queue, the failure rows, the
// draft and the tab's idea of archived and offline have no server
// record, so the adapter keeps them exactly as the page does (the flush
// effect's guard, the failure rows, Retry). Written first against the
// page as it was, where a flush the spec's Flush does not enable was
// posted anyway, this walk went red on the archive and outage cascades
// and on Retry steering a running turn; app.tsx now holds the queue on
// row.archived and a network failure, and Retry re-queues, so the
// page's guard is the spec's require. Everything the server decides is
// read off it:
// whether a POST is refused and why (409 with a pending ask, 409
// ErrArchived, or no answer at all), whether an accepted one starts a
// turn or steers, the archived flag, and the row's status once the step
// that changes it has happened.
//
// "serve down" is the tab losing serve, not serve exiting: the tab
// talks to serve through a forwarder the adapter cuts and restores. A
// real restart kills the child and leaves its turn interrupted, which
// the spec rules out (a row the page cannot poll does not change), and
// serve_restart_resume is the flow about that.
type queueVsRefusalAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	fwd  *forwarder
	tab  *http.Client
	gate gate

	walk  int
	id    string
	ids   []string
	turns int // llm-control turn names, unique across walks

	// The tab's view, field for field with the spec's role (archived and
	// serve are read off the server and the forwarder).
	status       string
	archived     bool // what the adapter did; GetState reads the server's
	seenArchived bool
	offline      bool
	queue        []int
	inflight     int
	failures     []int
	causes       []string
	draft        bool
	killed       bool // an archive killed the child the tab still shows
	made         int
	troubles     int
	refused      int

	stats map[string]int

	// unarchiveSends is the deliberate bug the wrong-adapter test
	// injects: Unarchive posts the queue's head, as a page that
	// re-sent on unarchive would.
	unarchiveSends bool
}

func newQueueVsRefusalAdapter(t *testing.T) *queueVsRefusalAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	f, err := newForwarder(s.Addr)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(f.close)
	// A fresh connection per request: a pooled one would outlive a cut.
	tab := &http.Client{Transport: &http.Transport{DisableKeepAlives: true}, Timeout: actionTimeout}
	return &queueVsRefusalAdapter{t: t, s: s, dir: control.Dir(s.Home), fwd: f, tab: tab, stats: map[string]int{}}
}

func (a *queueVsRefusalAdapter) text(m int) string {
	return fmt.Sprintf("walk %d message %d", a.walk, m)
}

// Init starts each walk on a fresh session whose first turn is running
// and held: the spec's Init.
func (a *queueVsRefusalAdapter) Init() error {
	a.walk++
	a.status, a.archived, a.seenArchived, a.offline = "", false, false, false
	a.queue, a.inflight, a.failures, a.causes = nil, -1, nil, nil
	a.draft, a.killed, a.made, a.troubles, a.refused = false, false, 0, 0, 0
	a.gate.reset()
	if err := a.fwd.up(); err != nil {
		return err
	}
	a.releaseAll(nil)
	before := a.taken()
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), fmt.Sprintf("walk %d first prompt", a.walk))
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	row, _, err = a.wait("the first turn to run", func(r serve.Row, _ []serve.Line) bool {
		return r.Status == serve.StatusRunning && a.takenSince(before)
	})
	a.status = qvrStatus(row.Status)
	return err
}

// Cleanup archives the walk's session, which kills its child: a child
// left alive would take the next walk's queued turns.
func (a *queueVsRefusalAdapter) Cleanup() error {
	if a.id == "" {
		return nil
	}
	if err := a.fwd.up(); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Archive(ctx, a.id)
	if err == nil {
		_, _, err = a.wait("the archived child to go", func(r serve.Row, _ []serve.Line) bool {
			return r.Status != serve.StatusRunning && r.Status != serve.StatusNeedsYou
		})
	}
	a.releaseAll(nil)
	return err
}

func (a *queueVsRefusalAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

func (a *queueVsRefusalAdapter) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return nil, err
	}
	serveUp := "up"
	if !a.fwd.isUp() {
		serveUp = "down"
	}
	return map[string]any{
		"status":        a.status,
		"archived":      row.Archived,
		"seen_archived": a.seenArchived,
		"serve":         serveUp,
		"offline":       a.offline,
		"queue":         append([]int{}, a.queue...),
		"inflight":      a.inflight,
		"failures":      append([]int{}, a.failures...),
		"causes":        append([]string{}, a.causes...),
		"draft":         a.draft,
		"killed":        a.killed,
		"made":          a.made,
		"troubles":      a.troubles,
		"refused":       a.refused,
	}, nil
}

// ---- the person ----

// Enqueue is Cmd/Ctrl+Enter while the turn runs.
func (a *queueVsRefusalAdapter) Enqueue() error {
	if !a.gate.pass(a.status == "running" && !a.seenArchived && a.made < 2) {
		return nil
	}
	a.queue = append(a.queue, a.made)
	a.made++
	a.draft = false
	return nil
}

// RemoveQueued is Remove on the newest queued row.
func (a *queueVsRefusalAdapter) RemoveQueued() error {
	if !a.gate.pass(len(a.queue) > 0) {
		return nil
	}
	a.queue = a.queue[:len(a.queue)-1]
	return nil
}

// Retry on the oldest failure row puts the message back in the queue
// ahead of every younger one (app.tsx requeue); the flush posts it.
func (a *queueVsRefusalAdapter) Retry() error {
	if !a.gate.pass(len(a.failures) > 0) {
		return nil
	}
	m := a.failures[0]
	a.failures, a.causes = a.failures[1:], a.causes[1:]
	j := 0
	for j < len(a.queue) && a.queue[j] < m {
		j++
	}
	a.queue = slices.Insert(a.queue, j, m)
	return nil
}

// Edit on the oldest failure row puts its text back in the composer.
func (a *queueVsRefusalAdapter) Edit() error {
	if !a.gate.pass(len(a.failures) > 0 && !a.draft) {
		return nil
	}
	a.failures, a.causes = a.failures[1:], a.causes[1:]
	a.draft = true
	return nil
}

func (a *queueVsRefusalAdapter) Discard() error {
	if !a.gate.pass(len(a.failures) > 0) {
		return nil
	}
	a.failures, a.causes = a.failures[1:], a.causes[1:]
	return nil
}

func (a *queueVsRefusalAdapter) ClearDraft() error {
	if !a.gate.pass(a.draft) {
		return nil
	}
	a.draft = false
	return nil
}

// ArchiveHere is archiveRow: the server saves the flag and kills the
// child, and the tab marks the row at once. The row's status is read
// by ChildExit, the step that says the child is gone.
func (a *queueVsRefusalAdapter) ArchiveHere() error {
	if !a.gate.pass(!a.archived && !a.seenArchived && a.fwd.isUp() && a.troubles < 2) {
		return nil
	}
	if _, err := a.tabDo(http.MethodPost, "/archive", nil, nil); err != nil {
		return err
	}
	a.archived, a.seenArchived = true, true
	a.killed = a.status != "idle"
	a.troubles++
	return nil
}

// ArchiveElsewhere is another client archiving, over the same API.
func (a *queueVsRefusalAdapter) ArchiveElsewhere() error {
	if !a.gate.pass(!a.archived && !a.seenArchived && a.fwd.isUp() && a.troubles < 2) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if _, err := a.s.Archive(ctx, a.id); err != nil {
		return err
	}
	a.archived = true
	a.killed = a.status != "idle"
	a.troubles++
	return nil
}

// Unarchive only clears the flag.
func (a *queueVsRefusalAdapter) Unarchive() error {
	if !a.gate.pass(a.archived && a.seenArchived && a.fwd.isUp()) {
		return nil
	}
	if _, err := a.tabDo(http.MethodPost, "/unarchive", nil, nil); err != nil {
		return err
	}
	a.archived, a.seenArchived = false, false
	if a.unarchiveSends && len(a.queue) > 0 {
		m := a.queue[0]
		a.queue = a.queue[1:]
		if err := a.deliver(m); err != nil {
			return err
		}
	}
	return nil
}

// ServeDown cuts the tab off from serve.
func (a *queueVsRefusalAdapter) ServeDown() error {
	if !a.gate.pass(a.fwd.isUp() && a.troubles < 2) {
		return nil
	}
	a.fwd.down()
	a.troubles++
	return nil
}

// AskArrives: another client's prompt starts a turn and it asks, while
// this tab's flushed POST is still on its way.
func (a *queueVsRefusalAdapter) AskArrives() error {
	if !a.gate.pass(a.status == "idle" && a.inflight >= 0 && !a.archived && a.fwd.isUp() && a.troubles < 2) {
		return nil
	}
	before := a.taken()
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, fmt.Sprintf("walk %d other client", a.walk)); err != nil {
		return err
	}
	if _, _, err := a.wait("the other client's turn", func(r serve.Row, _ []serve.Line) bool {
		return r.Status == serve.StatusRunning && a.takenSince(before)
	}); err != nil {
		return err
	}
	a.releaseAll(&control.Turn{Call: &control.Call{Name: "ask", Args: map[string]any{"question": fmt.Sprintf("walk %d which one?", a.walk)}}})
	row, _, err := a.wait("the question", func(r serve.Row, _ []serve.Line) bool { return r.Status == serve.StatusNeedsYou })
	if err != nil {
		return err
	}
	a.status = qvrStatus(row.Status)
	a.troubles++
	return nil
}

// AnswerAsk: the question is answered and its turn runs on.
func (a *queueVsRefusalAdapter) AnswerAsk() error {
	if !a.gate.pass(a.status == "needs-you" && a.fwd.isUp() && !a.archived && !a.killed) {
		return nil
	}
	before := a.taken()
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Answer(ctx, a.id, fmt.Sprintf("walk %d my answer", a.walk)); err != nil {
		return err
	}
	row, _, err := a.wait("the answered turn to run on", func(r serve.Row, _ []serve.Line) bool {
		return r.Status == serve.StatusRunning && a.takenSince(before)
	})
	if err != nil {
		return err
	}
	a.status = qvrStatus(row.Status)
	return nil
}

// ---- the page, serve and the child ----

// Flush is the flush effect handing the queue's head to deliver(). The
// POST is on its way until Post.
func (a *queueVsRefusalAdapter) Flush() error {
	if !a.gate.pass(a.flushEnabled()) {
		return nil
	}
	a.inflight = a.queue[0]
	a.queue = a.queue[1:]
	return nil
}

// Post: the flushed POST reaches serve (or does not), then deliverTo's
// refresh.
func (a *queueVsRefusalAdapter) Post() error {
	if !a.gate.pass(a.inflight >= 0) {
		return nil
	}
	m := a.inflight
	a.inflight = -1
	return a.deliver(m)
}

// TurnEnds: the model answers every request the turn makes until it
// ends.
func (a *queueVsRefusalAdapter) TurnEnds() error {
	if !a.gate.pass(a.status == "running" && a.fwd.isUp() && !a.archived && !a.killed) {
		return nil
	}
	row, _, err := a.wait("the turn to end", func(r serve.Row, _ []serve.Line) bool {
		if r.Status != serve.StatusRunning {
			return true
		}
		a.releaseAll(nil)
		return false
	})
	if err != nil {
		return err
	}
	a.status = qvrStatus(row.Status)
	return nil
}

// ChildExit: the tab re-reads the row of a child an archive killed
// (archive answers once the child is reaped); its turn (or question)
// went with it.
func (a *queueVsRefusalAdapter) ChildExit() error {
	if !a.gate.pass(a.killed) {
		return nil
	}
	row, _, err := a.wait("the archived child to go", func(r serve.Row, _ []serve.Line) bool {
		return r.Status != serve.StatusRunning && r.Status != serve.StatusNeedsYou
	})
	if err != nil {
		return err
	}
	a.releaseAll(nil) // the killed child's request: nobody reads it now
	a.status = qvrStatus(row.Status)
	a.killed = false
	return nil
}

// Poll is the tab's list poll catching up with an archive it did not
// make.
func (a *queueVsRefusalAdapter) Poll() error {
	if !a.gate.pass(a.fwd.isUp() && a.seenArchived != a.archived) {
		return nil
	}
	if err := a.refresh(); err != nil {
		return err
	}
	return nil
}

// ServeBack: the tab reaches serve again and its next poll says so.
func (a *queueVsRefusalAdapter) ServeBack() error {
	if !a.gate.pass(!a.fwd.isUp()) {
		return nil
	}
	if err := a.fwd.up(); err != nil {
		return err
	}
	// The poll that answers; what it says about archived is Poll's step.
	if _, err := a.tabDo(http.MethodGet, "", nil, nil); err != nil {
		return err
	}
	a.offline = false
	return nil
}

// ---- the page's own logic ----

// flushEnabled is the flush effect's guard in app.tsx: running, row.ask,
// row.archived, offline, flushing (a POST out) or an empty queue hold it.
func (a *queueVsRefusalAdapter) flushEnabled() bool {
	return a.status == "idle" && a.inflight == -1 && len(a.queue) > 0 && !a.seenArchived && !a.offline
}

// deliver is deliver() + deliverTo: POST the message through the tab,
// refresh, and turn a refusal into a failure row naming its cause.
func (a *queueVsRefusalAdapter) deliver(m int) error {
	after := a.newest()
	code, err := a.tabDo(http.MethodPost, "/prompt", map[string]string{"text": a.text(m)}, nil)
	cause := ""
	switch {
	case err != nil && code == 0:
		cause = "network"
		a.offline = true
	case err != nil && code == http.StatusConflict && strings.Contains(err.Error(), "pending ask"):
		cause = "ask"
	case err != nil && code == http.StatusConflict && strings.Contains(err.Error(), "archived"):
		cause = "archived"
	case err != nil:
		return fmt.Errorf("post message %d: %w", m, err)
	}
	if cause == "" {
		return a.landed(m, after)
	}
	if err := a.refresh(); err != nil && cause != "network" {
		return err
	}
	// Each request is its own row; a retry that fails again replaces its
	// own, at the end.
	i := slices.Index(a.failures, m)
	if i >= 0 {
		a.failures = slices.Delete(a.failures, i, i+1)
		a.causes = slices.Delete(a.causes, i, i+1)
	}
	a.failures = append(a.failures, m)
	a.causes = append(a.causes, cause)
	a.refused++
	return nil
}

// landed waits for an accepted message's input: a new turn from idle, a
// steer into the running one.
func (a *queueVsRefusalAdapter) landed(m int, after int64) error {
	text := a.text(m)
	in := func(ls []serve.Line) bool {
		return slices.ContainsFunc(ls, func(l serve.Line) bool {
			return l.Kind == "input" && l.Seq > after && strings.TrimSpace(l.Text) == text
		})
	}
	row, _, err := a.wait(fmt.Sprintf("message %d's input", m), func(r serve.Row, ls []serve.Line) bool {
		return in(ls) && r.Status == serve.StatusRunning && len(a.held()) > 0
	})
	if err != nil {
		return err
	}
	a.status = qvrStatus(row.Status)
	a.killed = false
	return nil
}

// refresh is the tab reading the row through its own connection: the
// poll, and deliverTo's refresh after a send.
func (a *queueVsRefusalAdapter) refresh() error {
	var r struct {
		Session serve.Row `json:"session"`
	}
	if _, err := a.tabDo(http.MethodGet, "", nil, &r); err != nil {
		return err
	}
	a.seenArchived = r.Session.Archived
	return nil
}

// tabDo is one request from the tab, through the forwarder. code is 0
// when serve never answered.
func (a *queueVsRefusalAdapter) tabDo(method, verb string, body, out any) (int, error) {
	var rd io.Reader
	if body != nil {
		b, _ := json.Marshal(body)
		rd = bytes.NewReader(b)
	}
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, method, "http://"+a.fwd.addr+"/api/sessions/"+url.PathEscape(a.id)+verb, rd)
	if err != nil {
		return 0, err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := a.tab.Do(req)
	if err != nil {
		return 0, err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return 0, err
	}
	if resp.StatusCode/100 != 2 {
		return resp.StatusCode, fmt.Errorf("%s %s: %d: %s", method, verb, resp.StatusCode, bytes.TrimSpace(raw))
	}
	if out != nil {
		return resp.StatusCode, json.Unmarshal(raw, out)
	}
	return resp.StatusCode, nil
}

// qvrStatus is the row's status as the flush reads it (app.tsx: running
// is row.status === "running"): a finished, stopped or killed turn is
// idle. Anything else passes through, so a row the flow never expects
// (error) shows up as a state mismatch.
func qvrStatus(s serve.Status) string {
	switch s {
	case serve.StatusDone, serve.StatusStopped, serve.StatusInterrupted:
		return "idle"
	}
	return string(s)
}

func (a *queueVsRefusalAdapter) newest() int64 {
	ctx, cancel := actionCtx()
	defer cancel()
	_, lines, err := a.s.GetSession(ctx, a.id)
	if err != nil || len(lines) == 0 {
		return 0
	}
	return lines[len(lines)-1].Seq
}

func (a *queueVsRefusalAdapter) wait(what string, ok func(serve.Row, []serve.Line) bool) (serve.Row, []serve.Line, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	var row serve.Row
	var lines []serve.Line
	for {
		r, ls, err := a.s.GetSession(ctx, a.id)
		if err == nil {
			row, lines = r, ls
			if ok(r, ls) {
				return row, lines, nil
			}
		}
		select {
		case <-ctx.Done():
			return row, lines, fmt.Errorf("waiting for %s: last status %q, transcript %s", what, row.Status, kinds(lines))
		case <-time.After(50 * time.Millisecond):
		}
	}
}

// topUp keeps three block turns queued, so every model request is held
// until the adapter answers it.
func (a *queueVsRefusalAdapter) topUp() {
	names, _ := filepath.Glob(filepath.Join(a.dir, "*.json"))
	for n := len(names); n < 3; n++ {
		a.turns++
		control.Queue(a.t, a.dir, fmt.Sprintf("q%08d", a.turns), control.Turn{Mode: "block", Text: "answered"})
	}
}

// taken lists every request ever taken; takenSince says a new one was.
func (a *queueVsRefusalAdapter) taken() map[string]bool {
	names, _ := filepath.Glob(filepath.Join(a.dir, "*.taken"))
	out := map[string]bool{}
	for _, f := range names {
		out[strings.TrimSuffix(filepath.Base(f), ".taken")] = true
	}
	return out
}

func (a *queueVsRefusalAdapter) takenSince(before map[string]bool) bool {
	for n := range a.taken() {
		if !before[n] {
			return true
		}
	}
	return false
}

// held lists the requests in flight: taken and not yet released.
func (a *queueVsRefusalAdapter) held() []string {
	var out []string
	for name := range a.taken() {
		if _, err := os.Stat(filepath.Join(a.dir, name+".release")); errors.Is(err, os.ErrNotExist) {
			out = append(out, name)
		}
	}
	slices.Sort(out)
	return out
}

func (a *queueVsRefusalAdapter) releaseAll(turn *control.Turn) {
	for _, name := range a.held() {
		if turn == nil {
			control.Release(a.t, a.dir, name)
		} else {
			control.ReleaseWith(a.t, a.dir, name, *turn)
		}
	}
	a.topUp()
}

// forwarder is the tab's route to serve: a TCP relay the adapter cuts
// (every connection closed, new ones refused) and restores on the same
// port.
type forwarder struct {
	target, addr string
	mu           sync.Mutex
	ln           net.Listener
	conns        map[net.Conn]bool
}

func newForwarder(target string) (*forwarder, error) {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return nil, err
	}
	f := &forwarder{target: target, addr: ln.Addr().String(), ln: ln, conns: map[net.Conn]bool{}}
	go f.accept(ln)
	return f, nil
}

func (f *forwarder) accept(ln net.Listener) {
	for {
		c, err := ln.Accept()
		if err != nil {
			return
		}
		go f.pipe(c)
	}
}

func (f *forwarder) pipe(c net.Conn) {
	d, err := net.Dial("tcp", f.target)
	if err != nil {
		c.Close()
		return
	}
	f.mu.Lock()
	if f.ln == nil {
		f.mu.Unlock()
		c.Close()
		d.Close()
		return
	}
	f.conns[c], f.conns[d] = true, true
	f.mu.Unlock()
	go func() { io.Copy(d, c); d.Close() }()
	io.Copy(c, d)
	c.Close()
	f.mu.Lock()
	delete(f.conns, c)
	delete(f.conns, d)
	f.mu.Unlock()
}

func (f *forwarder) isUp() bool {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.ln != nil
}

func (f *forwarder) down() {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.ln == nil {
		return
	}
	f.ln.Close()
	f.ln = nil
	for c := range f.conns {
		c.Close()
	}
	f.conns = map[net.Conn]bool{}
}

func (f *forwarder) up() error {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.ln != nil {
		return nil
	}
	ln, err := net.Listen("tcp", f.addr)
	if err != nil {
		return fmt.Errorf("forwarder: listen again on %s: %w", f.addr, err)
	}
	f.ln = ln
	go f.accept(ln)
	return nil
}

func (f *forwarder) close() { f.down() }

// ---- the run ----

var queueVsRefusalActionTable = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Enqueue":          action((*queueVsRefusalAdapter).Enqueue),
	"RemoveQueued":     action((*queueVsRefusalAdapter).RemoveQueued),
	"Retry":            action((*queueVsRefusalAdapter).Retry),
	"Edit":             action((*queueVsRefusalAdapter).Edit),
	"Discard":          action((*queueVsRefusalAdapter).Discard),
	"ClearDraft":       action((*queueVsRefusalAdapter).ClearDraft),
	"ArchiveHere":      action((*queueVsRefusalAdapter).ArchiveHere),
	"ArchiveElsewhere": action((*queueVsRefusalAdapter).ArchiveElsewhere),
	"Unarchive":        action((*queueVsRefusalAdapter).Unarchive),
	"ServeDown":        action((*queueVsRefusalAdapter).ServeDown),
	"AskArrives":       action((*queueVsRefusalAdapter).AskArrives),
	"AnswerAsk":        action((*queueVsRefusalAdapter).AnswerAsk),
	"Flush":            action((*queueVsRefusalAdapter).Flush),
	"Post":             action((*queueVsRefusalAdapter).Post),
	"TurnEnds":         action((*queueVsRefusalAdapter).TurnEnds),
	"ChildExit":        action((*queueVsRefusalAdapter).ChildExit),
	"Poll":             action((*queueVsRefusalAdapter).Poll),
	"ServeBack":        action((*queueVsRefusalAdapter).ServeBack),
}, "": {
	// A quiet thread with its bounds used up links to itself as "end".
	"end": func(any, []fmbt.Arg) (any, error) { return nil, nil },
}}

// queueVsRefusalActions counts what each step did, so a green run says
// how much of it was checked.
func queueVsRefusalActions(a *queueVsRefusalAdapter) map[string]map[string]fmbt.ActionFunc {
	acts := map[string]map[string]fmbt.ActionFunc{"Session": {}, "": {}}
	for role, m := range queueVsRefusalActionTable {
		for name, f := range m {
			acts[role][name] = func(m any, args []fmbt.Arg) (any, error) {
				was := a.gate.off
				v, err := f(m, args)
				switch {
				case err != nil:
					a.stats["failed "+name]++
				case was || a.gate.off:
					a.stats["skipped"]++
				default:
					a.stats["done "+name]++
				}
				return v, err
			}
		}
	}
	return acts
}

func queueVsRefusalOptions() map[string]any {
	return map[string]any{"max-seq-runs": 200, "max-actions": 8, "max-parallel-runs": 0}
}

// queueVsRefusalHistory reads the abstract trace off a transcript. Most
// of the flow never reaches it (the queue, refusals, the tab's view),
// so the projection is one path every transcript also is, checked on
// status alone: this tab's messages were queued while the first turn
// ran; a done ends a turn; a message input is a Flush and its Post; a
// cancel (the dangling turn a respawn closes) is an archive, the kill
// and an unarchive; another client's input is the Flush it raced, its
// ask AskArrives and the answer AnswerAsk. A message that lands as a
// steer was posted after that answer; one that lands as a new turn was
// refused by the question and retried.
func queueVsRefusalHistory(entries []history.Entry) []tracecheck.Step {
	status := func(s string) map[string]any { return map[string]any{"Session#0.status": s} }
	step := func(action, s string) tracecheck.Step {
		return tracecheck.Step{Action: "Session#0." + action, State: status(s)}
	}
	text := func(e history.Entry) string { t, _ := e.Data["text"].(string); return strings.TrimSpace(t) }
	isMsg := func(e history.Entry) bool { return strings.Contains(text(e), " message ") }
	var steps []tracecheck.Step
	cur := ""
	raced := false // the other client's turn raced a flushed message
	for i, e := range entries {
		switch e.Kind {
		case "input":
			switch {
			case len(steps) == 0:
				// Both messages, landed or not: a refused one leaves
				// nothing here but may still have raced a question.
				steps = append(steps, tracecheck.Step{Action: "Init", State: status("running")},
					step("Enqueue", "running"), step("Enqueue", "running"))
			case !isMsg(e):
				steps = append(steps, step("Flush", "idle"))
				raced = true
			case e.Data["steer"] == true || raced:
				// The raced POST, accepted after the answer (a steer) or
				// after the archive that killed the question.
				steps = append(steps, step("Post", "running"))
				raced = false
			default:
				steps = append(steps, step("Flush", "idle"), step("Post", "running"))
			}
			cur = "running"
		case "ask":
			steps = append(steps, step("AskArrives", "needs-you"))
			cur = "needs-you"
		case "ask/answer":
			if raced && !steerNext(entries[i+1:]) {
				// Refused by the question: a failure row, retried later.
				steps = append(steps, step("Post", "needs-you"))
				steps = append(steps, step("AnswerAsk", "running"), step("Retry", "running"))
				raced = false
			} else {
				steps = append(steps, step("AnswerAsk", "running"))
			}
			cur = "running"
		case "cancelled":
			if cur == "running" || cur == "needs-you" {
				steps = append(steps, step("ArchiveHere", cur), step("ChildExit", "idle"), step("Unarchive", "idle"))
			}
			cur = "idle"
		case "done":
			if cur == "running" {
				steps = append(steps, step("TurnEnds", "idle"))
			}
			cur = "idle"
		}
	}
	return steps
}

// steerNext says the next input is a steer.
func steerNext(entries []history.Entry) bool {
	for _, e := range entries {
		if e.Kind == "input" {
			return e.Data["steer"] == true
		}
	}
	return false
}

func init() { historyProjections["queue_vs_refusal"] = queueVsRefusalHistory }

// TestQueueVsRefusal lets fizzbee-mbt walk the spec at random (the
// exhaustive run only; see runMBT).
func TestQueueVsRefusal(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newQueueVsRefusalAdapter(t)
	if err := runMBT(t, "queue_vs_refusal", a, queueVsRefusalActions(a), queueVsRefusalOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("walk steps: %v", a.stats)
	checkQueueVsRefusalHistories(t, a)
}

// TestQueueVsRefusalPaths walks the graph's generated paths step by
// step against the adapter, comparing the whole role state after each
// step, in shards of one serve each.
func TestQueueVsRefusalPaths(t *testing.T) {
	t.Parallel()
	paths := loadPaths(t, "queue_vs_refusal")
	const shards = 4
	for sh := range shards {
		t.Run(fmt.Sprintf("shard%d", sh), func(t *testing.T) {
			t.Parallel()
			a := newQueueVsRefusalAdapter(t)
			acts := queueVsRefusalActions(a)
			for i := sh; i < len(paths); i += shards {
				if err := walkQueueVsRefusal(a, acts, paths[i].Trace); err != nil {
					t.Errorf("path %d %v", i, err)
				}
			}
			t.Logf("shard %d: %d paths, %v", sh, (len(paths)-sh+shards-1)/shards, a.stats)
			checkQueueVsRefusalHistories(t, a)
		})
	}
}

func checkQueueVsRefusalHistories(t *testing.T, a *queueVsRefusalAdapter) {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(specPath("x"), "..", "..", "testdata", "queue_vs_refusal"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), queueVsRefusalHistory)
	}
}

// walkQueueVsRefusal runs one path from Init and returns its first
// divergence: an action that errs, is refused by the adapter's own
// require, or leaves a state other than the path's.
func walkQueueVsRefusal(a *queueVsRefusalAdapter, acts map[string]map[string]fmbt.ActionFunc, trace []tracecheck.Step) (err error) {
	defer func() {
		if cerr := a.Cleanup(); err == nil && cerr != nil {
			err = fmt.Errorf("cleanup: %w", cerr)
		}
	}()
	if err := a.Init(); err != nil {
		return fmt.Errorf("Init: %w", err)
	}
	var names []string
	for i, st := range trace {
		if i > 0 {
			name := strings.TrimPrefix(st.Action, "Session#0.")
			names = append(names, name)
			f := acts["Session"][name]
			if f == nil {
				f = acts[""][name]
			}
			if f == nil {
				return fmt.Errorf("%v: no adapter action for %q", names, st.Action)
			}
			if _, err := f(a, nil); err != nil {
				return fmt.Errorf("%v: %w", names, err)
			}
			if a.gate.off {
				return fmt.Errorf("%v: the adapter refused a step the spec enables (its require disagrees)", names)
			}
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("%v: state: %w", names, err)
		}
		if want := roleState(st.State); !reflect.DeepEqual(jsonRound(got), jsonRound(want)) {
			return fmt.Errorf("%v: state\n got %v\nwant %v", names, jsonRound(got), jsonRound(want))
		}
	}
	return nil
}

// An Unarchive that posts the queue's head (a child spawned by
// unarchiving) must fail the walk, or a green run proves nothing. It
// shows only on an Unarchive with something queued, so every link is
// walked, stopping at the first divergence.
func TestQueueVsRefusalCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	b, err := pathsJSONCover("queue_vs_refusal", tracecheck.CoverTransitions)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []genPath `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		t.Fatal(err)
	}
	a := newQueueVsRefusalAdapter(t)
	a.unarchiveSends = true
	acts := queueVsRefusalActions(a)
	for i, p := range doc.Paths {
		if !slices.ContainsFunc(p.Trace, func(s tracecheck.Step) bool { return s.Action == "Session#0.Unarchive" }) {
			continue
		}
		if err := walkQueueVsRefusal(a, acts, p.Trace); err != nil {
			t.Logf("caught as expected on path %d: %v", i, err)
			return
		}
	}
	t.Fatal("a run whose Unarchive posts the queue's head passed; the paths are not checking state")
}

// The projection is only a check if a transcript the model forbids is
// refused: a queued message that starts a turn while the first still
// runs (no done between them).
func TestQueueVsRefusalHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(filepath.Join(specPath("x"), "..", "..", "testdata", "queue_vs_refusal"))
	if err != nil {
		t.Fatal(err)
	}
	in := func(text string, steer bool) history.Entry {
		e := history.Entry{Kind: "input", Data: map[string]any{"text": text}}
		if steer {
			e.Data["steer"] = true
		}
		return e
	}
	done := history.Entry{Kind: "done"}
	ok := map[string][]history.Entry{
		"flushed":  {in("walk 1 first prompt", false), done, in("walk 1 message 0", false), done},
		"archived": {in("walk 1 first prompt", false), {Kind: "cancelled"}, in("walk 1 message 0", false), done},
		"steered": {in("walk 1 first prompt", false), done, in("walk 1 other client", false), {Kind: "ask"}, {Kind: "ask/answer"},
			in("walk 1 message 0", true), done},
		"retried": {in("walk 1 first prompt", false), done, in("walk 1 other client", false), {Kind: "ask"}, {Kind: "ask/answer"},
			done, in("walk 1 message 0", false), done},
		"raced, then archived": {in("walk 1 first prompt", false), done, in("walk 1 other client", false), {Kind: "ask"},
			{Kind: "cancelled"}, in("walk 1 message 0", false), done},
	}
	for name, es := range ok {
		if v := g.Check(queueVsRefusalHistory(es)); v != nil {
			t.Errorf("%s: %v", name, v)
		}
	}
	bad := []history.Entry{in("walk 1 first prompt", false), in("walk 1 message 0", false), done}
	if v := g.Check(queueVsRefusalHistory(bad)); v == nil {
		t.Error("a message that started a turn while the first ran passed the trace check")
	} else {
		t.Logf("refused as expected: %v", v)
	}
}
