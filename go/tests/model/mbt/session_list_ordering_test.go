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
	"reflect"
	"regexp"
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

// specs/session_list_ordering.fizz against a real serve: where A and B
// land in GET /api/sessions, and where the page's rules put them, as
// turns, failures, acks, bookkeeping writes and 72 h move them.
//
// The scenery is built once per test and shared by every walk: folder w
// holds R1 and R2, each held running on a "block" turn for the whole
// test. Each walk's Init archives the last walk's A and B (archiving
// ends their processes and takes them off the list) and creates new
// ones in w, B first, so A has the larger id, with a clean turn in B,
// then one in A. Reusing one pair made their transcripts grow with every
// walk until the exhaustive run spent its time re-reading them. Q is a child of P in project p (fake container runtime).
// P is a history file with no process, as in background_agents: a live
// parent takes every report as a wake turn, and its model call would
// race everyone's for llm-control's one queue. The child queue is held
// full by a blocker, a child of P held on a block turn: Spawn asks with
// maxRunning 1, so Q queues behind it; Start releases the blocker, Q
// takes its slot on its own held turn and is the next walk's blocker.
//
// The spec's page fields (pinned, grouped, tucked, sidebar) are the
// page's rules (app.tsx Sidebar, status.tsx) applied to the rows the
// list answers; unfolded is the adapter's own, since it is the page.
//
// What the server cannot be asked to do is done to its files, the way
// its own writers do it:
//   - TitleWritten appends a "title" entry with history.AppendFile, as
//     `bough summarize` and serve's notices write a file another
//     process may hold.
//   - ClockPasses72h moves the session's past instead of the clock:
//     every entry's time and the file's mtime go back until the newest
//     is 72 h and a minute old. Times move by whole seconds, so each
//     line keeps its length and the live child's offset stays true.
//   - TwoSessionsSameMillisecond runs a turn in each, then gives both
//     turns' done the same instant (a whole second, so every fraction
//     becomes zeros of its own length) and both files the same mtime.
const sloConfig = controlConfig + "- id: orb\n  plugin: orb\n  config:\n    runtime: fake\n"

type sloAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	w, parent, slug string
	r1, r2, a, b    string
	blocker, held   string // the child holding the queue full, and its turn
	q, qTurn        string // this walk's Q ("" before Spawn) and its turn
	qs              []string
	pairs           []string // every A and B, for the trace check

	unfolded bool
	lastTurn string
	turn     int

	// titleAsNews is the deliberate bug the wrong-adapter tests inject:
	// TitleWritten appends a notice, which is news, not bookkeeping.
	titleAsNews bool
}

func newSLOAdapter(t *testing.T) *sloAdapter {
	s := servetest.Start(t, servetest.Options{Config: sloConfig})
	a := &sloAdapter{t: t, s: s, dir: control.Dir(s.Home), w: s.Dir(t, "w")}
	t.Cleanup(func() {
		for _, name := range []string{"r1", "r2", a.held, a.qTurn} {
			if name != "" {
				os.WriteFile(filepath.Join(a.dir, name+".release"), nil, 0o644)
			}
		}
	})
	if err := a.setup(); err != nil {
		t.Fatalf("setup: %v", err)
	}
	return a
}

// setup builds what every walk shares: R1 and R2 held running in w, the
// processless parent P, project p and the first blocker.
func (a *sloAdapter) setup() error {
	ctx, cancel := actionCtx()
	defer cancel()
	for i, id := range []*string{&a.r1, &a.r2} {
		name := fmt.Sprintf("r%d", i+1)
		control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "released " + name})
		row, err := a.s.CreateSession(ctx, a.w, "hold "+name)
		if err != nil {
			return err
		}
		*id = row.ID
		control.WaitTaken(a.t, a.dir, name, actionTimeout)
		row, err = waitRow(a.s, row.ID, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
		if err != nil {
			return err
		}
		// The group is keyed on the cwd serve records, which may be the
		// temp dir with its symlinks resolved.
		a.w = row.Cwd
	}
	a.parent = history.NewID()
	path := a.path(a.parent)
	if err := os.WriteFile(path, nil, 0o644); err != nil {
		return err
	}
	if _, err := history.AppendFile(path, "meta", map[string]any{"cwd": a.s.Dir(a.t, "parent")}); err != nil {
		return err
	}
	var p struct {
		Project serve.Project `json:"project"`
	}
	if err := a.api(http.MethodPost, "/api/projects", map[string]string{"name": "p"}, &p); err != nil {
		return err
	}
	a.slug = p.Project.Slug
	a.held = a.nextTurn("x")
	control.Queue(a.t, a.dir, a.held, control.Turn{Mode: "block", Text: "released " + a.held})
	id, queued, err := a.spawn("blocker")
	if err != nil {
		return err
	}
	if queued {
		return fmt.Errorf("the first blocker was queued")
	}
	a.blocker = id
	control.WaitTaken(a.t, a.dir, a.held, actionTimeout)
	return nil
}

