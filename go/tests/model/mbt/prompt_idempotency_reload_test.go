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

// specs/prompt_idempotency_reload.fizz against a real serve: sends whose
// answer is lost, reloads, switches, a serve restart, and one create or
// project message instead of the prompt.
//
// The adapter plays the page as the spec designs it (R2, R3: rows,
// failures and the Stop's memory outlive the Thread and the page), so
// what it tests here is serve's half: R1, a request id serve writes at
// most once (prompt, create, project message), and R2's question, what
// serve knows about an id. The page's half (app.tsx gaps G2-G5) is the
// browser stage's.
//
// Each network step is a real request: ServerWrites is the prompt's POST
// with its request id, Reconcile and ReconcileQueued ask serve about the
// id, ServerMakes is the create or project message POST, Restart and
// ServeBack stop and start serve. A lost answer (ResponseLost,
// LaunchLost, a reload) drops what the adapter got back, or, for a POST
// still "out", never sends it.
//
// The spec's "unread" (serve wrote the line, the child has not recorded
// it) is held open by a user-prompt-submit hook that waits for a file
// named after the line: ChildRecordsInput and ChildRecordsQueued create
// it and wait for the input. So a restart really does kill a line the
// child had not read, and a Stop really does come in before the child
// took it. m_n, q_n and l_n are counted off serve on every read, so a
// second write shows as soon as it lands.
const pirGateHook = `// gate: hold each line until the test opens its gate file.
const k = String(event.input).replace(/[^A-Za-z0-9]/g, "_");
tools.bash("while [ ! -e \"$HOME/gate/" + k + "\" ]; do sleep 0.05; done");
return {};
`

const pirConfig = "- id: llm\n  plugin: llm-control\n" +
	"- id: orb\n  plugin: orb\n  config:\n    runtime: fake\n"

type pirAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	walk  int
	id    string
	ids   []string
	turns int
	slug  string // the project MessageProject talks to, made on first use
	main  string // its main thread, once a message named it

	// The role's fields that are the page's own.
	up                      bool
	viewing                 bool
	draft                   string
	status                  string
	mReq, mSrv, mRow        string
	mSeen, mBehind          bool
	q                       string
	restore, stopM, stopQ   bool
	l                       string
	faults, stops           int
	lKind                   string // "create" or "project": which launch the walk made
	launchDir               string // a create's cwd: l_n counts sessions in it
	stats                   map[string]int
	retryNewID, launchNewID bool // the wrong-adapter bugs
	initSends               bool // ... and the random runner's: Init sends the typed prompt
	retries                 int

	// mEarly: the prompt went out while a turn ran, so it is a steer and
	// its gate opened at once (a steer's hook runs on the engine's actor,
	// and a shut gate there would hold that turn's Stop and its end).
	// It may land before ChildRecordsInput; GetState counts it only
	// from there. mOwnTurn: it went out behind the queued message, which
	// the child was still admitting, so the engine queues it as a turn
	// of its own rather than a steer (see ChildRecordsInput).
	mEarly, mOwnTurn bool
	stopSeq          int64 // the newest seq when Stop was pressed
}

func newPirAdapter(t *testing.T) *pirAdapter {
	s := servetest.Start(t, servetest.Options{Config: pirConfig, Files: map[string]string{
		".bough/hooks/user-prompt-submit/gate.js": pirGateHook,
		"gate/.keep": "",
	}})
	return &pirAdapter{t: t, s: s, dir: control.Dir(s.Home), up: true, stats: map[string]int{}}
}

func (a *pirAdapter) mText() string { return fmt.Sprintf("walk %d prompt", a.walk) }
func (a *pirAdapter) qText() string { return fmt.Sprintf("walk %d queued", a.walk) }
func (a *pirAdapter) lText() string { return fmt.Sprintf("walk %d launch", a.walk) }
func (a *pirAdapter) mRID() string  { return a.rid("m") }
func (a *pirAdapter) qRID() string  { return fmt.Sprintf("pir-%d-q", a.walk) }
func (a *pirAdapter) lRID() string  { return a.rid("l") }
func (a *pirAdapter) gateFile(text string) string {
	k := strings.Map(func(r rune) rune {
		if r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' {
			return r
		}
		return '_'
	}, text)
	return filepath.Join(a.s.Home, "gate", k)
}

// rid is the request id the page keeps for a send across its retries.
// The wrong adapters mint a new one per retry, which is what the page
// does today in effect: serve drops the id (gap G1).
func (a *pirAdapter) rid(what string) string {
	if (what == "m" && a.retryNewID) || (what == "l" && a.launchNewID) {
		return fmt.Sprintf("pir-%d-%s-%d", a.walk, what, a.retries)
	}
	return fmt.Sprintf("pir-%d-%s", a.walk, what)
}

func (a *pirAdapter) open(text string) error {
	return os.WriteFile(a.gateFile(text), nil, 0o644)
}