func (a *sloAdapter) path(id string) string {
	return filepath.Join(a.s.Home, ".bough", "history", id+".jsonl")
}

func (a *sloAdapter) nextTurn(prefix string) string {
	a.turn++
	return fmt.Sprintf("%s%05d", prefix, a.turn)
}

func (a *sloAdapter) api(method, path string, body, out any) error {
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

// spawn is a child of P in project p, capped at one running child.
func (a *sloAdapter) spawn(prompt string) (string, bool, error) {
	var r struct {
		Session serve.Row `json:"session"`
		Queued  bool      `json:"queued"`
	}
	err := a.api(http.MethodPost, "/api/sessions", map[string]any{
		"spawnedBy": a.parent, "slug": a.slug, "prompt": prompt, "maxRunning": 1, "maxPerSession": 1 << 20,
	}, &r)
	return r.Session.ID, r.Queued, err
}

// Init is the spec's start: a new B, then a new A, each after a clean
// turn, the last walk's pair archived.
func (a *sloAdapter) Init() error {
	a.gate.reset()
	a.unfolded, a.q, a.qTurn = false, "", ""
	for _, id := range []*string{&a.b, &a.a} {
		ctx, cancel := actionCtx()
		defer cancel()
		if *id != "" {
			if _, err := a.s.Archive(ctx, *id); err != nil {
				return err
			}
		}
		row, err := a.s.CreateSession(ctx, a.w, "")
		if err != nil {
			return err
		}
		*id = row.ID
		a.pairs = append(a.pairs, row.ID)
	}
	if err := a.turnIn(a.b, false); err != nil {
		return err
	}
	if err := a.turnIn(a.a, false); err != nil {
		return err
	}
	a.lastTurn = "A"
	return nil
}

// Cleanup starts a Q the walk left queued, so the next walk begins with
// one child holding the slot and none waiting.
func (a *sloAdapter) Cleanup() error {
	if a.q == "" {
		return nil
	}
	row, err := a.row(a.q)
	if err != nil {
		return err
	}
	if row != nil && row.Queued {
		return a.start()
	}
	return nil
}

func (a *sloAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "List", Index: 0}: a}, nil
}

func (a *sloAdapter) list() ([]serve.Row, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	return a.s.ListSessions(ctx, false)
}

// row is id's row in the list, nil when it is not listed.
func (a *sloAdapter) row(id string) (*serve.Row, error) {
	rows, err := a.list()
	if err != nil {
		return nil, err
	}
	for i := range rows {
		if rows[i].ID == id {
			return &rows[i], nil
		}
	}
	return nil, nil
}

// The page's rules, as app.tsx and status.tsx have them.
const (
	sloInactive = 72 * time.Hour // INACTIVE_MS, STALE_FAILURE_MS
	sloFreshMin = 3              // FRESH_MIN
)

func sloHasFailure(r serve.Row, now time.Time) bool {
	if r.Trouble != "" {
		return true
	}
	recent := r.LastAt.IsZero() || now.Sub(r.LastAt) < sloInactive
	return (r.TestsFailed || r.Status == serve.StatusError) && recent
}

func sloSignal(r serve.Row, now time.Time) int {
	switch {
	case sloHasFailure(r, now) || r.Ask != nil:
		return 0
	case r.Status == serve.StatusRunning:
		return 1
	}
	return 2
}

func sloOld(r serve.Row, now time.Time) bool {
	return sloSignal(r, now) >= 2 && now.Sub(r.LastAt) >= sloInactive
}

// GetState reads the List role off one GET /api/sessions.
func (a *sloAdapter) GetState() (map[string]any, error) {
	rows, err := a.list()
	if err != nil {
		return nil, err
	}
	now := time.Now()
	byID := map[string]serve.Row{}
	var order []string // A, B and Q as the server lists them
	for _, r := range rows {
		byID[r.ID] = r
		switch r.ID {
		case a.a:
			order = append(order, "A")
		case a.b:
			order = append(order, "B")
		case a.q:
			if a.q != "" {
				order = append(order, "Q")
			}
		}
	}
	ra, okA := byID[a.a]
	rb, okB := byID[a.b]
	if !okA || !okB {
		return nil, fmt.Errorf("A or B is not listed: %v", order)
	}
	st := map[string]any{
		"last_turn":   a.lastTurn,
		"last_active": sloLastActive(ra, rb),
		"a":           sloMark(ra),
		"b":           sloMark(rb),
		"a_old":       now.Sub(ra.LastAt) >= sloInactive,
		"b_old":       now.Sub(rb.LastAt) >= sloInactive,
		"unfolded":    a.unfolded,
		"first":       order[0],
		"q":           "none",
	}
	server := ""
	for _, x := range order {
		if x != "Q" {
			server += x
		}
	}
	st["server"] = server
	if a.q != "" {
		r, ok := byID[a.q]
		switch {
		case !ok:
			return nil, fmt.Errorf("Q %s is not listed", a.q)
		case r.Queued:
			st["q"] = "queued"
		default:
			if _, err := os.Stat(a.path(a.q)); err != nil {
				return nil, fmt.Errorf("Q %s is neither queued nor written: %v", a.q, err)
			}
			st["q"] = "started"
		}
	}

	// Needs you: every row that wants a person, in the server's order.
	var needs []string
	pinned := map[string]bool{}
	for _, r := range rows {
		if sloSignal(r, now) == 0 {
			pinned[r.ID] = true
			needs = append(needs, r.ID)
		}
	}
	// w's group: its rows by urgency (a stable sort over the server's
	// order), the pinned ones out, then the 72 h fold with FRESH_MIN.
	var group []serve.Row
	for _, r := range rows {
		if r.Cwd == a.w && r.Project == "" && !r.Background && !(r.Empty && !r.Live) && !pinned[r.ID] {
			group = append(group, r)
		}
	}
	slices.SortStableFunc(group, func(x, y serve.Row) int {
		if d := sloSignal(x, now) - sloSignal(y, now); d != 0 {
			return d
		}
		return y.LastAt.Truncate(time.Millisecond).Compare(x.LastAt.Truncate(time.Millisecond))
	})
	var fresh, older []serve.Row
	for _, r := range group {
		if sloOld(r, now) || (sloSignal(r, now) >= 2 && r.Empty && !r.Live) {
			older = append(older, r)
		} else {
			fresh = append(fresh, r)
		}
	}
	for len(fresh) < sloFreshMin {
		i := slices.IndexFunc(older, func(r serve.Row) bool { return !r.Empty })
		if i < 0 {
			break
		}
		fresh = append(fresh, older[i])
		older = slices.Delete(older, i, i+1)
	}
	in := func(list []serve.Row, id string) bool {
		return slices.ContainsFunc(list, func(r serve.Row) bool { return r.ID == id })
	}
	st["a_pinned"], st["b_pinned"] = pinned[a.a], pinned[a.b]
	st["a_grouped"], st["b_grouped"] = in(group, a.a), in(group, a.b)
	st["a_tucked"], st["b_tucked"] = in(older, a.a), in(older, a.b)
	pick := func(ids []string) string {
		out := ""
		for _, id := range ids {
			switch id {
			case a.a:
				out += "A"
			case a.b:
				out += "B"
			}
		}
		return out
	}
	switch {
	case pinned[a.a] && pinned[a.b]:
		st["sidebar"] = pick(needs)
	case !pinned[a.a] && !pinned[a.b]:
		var ids []string
		for _, r := range append(fresh, older...) {
			ids = append(ids, r.ID)
		}
		st["sidebar"] = pick(ids)
	default:
		st["sidebar"] = ""
	}
	return st, nil
}

// sloLastActive is which of A and B the server has active last, by
// lastAt to the millisecond (the page's precision). Not by mtime: a
// bookkeeping write moves it, and the spec says nothing may.
func sloLastActive(ra, rb serve.Row) string {
	switch a, b := ra.LastAt.UnixMilli(), rb.LastAt.UnixMilli(); {
	case a > b:
		return "A"
	case a < b:
		return "B"
	}
	return "tie"
}