// Init is the spec's: a fresh idle session on the page, serve up.
func (a *pirAdapter) Init() error {
	if !a.up {
		if err := a.s.Resume(""); err != nil {
			return err
		}
		a.up = true
	}
	a.walk++
	a.viewing, a.draft, a.status = true, "msg", "idle"
	a.mReq, a.mSrv, a.mRow, a.mSeen, a.mBehind = "none", "none", "none", false, false
	a.mEarly, a.mOwnTurn, a.stopSeq = false, false, 0
	a.q, a.restore, a.stopM, a.stopQ = "none", false, false, false
	a.l, a.faults, a.stops, a.lKind, a.retries = "none", 0, 0, "", 0
	a.launchDir = a.s.Dir(a.t, fmt.Sprintf("launch%d", a.walk))
	a.gate.reset()
	a.topUp()
	// The launch's line is never the point of a gate: let it through.
	if err := a.open(a.lText()); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	first := ""
	if a.initSends {
		first = a.mText()
		if err := a.open(first); err != nil {
			return err
		}
	}
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), first)
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	if first != "" {
		_, _, err = a.wait("the prompt Init sent", func(_ serve.Row, ls []serve.Line) bool { return countLines(ls, first) > 0 })
	}
	return err
}

// Cleanup brings serve back if the walk left it down, opens every gate
// and archives the walk's session, so nothing of it takes the next
// walk's turns.
func (a *pirAdapter) Cleanup() error {
	var errs []error
	if !a.up {
		errs = append(errs, a.s.Resume(""))
		a.up = true
	}
	for _, t := range []string{a.mText(), a.qText()} {
		errs = append(errs, a.open(t))
	}
	if a.id != "" {
		ctx, cancel := actionCtx()
		_, err := a.s.Archive(ctx, a.id)
		cancel()
		errs = append(errs, err)
	}
	if a.main != "" {
		_, err := waitRow(a.s, a.main, "main to settle", func(r serve.Row) bool {
			if r.Status != serve.StatusRunning {
				return true
			}
			a.releaseAll(nil)
			return false
		})
		errs = append(errs, err)
	}
	a.releaseAll(nil)
	return errors.Join(errs...)
}

func (a *pirAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

// GetState is the role. The counts are serve's: inputs recorded with the
// prompt's and the queued message's text, and sessions (or main-thread
// inputs) made for the launch.
func (a *pirAdapter) GetState() (map[string]any, error) {
	mN, qN, lN, err := a.counts()
	if err != nil {
		return nil, err
	}
	if a.mEarly && a.mSrv == "unread" && mN > 0 {
		mN--
	}
	serveState := "up"
	if !a.up {
		serveState = "down"
	}
	return map[string]any{
		"serve": serveState, "viewing": a.viewing, "draft": a.draft, "status": a.status,
		"m_req": a.mReq, "m_srv": a.mSrv, "m_n": mN, "m_row": a.mRow, "m_seen": a.mSeen,
		"m_behind": a.mBehind, "q": a.q, "q_n": qN, "restore": a.restore,
		"stop_m": a.stopM, "stop_q": a.stopQ, "l": a.l, "l_n": lN,
		"faults": a.faults, "stops": a.stops,
	}, nil
}

// counts reads m_n, q_n and l_n off disk: history files are readable
// while serve is down, which the API is not.
func (a *pirAdapter) counts() (mN, qN, lN int, err error) {
	entries, err := a.entries(a.id)
	if err != nil {
		return 0, 0, 0, err
	}
	mN, qN = countInputs(entries, a.mText()), countInputs(entries, a.qText())
	switch a.lKind {
	case "create":
		lN, err = a.sessionsIn(a.launchDir)
	case "project":
		if a.main != "" {
			var es []history.Entry
			es, err = a.entries(a.main)
			lN = countInputs(es, a.lText())
		}
	}
	return mN, qN, lN, err
}

func (a *pirAdapter) entries(id string) ([]history.Entry, error) {
	es, err := history.Read(filepath.Join(a.s.Home, ".bough", "history", id+".jsonl"))
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	return es, err
}

func countInputs(entries []history.Entry, text string) int {
	n := 0
	for _, e := range entries {
		if e.Kind == "input" && strings.TrimSpace(pirEntryText(e)) == text {
			n++
		}
	}
	return n
}

func pirEntryText(e history.Entry) string {
	if s, ok := e.Data["typed"].(string); ok {
		return s
	}
	s, _ := e.Data["text"].(string)
	return s
}

// sessionsIn counts the sessions serve made in dir: every history file
// whose meta names it as its cwd, read off disk like the counts above.
func (a *pirAdapter) sessionsIn(dir string) (int, error) {
	files, _ := filepath.Glob(filepath.Join(a.s.Home, ".bough", "history", "*.jsonl"))
	n := 0
	for _, f := range files {
		es, err := history.Read(f)
		if err != nil {
			continue
		}
		for _, e := range es {
			if e.Kind == "meta" {
				if cwd, _ := e.Data["cwd"].(string); cwd == dir {
					n++
				}
				break
			}
		}
	}
	return n, nil
}

// ---- the person ----

func (a *pirAdapter) mPending() bool {
	return a.mRow == "sending" || a.mRow == "waiting" || a.mRow == "unknown"
}

func (a *pirAdapter) Send() error {
	if a.gate.pass(a.viewing && a.draft == "msg" && a.mSrv == "none" && a.mRow == "none" &&
		a.mReq == "none" && a.l == "none" && a.nM() == 0) {
		a.draft, a.mRow, a.mReq = "", "sending", "out"
	}
	return nil
}

func (a *pirAdapter) nM() int { n, _, _, _ := a.counts(); return n }

func (a *pirAdapter) Enqueue() error {
	if a.gate.pass(a.viewing && a.q == "none" && (a.status == "running" || a.mRow == "sending" || a.mRow == "waiting")) {
		a.q = "queued"
	}
	return nil
}

func (a *pirAdapter) Retry() error {
	if a.gate.pass(a.viewing && (a.mRow == "failed" || a.mRow == "unknown")) {
		a.mRow, a.mReq = "sending", "out"
		a.retries++
	}
	return nil
}

func (a *pirAdapter) Discard() error {
	if a.gate.pass(a.viewing && a.mRow == "failed") {
		a.mRow = "none"
	}
	return nil
}

// Stop is Esc: the page waits for sends in flight (none: the spec
// requires m_req != out), then interrupts. A turn running is over once
// its cancel is recorded; a line not yet taken is serve's held
// interrupt, which ChildRecordsInput / ChildRecordsQueued settle.
func (a *pirAdapter) Stop() error {
	live := a.status == "running" || a.mRow == "sending" || a.mRow == "waiting" || a.q == "unread"
	if !a.gate.pass(a.viewing && a.up && a.stops < 1 && a.mReq != "out" && live) {
		return nil
	}
	a.stops++
	a.restore = true
	a.stopM = a.mSrv == "unread"
	a.stopQ = a.q == "unread"
	since := a.newest(a.id)
	a.stopSeq = since
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Interrupt(ctx, a.id); err != nil {
		// After a restart the rows can still be live on the page with no
		// child holding the session (nothing re-sent, or a retry serve
		// deduplicated): serve answers 404 and the page shows "Retry
		// stop", which specs/stop_interrupt.fizz owns. Here that Stop
		// has nothing to stop, so its effect on this spec's state is the
		// same.
		var e *servetest.APIError
		idle := a.status != "running" && a.mSrv != "unread" && a.q != "unread"
		if !idle || !errors.As(err, &e) || e.Status != http.StatusNotFound {
			return err
		}
	}
	if a.status != "running" {
		return nil
	}
	_, _, err := a.waitFor(stopWait, "the stopped turn's cancel", func(r serve.Row, ls []serve.Line) bool {
		return r.Status != serve.StatusRunning && hasKindAfter(ls, "cancelled", since)
	})
	if err != nil {
		return err
	}
	a.releaseAll(nil)
	a.status = "idle"
	if a.mSrv == "running" {
		a.mSrv = "cancelled"
	}
	return nil
}

func (a *pirAdapter) SwitchAway() error {
	if a.gate.pass(a.viewing) {
		a.viewing = false
	}
	return nil
}

func (a *pirAdapter) SwitchBack() error {
	if a.gate.pass(!a.viewing) {
		a.viewing = true
	}
	return nil
}

// ReloadPage loses the answers of requests in flight; the rest of the
// page's state is kept (R3). Nothing reaches serve.
func (a *pirAdapter) ReloadPage() error {
	if !a.gate.pass(a.viewing && a.faults < 1) {
		return nil
	}
	a.faults++
	if a.mReq != "none" {
		a.mReq = "none"
		if a.mRow == "sending" {
			a.mRow = "unknown"
		}
	}
	if a.l == "out" || a.l == "back" {
		a.l = "failed"
	}
	return nil
}

func (a *pirAdapter) launch(kind string) error {
	if !a.gate.pass(a.viewing && a.l == "none" && a.mRow == "none" && a.mReq == "none" && a.nM() == 0) {
		return nil
	}
	a.l, a.lKind = "out", kind
	return nil
}

func (a *pirAdapter) NewSession() error     { return a.launch("create") }
func (a *pirAdapter) MessageProject() error { return a.launch("project") }

func (a *pirAdapter) RetryLaunch() error {
	if a.gate.pass(a.viewing && a.l == "failed") {
		a.l = "out"
		a.retries++
	}
	return nil
}

// ---- the network and serve ----

// ServerWrites is the prompt's POST, carrying its request id.
func (a *pirAdapter) ServerWrites() error {
	if !a.gate.pass(a.up && a.mReq == "out") {
		return nil
	}
	if err := a.prompt(a.mText(), a.mRID()); err != nil {
		return err
	}
	if a.mSrv == "none" || a.mSrv == "lost" {
		a.mSrv = "unread"
		a.mBehind = a.q == "unread"
		a.mOwnTurn = a.mBehind
		if a.status == "running" {
			a.mEarly = true
			if err := a.open(a.mText()); err != nil {
				return err
			}
		}
	} else {
		// Serve already has the id: nothing may be written. A second
		// line past an open gate lands at once; give it the time.
		time.Sleep(300 * time.Millisecond)
	}
	a.mReq = "back"
	return nil
}

func (a *pirAdapter) Answer() error {
	if a.gate.pass(a.mReq == "back") {
		a.mReq = "none"
		if a.mRow == "sending" {
			a.mRow = "waiting"
		}
	}
	return nil
}

func (a *pirAdapter) ResponseLost() error {
	if !a.gate.pass(a.mReq != "none" && a.faults < 1) {
		return nil
	}
	a.faults++
	a.mReq = "none"
	if a.mRow == "sending" {
		a.mRow = "unknown"
	}
	return nil
}

// Reconcile asks serve about the prompt's id (R2): an id serve never
// had, or wrote to a child that died, is Not sent.
func (a *pirAdapter) Reconcile() error {
	if !a.gate.pass(a.up && a.mRow == "unknown" && a.mReq == "none") {
		return nil
	}
	st, err := a.promptState(a.mRID())
	if err != nil {
		return err
	}
	if st == "none" {
		a.mRow = "failed"
	} else {
		a.mRow = "waiting"
	}
	return nil
}

// Restart is serve stopping and every child with it. A line the adapter
// holds unread cannot have landed (its gate is shut); if one did, the
// server took another branch and the walk ends here.
func (a *pirAdapter) Restart() error {
	if !a.gate.pass(a.up && a.faults < 1) {
		return nil
	}
	mN, qN, _, err := a.counts()
	if err != nil {
		return err
	}
	if (a.mSrv == "unread" && mN > 0) || (a.q == "unread" && qN > 0) {
		return fmbt.ErrNotImplemented
	}
	a.faults++
	a.s.Shutdown()
	a.up = false
	deadline := time.Now().Add(3 * time.Second)
	for a.s.GroupAlive() && time.Now().Before(deadline) {
		time.Sleep(20 * time.Millisecond)
	}
	a.releaseAll(nil) // requests the dead children held
	a.mReq = "none"
	if a.mRow == "sending" || a.mRow == "waiting" {
		a.mRow = "unknown"
	}
	switch a.mSrv {
	case "unread":
		a.mSrv, a.mBehind, a.mEarly, a.mOwnTurn = "lost", false, false, false
	case "running":
		a.mSrv = "done"
	}
	a.stopM = false
	if a.q == "unread" {
		a.q, a.stopQ = "lost", false
	}
	a.status = "idle"
	if a.l == "out" || a.l == "back" {
		a.l = "failed"
	}
	return nil
}

func (a *pirAdapter) ServeBack() error {
	if !a.gate.pass(!a.up) {
		return nil
	}
	if err := a.s.Resume(""); err != nil {
		return err
	}
	a.up = true
	return nil
}

// ReconcileQueued: on reconnect the page asks about the flushed line's
// id; serve must not claim a line its dead child never read.
func (a *pirAdapter) ReconcileQueued() error {
	if !a.gate.pass(a.up && a.q == "lost") {
		return nil
	}
	st, err := a.promptState(a.qRID())
	if err != nil {
		return err
	}
	if st != "none" {
		return fmt.Errorf("serve says the queued line died with its child is %q, not none", st)
	}
	a.q = "failed"
	return nil
}

// ServerMakes is the launch's POST with its request id: a create in a
// directory of the walk's own, or a message to the project.
func (a *pirAdapter) ServerMakes() error {
	if !a.gate.pass(a.up && a.l == "out") {
		return nil
	}
	_, _, before, err := a.counts()
	if err != nil {
		return err
	}
	switch a.lKind {
	case "create":
		err = a.api(http.MethodPost, "/api/sessions", map[string]any{"cwd": a.launchDir, "requestId": a.lRID()}, nil)
	case "project":
		err = a.messageProject()
	}
	if err != nil {
		return err
	}
	if before == 0 {
		if err := a.waitCount("the launch to land", func(n int) bool { return n >= 1 }); err != nil {
			return err
		}
	} else {
		time.Sleep(300 * time.Millisecond)
	}
	a.l = "back"
	return nil
}

func (a *pirAdapter) messageProject() error {
	if a.slug == "" {
		var r struct {
			Project serve.Project `json:"project"`
		}
		if err := a.api(http.MethodPost, "/api/projects", map[string]string{"name": "Idempotency"}, &r); err != nil {
			return err
		}
		a.slug = r.Project.Slug
	}
	var out struct {
		Main string `json:"main"`
	}
	if err := a.api(http.MethodPost, "/api/projects/"+a.slug+"/message", map[string]any{"text": a.lText(), "id": a.lRID()}, &out); err != nil {
		return err
	}
	if out.Main != "" {
		a.main = out.Main
	}
	return nil
}

func (a *pirAdapter) waitCount(what string, ok func(int) bool) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		_, _, n, err := a.counts()
		if err == nil && ok(n) {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for %s: l_n %d, err %v", what, n, err)
		}
		time.Sleep(50 * time.Millisecond)
	}
}