// sloMark is the spec's a/b: an unacked failure is "failed" (trouble), an
// acked one "seen".
func sloMark(r serve.Row) string {
	switch {
	case r.Status != serve.StatusError:
		return "done"
	case r.Trouble != "":
		return "failed"
	}
	return "seen"
}

// view is the state the gates read the spec's requires off.
func (a *sloAdapter) view() map[string]any {
	st, err := a.GetState()
	if err != nil {
		return map[string]any{}
	}
	return st
}

// turnIn runs one turn in id, clean or failing, and waits for the turn's
// bookkeeping (its turn-summary, and the title after the first) so no
// write of the child's lands after the step.
func (a *sloAdapter) turnIn(id string, fail bool) error {
	before, err := history.Read(a.path(id))
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	dones := sloCount(before, "done")
	name := a.nextTurn("t")
	turn, want := control.Turn{Mode: "ok", Text: "finished " + name}, serve.StatusDone
	if fail {
		turn, want = control.Turn{Mode: "error", Error: "model says no"}, serve.StatusError
	}
	control.Queue(a.t, a.dir, name, turn)
	letter := "A"
	if id == a.b {
		letter = "B"
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, id, letter+" turn "+name); err != nil {
		return err
	}
	control.WaitTaken(a.t, a.dir, name, actionTimeout)
	if _, err := waitRow(a.s, id, string(want), func(r serve.Row) bool { return r.Status == want }); err != nil {
		return err
	}
	return a.settled(id, dones+1)
}

func sloCount(entries []history.Entry, kind string) int {
	n := 0
	for _, e := range entries {
		if e.Kind == kind {
			n++
		}
	}
	return n
}