func (a *pirAdapter) LaunchAnswer() error {
	if a.gate.pass(a.l == "back") {
		a.l = "done"
	}
	return nil
}

func (a *pirAdapter) LaunchLost() error {
	if a.gate.pass((a.l == "out" || a.l == "back") && a.faults < 1) {
		a.faults++
		a.l = "failed"
	}
	return nil
}

// ---- the child ----

// ChildRecordsInput opens the prompt's gate and waits for serve to show
// what the child did with it: a turn cancelled at once after a held
// Stop, a steer into the running turn, or a turn of its own.
func (a *pirAdapter) ChildRecordsInput() error {
	if !a.gate.pass(a.up && a.mSrv == "unread" && !(a.mBehind && a.q == "unread")) {
		return nil
	}
	// The engine takes a line that arrives while the line before it is
	// still being admitted (its user-prompt-submit hook, here the queued
	// message's gate) as a turn of its own, queued behind the running
	// one, not as a steer: Runtime.Steer sees no turn open and no submit
	// on its way. The spec has it steer; which one the child does is the
	// engine's call, and the walk ends here when it would be a steer.
	if a.mOwnTurn && (a.status == "running" || a.stopM) {
		return fmbt.ErrNotImplemented
	}
	before := countInputs(a.mustEntries(), a.mText())
	if a.mEarly {
		before = 0
	}
	since := a.newest(a.id)
	if a.stopM {
		since = a.stopSeq
	}
	if err := a.open(a.mText()); err != nil {
		return err
	}
	a.mBehind, a.mEarly, a.mOwnTurn = false, false, false
	landed := func(ls []serve.Line) bool { return countLines(ls, a.mText()) > before }
	switch {
	case a.stopM:
		_, _, err := a.waitFor(stopWait, "the held Stop to cancel the prompt's turn", func(r serve.Row, ls []serve.Line) bool {
			return landed(ls) && r.Status != serve.StatusRunning && hasKindAfter(ls, "cancelled", since)
		})
		if err != nil {
			return err
		}
		a.releaseAll(nil)
		a.stopM, a.mSrv = false, "cancelled"
	case a.status == "running":
		if _, _, err := a.wait("the prompt's steer", func(_ serve.Row, ls []serve.Line) bool { return landed(ls) }); err != nil {
			return err
		}
		a.mSrv = "done"
	default:
		if _, _, err := a.wait("the prompt's turn", func(r serve.Row, ls []serve.Line) bool {
			return landed(ls) && r.Status == serve.StatusRunning && len(a.held()) > 0
		}); err != nil {
			return err
		}
		a.mSrv, a.status = "running", "running"
	}
	return nil
}