// settled waits until id has closed turns turns and the title plugin has
// logged the last of them and named the session.
func (a *sloAdapter) settled(id string, turns int) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		entries, err := history.Read(a.path(id))
		if err == nil && sloCount(entries, "done") >= turns && sloLogged(entries) >= turns && sloFinal(entries) {
			return nil
		}
		if time.Now().After(deadline) {
			var kinds []string
			for _, e := range entries {
				kinds = append(kinds, e.Kind)
			}
			return fmt.Errorf("waiting for %s's turn %d to be logged: %v (%v)", id, turns, kinds, err)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func sloLogged(entries []history.Entry) int {
	n := 0
	for _, e := range entries {
		if t, ok := e.Data["turn"].(float64); ok && e.Kind == "turn-summary" && int(t) > n {
			n = int(t)
		}
	}
	return n
}

func sloFinal(entries []history.Entry) bool {
	return slices.ContainsFunc(entries, func(e history.Entry) bool { return e.Kind == "title" && e.Data["final"] == true })
}

func (a *sloAdapter) TurnDoneA() error { return a.turnStep(a.a, "A", false) }
func (a *sloAdapter) TurnDoneB() error { return a.turnStep(a.b, "B", false) }
func (a *sloAdapter) FailA() error     { return a.turnStep(a.a, "A", true) }
func (a *sloAdapter) FailB() error     { return a.turnStep(a.b, "B", true) }

func (a *sloAdapter) turnStep(id, letter string, fail bool) error {
	if !a.gate.pass(true) {
		return nil
	}
	if err := a.turnIn(id, fail); err != nil {
		return err
	}
	a.lastTurn = letter
	return nil
}

// AckA and AckB are the Seen button: POST /ack.
func (a *sloAdapter) AckA() error { return a.ackStep(a.a, "a") }
func (a *sloAdapter) AckB() error { return a.ackStep(a.b, "b") }

func (a *sloAdapter) ackStep(id, field string) error {
	if !a.gate.pass(a.view()[field] == "failed") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Ack(ctx, id)
	return err
}

func (a *sloAdapter) TwoSessionsSameMillisecond() error {
	if !a.gate.pass(true) {
		return nil
	}
	if err := a.turnIn(a.a, false); err != nil {
		return err
	}
	if err := a.turnIn(a.b, false); err != nil {
		return err
	}
	// The next whole second: after everything either wrote, a started Q
	// included, so the pair stays where the turns put it. The step waits
	// it out, so a later write is later than the tie.
	at := time.Now().Truncate(time.Second).Add(time.Second)
	defer time.Sleep(time.Until(at) + 10*time.Millisecond)
	for _, id := range []string{a.a, a.b} {
		if err := a.rewrite(id, func(lines []sloLine) {
			for i := len(lines) - 1; i >= 0; i-- {
				if lines[i].kind == "done" {
					lines[i].at = at
					return
				}
			}
		}, at); err != nil {
			return err
		}
	}
	a.lastTurn = "tie"
	return nil
}

// TitleWrittenA and TitleWrittenB append a title entry the way a writer
// outside the session's process does (`bough summarize`).
func (a *sloAdapter) TitleWrittenA() error { return a.titleStep(a.a) }
func (a *sloAdapter) TitleWrittenB() error { return a.titleStep(a.b) }

func (a *sloAdapter) titleStep(id string) error {
	if !a.gate.pass(true) {
		return nil
	}
	if a.titleAsNews {
		_, err := history.AppendFile(a.path(id), "notice", map[string]any{"id": history.NewID(), "to": id, "text": "control"})
		return err
	}
	entries, err := history.Read(a.path(id))
	if err != nil {
		return err
	}
	_, err = history.AppendFile(a.path(id), "title", map[string]any{"text": "control", "turn": sloLogged(entries), "final": true})
	return err
}

// ClockPasses72h ages every row that is not old yet (A, B, and a Q that
// has started) by one shift, so their order among themselves holds, and
// by just enough that the newest of them is 72 h and a minute old.
//
// A row aged by an earlier step stays where it is (moving it again could
// take a failure past serve's 7-day trouble window, beyond the spec's
// horizon), so the rows aged now must still land after it: whatever is
// fresh now was written after that step, and the step waits until a
// second has passed since the newest write, which is the most the
// whole-second shift can overshoot.
func (a *sloAdapter) ClockPasses72h() error {
	v := a.view()
	if !a.gate.pass(!(v["a_old"] == true && v["b_old"] == true)) {
		return nil
	}
	ids := []string{a.a, a.b}
	if v["q"] == "started" {
		ids = append(ids, a.q)
	}
	newest := map[string]time.Time{}
	var top time.Time
	for _, id := range ids {
		entries, err := history.Read(a.path(id))
		if err != nil {
			return err
		}
		for _, e := range entries {
			if e.At.After(newest[id]) {
				newest[id] = e.At
			}
		}
		if time.Since(newest[id]) >= sloInactive {
			delete(newest, id)
		} else if newest[id].After(top) {
			top = newest[id]
		}
	}
	if wait := time.Second - time.Since(top); wait > 0 {
		time.Sleep(wait)
	}
	shift := top.Sub(time.Now().Add(-sloInactive-time.Minute)).Truncate(time.Second) + time.Second
	for id := range newest {
		st, err := os.Stat(a.path(id))
		if err != nil {
			return err
		}
		if err := a.rewrite(id, func(lines []sloLine) {
			for i := range lines {
				lines[i].at = lines[i].at.Add(-shift)
			}
		}, st.ModTime().Add(-shift)); err != nil {
			return err
		}
	}
	return nil
}

// sloLine is one line of a transcript with its time pulled out.
type sloLine struct {
	raw  []byte
	kind string
	at   time.Time
	loc  []int // where the time's text is in raw
}

var sloAt = regexp.MustCompile(`^\{"seq":\d+,"at":"([^"]+)","kind":"([^"]+)"`)

// rewrite edits the entries' times in place under the history lock,
// keeping each line's length, and sets the file's mtime to mtime (when
// not zero). A time changed by whole seconds keeps its fraction, and a
// whole second written into a fraction of k digits is k zeros, so the
// file never changes size under the child that holds it.
func (a *sloAdapter) rewrite(id string, edit func([]sloLine), mtime time.Time) error {
	f, err := os.OpenFile(a.path(id), os.O_RDWR, 0)
	if err != nil {
		return err
	}
	defer f.Close()
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX); err != nil {
		return err
	}
	defer syscall.Flock(int(f.Fd()), syscall.LOCK_UN)
	raw, err := io.ReadAll(f)
	if err != nil {
		return err
	}
	var lines []sloLine
	for _, l := range bytes.SplitAfter(raw, []byte("\n")) {
		line := sloLine{raw: l}
		if m := sloAt.FindSubmatchIndex(l); m != nil {
			line.loc = m[2:4]
			line.kind = string(l[m[4]:m[5]])
			if line.at, err = time.Parse(time.RFC3339Nano, string(l[m[2]:m[3]])); err != nil {
				return err
			}
		}
		lines = append(lines, line)
	}
	edit(lines)
	var out []byte
	for _, l := range lines {
		if l.loc == nil {
			out = append(out, l.raw...)
			continue
		}
		old := string(l.raw[l.loc[0]:l.loc[1]])
		text, err := sloSameWidth(old, l.at)
		if err != nil {
			return fmt.Errorf("%s: %w", id, err)
		}
		out = append(out, l.raw[:l.loc[0]]...)
		out = append(out, text...)
		out = append(out, l.raw[l.loc[1]:]...)
	}
	if len(out) != len(raw) {
		return fmt.Errorf("%s: rewrite changed the file's size", id)
	}
	if _, err := f.WriteAt(out, 0); err != nil {
		return err
	}
	if !mtime.IsZero() {
		return os.Chtimes(a.path(id), time.Time{}, mtime)
	}
	return nil
}