func (a *pirAdapter) ChildRecordsQueued() error {
	if !a.gate.pass(a.up && a.q == "unread" && !(a.mSrv == "unread" && !a.mBehind)) {
		return nil
	}
	before := countInputs(a.mustEntries(), a.qText())
	since := a.newest(a.id)
	if a.stopQ {
		since = a.stopSeq
	}
	if err := a.open(a.qText()); err != nil {
		return err
	}
	landed := func(ls []serve.Line) bool { return countLines(ls, a.qText()) > before }
	switch {
	case a.stopQ:
		_, _, err := a.waitFor(stopWait, "the held Stop to cancel the queued turn", func(r serve.Row, ls []serve.Line) bool {
			return landed(ls) && r.Status != serve.StatusRunning && hasKindAfter(ls, "cancelled", since)
		})
		if err != nil {
			return err
		}
		a.releaseAll(nil)
		a.stopQ, a.q = false, "turn"
	case a.status == "running":
		if _, _, err := a.wait("the queued message's steer", func(_ serve.Row, ls []serve.Line) bool { return landed(ls) }); err != nil {
			return err
		}
		a.q = "steer"
	default:
		if _, _, err := a.wait("the queued message's turn", func(r serve.Row, ls []serve.Line) bool {
			return landed(ls) && r.Status == serve.StatusRunning && len(a.held()) > 0
		}); err != nil {
			return err
		}
		a.q, a.status = "turn", "running"
	}
	return nil
}

// StopEatsQueued is the race a held interrupt exists to prevent: serve
// holds the Stop for a line the child has not taken, so the line is
// never eaten. The walk ends here: serve took the other branch.
func (a *pirAdapter) StopEatsQueued() error {
	if !a.gate.pass(a.stopQ && a.q == "unread") {
		return nil
	}
	return fmbt.ErrNotImplemented
}

func (a *pirAdapter) Swallow() error {
	if a.gate.pass(a.stopQ && a.q == "swallowed" && a.status == "idle") {
		a.stopQ, a.q = false, "queued"
	}
	return nil
}

// Finish: the model answers every request until the turn ends.
func (a *pirAdapter) Finish() error {
	if !a.gate.pass(a.status == "running") {
		return nil
	}
	// The spec lets the turn end before the child reads a steer, which
	// then runs as a turn of its own; the steer here went through at
	// once (mEarly), so the child took the other branch.
	if a.mEarly && a.mSrv == "unread" {
		return fmbt.ErrNotImplemented
	}
	if _, _, err := a.wait("the turn to end", func(r serve.Row, _ []serve.Line) bool {
		if r.Status != serve.StatusRunning {
			return true
		}
		a.releaseAll(nil)
		return false
	}); err != nil {
		return err
	}
	a.status = "idle"
	if a.mSrv == "running" {
		a.mSrv = "done"
	}
	return nil
}

// ---- the page ----