// sloSameWidth formats t in old's zone with old's fraction width.
func sloSameWidth(old string, t time.Time) (string, error) {
	was, err := time.Parse(time.RFC3339Nano, old)
	if err != nil {
		return "", err
	}
	t = t.In(was.Location())
	digits := 0
	if i := strings.IndexByte(old, '.'); i >= 0 {
		j := i + 1
		for j < len(old) && old[j] >= '0' && old[j] <= '9' {
			j++
		}
		digits = j - i - 1
	}
	layout := "2006-01-02T15:04:05"
	if digits > 0 {
		layout += "." + strings.Repeat("0", digits)
	}
	layout += "Z07:00"
	s := t.Format(layout)
	if back, err := time.Parse(time.RFC3339Nano, s); err != nil || !back.Equal(t) {
		return "", fmt.Errorf("%s cannot be written in the width of %s", t.Format(time.RFC3339Nano), old)
	}
	if len(s) != len(old) {
		return "", fmt.Errorf("%s is not as wide as %s", s, old)
	}
	return s, nil
}

// Unfold and Fold are w's "N older" line: the page's own state.
func (a *sloAdapter) Unfold() error {
	v := a.view()
	if a.gate.pass((v["a_tucked"] == true || v["b_tucked"] == true) && !a.unfolded) {
		a.unfolded = true
	}
	return nil
}

func (a *sloAdapter) Fold() error {
	v := a.view()
	if a.gate.pass((v["a_tucked"] == true || v["b_tucked"] == true) && a.unfolded) {
		a.unfolded = false
	}
	return nil
}

// Spawn asks for Q while the blocker holds the one slot.
func (a *sloAdapter) Spawn() error {
	if !a.gate.pass(a.q == "") {
		return nil
	}
	id, queued, err := a.spawn("task " + a.nextTurn("q"))
	if err != nil {
		return err
	}
	a.q = id
	a.qs = append(a.qs, id)
	if !queued {
		return fmt.Errorf("Q was started at once: the blocker does not hold the queue full")
	}
	return nil
}

func (a *sloAdapter) QueuedRowPoll() error {
	a.gate.pass(a.view()["q"] == "queued")
	return nil
}

// Start frees the slot: the blocker's turn ends, Q starts on a held
// turn of its own and is the blocker from now on.
func (a *sloAdapter) Start() error {
	if !a.gate.pass(a.view()["q"] == "queued") {
		return nil
	}
	return a.start()
}

func (a *sloAdapter) start() error {
	a.qTurn = a.nextTurn("y")
	control.Queue(a.t, a.dir, a.qTurn, control.Turn{Mode: "block", Text: "released " + a.qTurn})
	control.Release(a.t, a.dir, a.held)
	old := a.blocker
	control.WaitTaken(a.t, a.dir, a.qTurn, actionTimeout)
	a.blocker, a.held, a.qTurn = a.q, a.qTurn, ""
	deadline := time.Now().Add(actionTimeout)
	for {
		if _, err := os.Stat(a.path(a.q)); err == nil {
			break
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("Q %s took its turn but wrote no history", a.q)
		}
		time.Sleep(20 * time.Millisecond)
	}
	// The old blocker's process is done with: end it.
	if _, err := waitRow(a.s, old, "the old blocker to finish", func(r serve.Row) bool { return r.Status != serve.StatusRunning }); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Archive(ctx, old)
	return err
}