// Catchup: the transcript shows the recorded prompt, and the page's row
// for it goes.
func (a *pirAdapter) Catchup() error {
	if !a.gate.pass(a.viewing && a.up && !a.mSeen && (a.mSrv == "running" || a.mSrv == "done" || a.mSrv == "cancelled")) {
		return nil
	}
	if _, _, err := a.wait("the prompt in the transcript", func(_ serve.Row, ls []serve.Line) bool {
		return countLines(ls, a.mText()) > 0
	}); err != nil {
		return err
	}
	a.mSeen, a.mRow = true, "none"
	return nil
}

// Flush is the flush effect: the queued message's POST, with its id.
func (a *pirAdapter) Flush() error {
	if !a.gate.pass(a.viewing && a.up && a.status == "idle" && a.q == "queued" && a.mReq == "none" && !a.mPending()) {
		return nil
	}
	if err := a.prompt(a.qText(), a.qRID()); err != nil {
		return err
	}
	a.q = "unread"
	return nil
}

func (a *pirAdapter) Restore() error {
	if !a.gate.pass(a.viewing && a.restore && a.status == "idle" && a.mSrv == "cancelled" && a.mSeen) {
		return nil
	}
	a.restore = false
	if a.draft == "" {
		a.draft = "msg"
	}
	return nil
}

// ---- helpers ----

// prompt is POST /api/sessions/{id}/prompt with the page's request id.
func (a *pirAdapter) prompt(text, rid string) error {
	return a.api(http.MethodPost, "/api/sessions/"+url.PathEscape(a.id)+"/prompt", map[string]any{"text": text, "id": rid}, nil)
}

// promptState is R2's question: what serve knows about a request id,
// "none" (never had it, or its child died unread), "unread" or "landed".
func (a *pirAdapter) promptState(rid string) (string, error) {
	var out struct {
		State string `json:"state"`
	}
	err := a.api(http.MethodGet, "/api/sessions/"+url.PathEscape(a.id)+"/prompts/"+url.PathEscape(rid), nil, &out)
	return out.State, err
}