func (a *sloAdapter) Poll() error {
	a.gate.pass(true)
	return nil
}

var sloActions = map[string]map[string]fmbt.ActionFunc{"List": {
	"TurnDoneA":                  action((*sloAdapter).TurnDoneA),
	"TurnDoneB":                  action((*sloAdapter).TurnDoneB),
	"FailA":                      action((*sloAdapter).FailA),
	"FailB":                      action((*sloAdapter).FailB),
	"AckA":                       action((*sloAdapter).AckA),
	"AckB":                       action((*sloAdapter).AckB),
	"TwoSessionsSameMillisecond": action((*sloAdapter).TwoSessionsSameMillisecond),
	"TitleWrittenA":              action((*sloAdapter).TitleWrittenA),
	"TitleWrittenB":              action((*sloAdapter).TitleWrittenB),
	"ClockPasses72h":             action((*sloAdapter).ClockPasses72h),
	"Unfold":                     action((*sloAdapter).Unfold),
	"Fold":                       action((*sloAdapter).Fold),
	"Spawn":                      action((*sloAdapter).Spawn),
	"QueuedRowPoll":              action((*sloAdapter).QueuedRowPoll),
	"Start":                      action((*sloAdapter).Start),
	"Poll":                       action((*sloAdapter).Poll),
}, "": {
	// deadlock_detection is off, so the runner offers a role-less "end"
	// (see unseen_trouble_ack): decline it and close the gate.
	"end": func(m any, _ []fmbt.Arg) (any, error) {
		m.(*sloAdapter).gate.pass(false)
		return nil, errDisabled
	},
}}

func sloOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

// sloHistory reads A's or B's transcript (the first input names which)
// as that session's own steps: its first turn is part of Init, every
// later turn is TurnDone or Fail, and a title or turn-summary is
// TitleWritten. Acks live in meta.json, and the other session's steps
// are not in this file, so the check is on the session's own mark.
func sloHistory(entries []history.Entry) []tracecheck.Step {
	letter := ""
	for _, e := range entries {
		if e.Kind == "input" {
			text, _ := e.Data["text"].(string)
			letter, _, _ = strings.Cut(text, " ")
			break
		}
	}
	if letter != "A" && letter != "B" {
		return nil
	}
	field := "List#0." + strings.ToLower(letter)
	mark := func(m string) map[string]any { return map[string]any{field: m} }
	steps := []tracecheck.Step{{Action: "Init", State: mark("done")}}
	turns, failed, cur := 0, false, "done"
	for _, e := range entries {
		switch e.Kind {
		case "input":
			failed = false
		case "error":
			failed = true
		case "done":
			turns++
			cur = "done"
			action := "TurnDone" + letter
			if failed {
				cur, action = "failed", "Fail"+letter
			}
			if turns == 1 {
				// Init's own turn: the spec starts on a clean one.
				steps[0].State = mark(cur)
				continue
			}
			steps = append(steps, tracecheck.Step{Action: "List#0." + action, State: mark(cur)})
		case "title", "turn-summary":
			if turns > 0 {
				steps = append(steps, tracecheck.Step{Action: "List#0.TitleWritten" + letter, State: mark(cur)})
			}
		}
	}
	return steps
}

func init() { historyProjections["session_list_ordering"] = sloHistory }

func (a *sloAdapter) checkHistories(t *testing.T, g *tracecheck.Graph) {
	t.Helper()
	for _, id := range a.pairs {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), sloHistory)
	}
}

func TestSessionListOrdering(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	if envCover() != tracecheck.CoverTransitions {
		t.Skip("fizzbee-mbt random runs are part of the exhaustive run: MODEL_COVER=transitions")
	}
	a := newSLOAdapter(t)
	if err := runMBT(t, "session_list_ordering", a, sloActions, sloOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "session_list_ordering"))
	if err != nil {
		t.Fatal(err)
	}
	a.checkHistories(t, g)
}

func TestSessionListOrderingCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	if envCover() != tracecheck.CoverTransitions {
		t.Skip("fizzbee-mbt random runs are part of the exhaustive run: MODEL_COVER=transitions")
	}
	a := newSLOAdapter(t)
	a.titleAsNews = true
	if err := runMBT(t, "session_list_ordering", a, sloActions, sloOptions()); err == nil {
		t.Fatal("a run whose TitleWritten writes news passed; the runner is not checking state")
	}
}

type sloPath struct {
	Links []int `json:"links"`
	Trace []struct {
		Action string         `json:"action"`
		State  map[string]any `json:"state"`
	} `json:"trace"`
}

func loadSLOPaths(t *testing.T, cover tracecheck.Cover) []sloPath {
	t.Helper()
	b, err := pathsJSONCover("session_list_ordering", cover)
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Paths []sloPath `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		t.Fatal(err)
	}
	return f.Paths
}

// walkPath takes one path through the adapter and compares the state
// after every step with the spec's; it returns the links it checked.
func (a *sloAdapter) walkPath(p sloPath) (checked []int, err error) {
	if err := a.Init(); err != nil {
		return nil, err
	}
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	for i, step := range p.Trace {
		if i > 0 {
			name := strings.TrimPrefix(step.Action, "List#0.")
			if _, err := sloActions["List"][name](a, nil); err != nil {
				return checked, fmt.Errorf("step %d (%s): %w", i, name, err)
			}
			if a.gate.off {
				return checked, fmt.Errorf("step %d (%s): the adapter says the spec's require does not hold", i, name)
			}
		}
		got, err := a.GetState()
		if err != nil {
			return checked, fmt.Errorf("step %d (%s): %w", i, step.Action, err)
		}
		want := map[string]any{}
		for k, v := range step.State {
			if f, ok := strings.CutPrefix(k, "List#0."); ok {
				want[f] = v
			}
		}
		if !reflect.DeepEqual(got, want) {
			return checked, fmt.Errorf("step %d (%s): state\n%s", i, step.Action, sloDiff(got, want))
		}
		if i > 0 && i <= len(p.Links) {
			checked = append(checked, p.Links[i-1])
		}
	}
	return checked, nil
}

func sloDiff(got, want map[string]any) string {
	var keys []string
	for k := range want {
		keys = append(keys, k)
	}
	for k := range got {
		if _, ok := want[k]; !ok {
			keys = append(keys, k)
		}
	}
	slices.Sort(keys)
	var b strings.Builder
	for _, k := range keys {
		if !reflect.DeepEqual(got[k], want[k]) {
			fmt.Fprintf(&b, "  %s: got %v, want %v\n", k, got[k], want[k])
		}
	}
	return b.String()
}

// Every walk over the checked-in graph through a real serve: every
// settled state by default, every link under MODEL_COVER=transitions.
func TestSessionListOrderingPaths(t *testing.T) {
	t.Parallel()
	a := newSLOAdapter(t)
	covered := map[int]bool{}
	paths := loadSLOPaths(t, envCover())
	start := time.Now()
	for i, p := range paths {
		if i > 0 && i%50 == 0 {
			t.Logf("%d of %d paths in %v", i, len(paths), time.Since(start).Round(time.Second))
		}
		checked, err := a.walkPath(p)
		if err != nil {
			t.Errorf("path %d: %v", i, err)
		}
		for _, l := range checked {
			covered[l] = true
		}
	}
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "session_list_ordering"))
	if err != nil {
		t.Fatal(err)
	}
	a.checkHistories(t, g)
	t.Logf("paths: %d walked, %d links checked, %d in the graph", len(paths), len(covered), len(g.Links))
	if envCover() == tracecheck.CoverTransitions && len(covered) != len(g.Links) {
		t.Errorf("%d of %d links checked", len(covered), len(g.Links))
	}
}

// The paths test is only worth its time if a wrong adapter fails it:
// this one's TitleWritten writes news, which moves the row. TitleWritten
// is a self-loop, which a walk to every state never takes, so this one
// walks the links; it stops at the first failure.
func TestSessionListOrderingPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newSLOAdapter(t)
	a.titleAsNews = true
	for _, p := range loadSLOPaths(t, tracecheck.CoverTransitions) {
		if _, err := a.walkPath(p); err != nil {
			return
		}
	}
	t.Fatal("every path passed with TitleWritten writing news; the walk is not checking state")
}