func (a *pirAdapter) api(method, path string, body, out any) error {
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

func (a *pirAdapter) mustEntries() []history.Entry {
	es, _ := a.entries(a.id)
	return es
}

func countLines(lines []serve.Line, text string) int {
	n := 0
	for _, l := range lines {
		if l.Kind == "input" && strings.TrimSpace(l.Text) == text {
			n++
		}
	}
	return n
}

func (a *pirAdapter) newest(id string) int64 {
	ctx, cancel := actionCtx()
	defer cancel()
	_, lines, err := a.s.GetSession(ctx, id)
	if err != nil || len(lines) == 0 {
		return 0
	}
	return lines[len(lines)-1].Seq
}

func (a *pirAdapter) wait(what string, ok func(serve.Row, []serve.Line) bool) (serve.Row, []serve.Line, error) {
	return a.waitFor(actionTimeout, what, ok)
}

func (a *pirAdapter) waitFor(d time.Duration, what string, ok func(serve.Row, []serve.Line) bool) (serve.Row, []serve.Line, error) {
	ctx, cancel := context.WithTimeout(context.Background(), d)
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
func (a *pirAdapter) topUp() {
	names, _ := filepath.Glob(filepath.Join(a.dir, "*.json"))
	for n := len(names); n < 3; n++ {
		a.turns++
		control.Queue(a.t, a.dir, fmt.Sprintf("p%08d", a.turns), control.Turn{Mode: "block", Text: "answered"})
	}
}

func (a *pirAdapter) held() []string {
	taken, _ := filepath.Glob(filepath.Join(a.dir, "*.taken"))
	var out []string
	for _, f := range taken {
		name := strings.TrimSuffix(filepath.Base(f), ".taken")
		if _, err := os.Stat(filepath.Join(a.dir, name+".release")); errors.Is(err, os.ErrNotExist) {
			out = append(out, name)
		}
	}
	return out
}

func (a *pirAdapter) releaseAll(turn *control.Turn) {
	for _, name := range a.held() {
		if turn == nil {
			control.Release(a.t, a.dir, name)
		} else {
			control.ReleaseWith(a.t, a.dir, name, *turn)
		}
	}
	a.topUp()
}

// pirActions counts what each step did, so a green walk says how much
// was checked.
func pirActions(a *pirAdapter) map[string]map[string]fmbt.ActionFunc {
	acts := map[string]map[string]fmbt.ActionFunc{"Session": {}, "": {}}
	for role, m := range pirActionTable {
		for name, f := range m {
			acts[role][name] = func(m any, args []fmbt.Arg) (any, error) {
				was := a.gate.off
				v, err := f(m, args)
				switch {
				case errors.Is(err, fmbt.ErrNotImplemented):
					a.stats["notTaken "+name]++
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

var pirActionTable = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Send":               action((*pirAdapter).Send),
	"Enqueue":            action((*pirAdapter).Enqueue),
	"Retry":              action((*pirAdapter).Retry),
	"Discard":            action((*pirAdapter).Discard),
	"Stop":               action((*pirAdapter).Stop),
	"SwitchAway":         action((*pirAdapter).SwitchAway),
	"SwitchBack":         action((*pirAdapter).SwitchBack),
	"ReloadPage":         action((*pirAdapter).ReloadPage),
	"NewSession":         action((*pirAdapter).NewSession),
	"MessageProject":     action((*pirAdapter).MessageProject),
	"RetryLaunch":        action((*pirAdapter).RetryLaunch),
	"ServerWrites":       action((*pirAdapter).ServerWrites),
	"Answer":             action((*pirAdapter).Answer),
	"ResponseLost":       action((*pirAdapter).ResponseLost),
	"Reconcile":          action((*pirAdapter).Reconcile),
	"Restart":            action((*pirAdapter).Restart),
	"ServeBack":          action((*pirAdapter).ServeBack),
	"ReconcileQueued":    action((*pirAdapter).ReconcileQueued),
	"ServerMakes":        action((*pirAdapter).ServerMakes),
	"LaunchAnswer":       action((*pirAdapter).LaunchAnswer),
	"LaunchLost":         action((*pirAdapter).LaunchLost),
	"ChildRecordsInput":  action((*pirAdapter).ChildRecordsInput),
	"ChildRecordsQueued": action((*pirAdapter).ChildRecordsQueued),
	"StopEatsQueued":     action((*pirAdapter).StopEatsQueued),
	"Finish":             action((*pirAdapter).Finish),
	"Catchup":            action((*pirAdapter).Catchup),
	"Flush":              action((*pirAdapter).Flush),
	"Restore":            action((*pirAdapter).Restore),
	"Swallow":            action((*pirAdapter).Swallow),
}, "": {
	// A quiet thread with its bounds used up links to itself as "end".
	"end": func(any, []fmbt.Arg) (any, error) { return nil, nil },
}}

func pirOptions() map[string]any {
	return map[string]any{"max-seq-runs": 200, "max-actions": 10, "max-parallel-runs": 0}
}

// pirHistory reads the abstract trace off the thread's transcript. What
// the page and the network do leaves nothing there, so it takes the one
// plain path every such transcript also is, checked on status and the
// counts: the first input is the prompt sent and taken (or, when a
// steer follows it, the queued message flushed after the prompt's send
// failed, the prompt retried into its turn), a cancel is Stop, a done
// Finish, a second input the queued message.
func pirHistory(entries []history.Entry) []tracecheck.Step {
	st := func(kv ...any) map[string]any {
		m := map[string]any{}
		for i := 0; i < len(kv); i += 2 {
			m["Session#0."+kv[i].(string)] = kv[i+1]
		}
		return m
	}
	step := func(action string, s map[string]any) tracecheck.Step {
		return tracecheck.Step{Action: "Session#0." + action, State: s}
	}
	var inputs []history.Entry
	for _, e := range entries {
		if e.Kind == "input" {
			inputs = append(inputs, e)
		}
	}
	steps := []tracecheck.Step{{Action: "Init", State: st("status", "idle", "m_n", 0, "q_n", 0)}}
	if len(inputs) == 0 {
		return steps
	}
	steered := slices.ContainsFunc(inputs, func(e history.Entry) bool { return e.Data["steer"] == true })
	mN, qN, open, cancelled := 0, 0, false, false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			switch {
			case steered && e.Data["steer"] == true:
				mN++
				steps = append(steps, step("Retry", nil), step("ServerWrites", nil),
					step("ChildRecordsInput", st("status", "running", "m_n", mN)))
			case steered:
				qN++
				steps = append(steps, step("Send", nil), step("Enqueue", nil), step("ResponseLost", nil),
					step("Reconcile", st("m_row", "failed")), step("Flush", nil),
					step("ChildRecordsQueued", st("status", "running", "q_n", qN)))
			case mN == 0:
				mN++
				steps = append(steps, step("Send", nil), step("ServerWrites", nil), step("Answer", nil),
					step("ChildRecordsInput", st("status", "running", "m_n", mN)))
				if len(inputs) > 1 {
					steps = append(steps, step("Enqueue", st("q", "queued")))
				}
			default:
				qN++
				steps = append(steps, step("Catchup", st("m_row", "none")), step("Flush", nil),
					step("ChildRecordsQueued", st("status", "running", "q_n", qN)))
			}
			open, cancelled = true, false
		case "cancelled":
			switch {
			case e.Data["interrupted"] == true:
				// Written when the session is next opened after serve
				// died mid-turn: the turn a restart killed.
				steps = append(steps, step("Restart", st("status", "idle")), step("ServeBack", st("serve", "up")))
			case open:
				steps = append(steps, step("Stop", st("status", "idle")))
			}
			open, cancelled = false, true
		case "done":
			if open && !cancelled {
				steps = append(steps, step("Finish", st("status", "idle")))
			}
			open = false
		}
	}
	return steps
}

func init() { historyProjections["prompt_idempotency_reload"] = pirHistory }

func TestPromptIdempotencyReload(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPirAdapter(t)
	if err := runMBT(t, "prompt_idempotency_reload", a, pirActions(a), pirOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("walk steps: %v", a.stats)
	checkPirHistories(t, a)
}

// The runner's walks almost never get past their second step here (one
// run: 42 steps done, 1958 skipped), so the bug it must catch is one the
// first step shows: an Init that sends the typed prompt, which lands
// before anything was sent. The idempotency bugs are the path walk's
// (TestPromptIdempotencyReloadPathsCatchWrongAdapter).
func TestPromptIdempotencyReloadCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPirAdapter(t)
	a.initSends = true
	if err := runMBT(t, "prompt_idempotency_reload", a, pirActions(a), pirOptions()); err == nil {
		t.Fatal("a run whose Init sends the prompt passed; the runner is not checking state")
	}
}

// The runner's walks stop at their first disabled step and rarely get
// past a send; the generated walks over the checked-in graph reach every
// state (every link under MODEL_COVER=transitions). Four serves share
// them.
func TestPromptIdempotencyReloadPaths(t *testing.T) {
	t.Parallel()
	paths := loadPaths(t, "prompt_idempotency_reload")
	const shards = 4
	for sh := range shards {
		t.Run(fmt.Sprintf("shard%d", sh), func(t *testing.T) {
			t.Parallel()
			a := newPirAdapter(t)
			acts := pirActions(a)
			for i := sh; i < len(paths); i += shards {
				if err := walkPirPath(a, acts, paths[i]); err != nil {
					t.Errorf("path %d: %v", i, err)
				}
			}
			t.Logf("shard %d: %v", sh, a.stats)
			checkPirHistories(t, a)
		})
	}
}

// Every link: the wrong Retry shows only where a Retry follows a write
// serve made, which a walk that reaches every state need not take. The
// walk stops at the first path that catches it.
func TestPromptIdempotencyReloadPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	for _, bug := range []string{"retry", "launch"} {
		t.Run(bug, func(t *testing.T) {
			t.Parallel()
			b, err := pathsJSONCover("prompt_idempotency_reload", tracecheck.CoverTransitions)
			if err != nil {
				t.Fatal(err)
			}
			var f struct {
				Paths []genPath `json:"paths"`
			}
			if err := json.Unmarshal(b, &f); err != nil {
				t.Fatal(err)
			}
			a := newPirAdapter(t)
			a.retryNewID, a.launchNewID = bug == "retry", bug == "launch"
			acts := pirActions(a)
			for i, p := range f.Paths {
				if !pirRetriesAfterWrite(p, bug) {
					continue
				}
				if err := walkPirPath(a, acts, p); err != nil {
					t.Logf("path %d caught it: %v", i, err)
					return
				}
			}
			t.Fatalf("every path passed with a %s that sends a new request id; the walk is not checking state", bug)
		})
	}
}

// pirRetriesAfterWrite picks the paths the wrong adapter can show on:
// ones that retry after serve handled the request, so the walk does not
// spend minutes on paths that cannot catch it.
func pirRetriesAfterWrite(p genPath, bug string) bool {
	write, retry := "Session#0.ServerWrites", "Session#0.Retry"
	if bug == "launch" {
		write, retry = "Session#0.ServerMakes", "Session#0.RetryLaunch"
	}
	wrote := false
	for _, s := range p.Trace {
		switch s.Action {
		case write:
			wrote = true
		case retry:
			if wrote {
				return true
			}
		}
	}
	return false
}

// walkPirPath runs one generated path from Init and returns the first
// step that errs, is refused by the adapter's own require, or leaves a
// state other than the path's. A step the server took another branch on
// ends the path quietly.
func walkPirPath(a *pirAdapter, acts map[string]map[string]fmbt.ActionFunc, p genPath) (err error) {
	defer func() {
		if cerr := a.Cleanup(); err == nil && cerr != nil {
			err = fmt.Errorf("cleanup: %w", cerr)
		}
	}()
	if err := a.Init(); err != nil {
		return fmt.Errorf("Init: %w", err)
	}
	var names []string
	for i, st := range p.Trace {
		want := roleState(st.State)
		if i > 0 {
			name := strings.TrimPrefix(st.Action, "Session#0.")
			names = append(names, name)
			f := acts["Session"][name]
			if f == nil {
				f = acts[""][name]
			}
			if f == nil {
				return fmt.Errorf("no adapter action for %q", st.Action)
			}
			_, err := f(a, nil)
			if errors.Is(err, fmbt.ErrNotImplemented) {
				return nil
			}
			if err != nil {
				return fmt.Errorf("%v: %w", names, err)
			}
			if a.gate.off {
				return fmt.Errorf("%v: the adapter refused a step the spec enables (its require disagrees)", names)
			}
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("%v: %w", names, err)
		}
		if !reflect.DeepEqual(jsonRound(got), jsonRound(want)) {
			return fmt.Errorf("%v: state\n got %v\nwant %v", names, jsonRound(got), jsonRound(want))
		}
	}
	return nil
}

func checkPirHistories(t *testing.T, a *pirAdapter) {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "prompt_idempotency_reload"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		if es := sessionHistoryOrNil(t, a.s.Home, id); es != nil {
			checkHistory(t, g, es, pirHistory)
		}
	}
}

// sessionHistoryOrNil is sessionHistory for a session that may never
// have written a file (a walk that only launched).
func sessionHistoryOrNil(t *testing.T, home, id string) []history.Entry {
	t.Helper()
	if _, err := os.Stat(filepath.Join(home, ".bough", "history", id+".jsonl")); errors.Is(err, os.ErrNotExist) {
		return nil
	}
	return sessionHistory(t, home, id)
}
