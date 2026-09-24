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
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/create_races_failures.fizz against a real serve. The adapter is
// the page (palette, route, toast, StartingStatus, the Sending previews
// are its own state, kept by the page's rules); the server's side of
// each press is read off the serve, its answers and the filesystem: the
// press's history files (sa, sb) and whether any child a press spawned
// is alive when nothing the page waits on or shows owns it (stray).
//
// Each press's POST runs in the background and carries an id the page
// minted for the press, as the page sends it: a retry of the same press
// sends it again. llm-control's hold_boot parks every new child before
// its history file, so the adapter decides when the file lands
// (WriteHistory), whether the child dies first (ChildExitsEarly) or
// never gets there (CreateTimeout, on serve's real 10 s timeout),
// and whether serve's write of the first prompt fails (PromptWriteFails:
// the child drops its stdin as it is released).
//
// Where the product cannot be put in the spec's state, the step is the
// nearest thing it can be put in, and says so:
//   - HistoryAfterDeadline is the file landing between serve's last Stat
//     and its kill, a window no test can aim at; it is driven as the
//     timeout, so what it checks is the outcome the spec requires of
//     that race (a 500, no file, no child).
//   - Both creates wait serve's one timeout, in real time, from their
//     own spawns: once A has timed out, B has only what separated the two
//     presses left. When the walk times A out while B still has to write
//     its file, the page presses B at least createRacesGap after A, as a
//     person would, so that B's file can land in the rest.
//   - ServeRestart is SIGTERM and a fresh serve on the same HOME; a 201
//     the old serve sent for a create the page had not read yet is
//     dropped, which is the page losing it to the restart.

// The llm row holds every fresh session at boot until the adapter lets
// it go.
const createRacesConfig = "- id: llm\n  plugin: llm-control\n  config:\n    hold_boot: true\n"

type crPress struct {
	name   string // "A" | "B"
	st     string // the spec's none | spawned | hist | claimed | done | failed | lost
	kind   string // none | typed | new
	pageID string // what every POST of this press sends as its id
	prompt string
	cur    string            // the id the last POST's child was started under
	ids    []string          // every id a POST of this press started a child under
	resp   chan createResult // the last POST's answer, until read
	ans    *createResult     // a 201 read but not yet landed by the page
	landed string            // the session the page opened
	sendng bool              // the Sending preview (pa, pb)
	at     time.Time         // when the last POST was sent
}

const createRacesGap = 4 * time.Second

type createRacesAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's dir
	hist string
	work string
	gate gate

	a, b                     *crPress
	starting, palette, moved bool
	route, last, toast       string
	seen                     map[string]bool // every child id seen held at boot
	n                        int
	rest                     []string          // the walk's actions after this one
	transcripts              [][]history.Entry // every landed session's, for the trace check
	exitAsRelease            bool
}

func newCreateRacesAdapter(t *testing.T) *createRacesAdapter {
	s := servetest.Start(t, servetest.Options{
		Config: createRacesConfig,
	})
	return &createRacesAdapter{
		t: t, s: s, dir: control.Dir(s.Home),
		hist: filepath.Join(s.Home, ".bough", "history"),
		work: s.Dir(t, "work"),
		seen: map[string]bool{},
	}
}

func inflightSt(st string) bool { return st == "spawned" || st == "hist" || st == "claimed" }

func (a *createRacesAdapter) inflight() bool { return inflightSt(a.a.st) || inflightSt(a.b.st) }

func (a *createRacesAdapter) Init() error {
	a.a = &crPress{name: "A", st: "none", kind: "none"}
	a.b = &crPress{name: "B", st: "none", kind: "none"}
	a.starting, a.palette, a.moved = false, false, false
	a.route, a.last, a.toast = "overview", "none", "none"
	a.gate.reset()
	return nil
}

func (a *createRacesAdapter) GetState() (map[string]any, error) {
	return map[string]any{
		"a": a.a.st, "b": a.b.st, "ka": a.a.kind, "kb": a.b.kind,
		"sa": a.files(a.a), "sb": a.files(a.b), "stray": a.stray(),
		"starting": a.starting, "palette": a.palette, "route": a.route,
		"last": a.last, "moved": a.moved, "toast": a.toast,
		"pa": a.a.sendng, "pb": a.b.sendng,
	}, nil
}

// files is how many history files the press's POSTs made.
func (a *createRacesAdapter) files(p *crPress) int {
	n := 0
	for _, id := range p.ids {
		if _, err := os.Stat(filepath.Join(a.hist, id+".jsonl")); err == nil {
			n++
		}
	}
	return n
}

// stray is a live child a press started that the page is not waiting
// on (its POST in flight, or a 201 read) and did not land.
func (a *createRacesAdapter) stray() bool {
	for _, p := range []*crPress{a.a, a.b} {
		for _, id := range p.ids {
			if (inflightSt(p.st) && id == p.cur) || id == p.landed {
				continue
			}
			if pid := control.BootPID(a.dir, id); pid != 0 && alive(pid) {
				return true
			}
		}
	}
	return false
}

// post is the page's POST /api/sessions for press p, left in flight.
// It returns once the POST has either started a child (parked at boot)
// or answered: a retry of a press whose session exists is answered at
// once, and the press is then "claimed" (its 201 read, not landed).
func (a *createRacesAdapter) post(p *crPress) error {
	// A retry sends the press's id again, and a child started under it
	// is held at boot under it again.
	if a.seen[p.pageID] {
		control.ForgetBoot(a.t, a.dir, p.pageID)
		delete(a.seen, p.pageID)
	}
	p.resp = make(chan createResult, 1)
	p.cur, p.at = "", time.Now()
	go func(resp chan createResult, body map[string]string) {
		ctx, cancel := context.WithTimeout(context.Background(), actionTimeout)
		defer cancel()
		row, err := a.create(ctx, body)
		resp <- createResult{row, err}
	}(p.resp, map[string]string{"cwd": a.work, "prompt": p.prompt, "id": p.pageID})
	p.st = "spawned"
	deadline := time.Now().Add(actionTimeout)
	for {
		for id := range control.Booting(a.dir) {
			if !a.seen[id] {
				a.seen[id] = true
				p.cur = id
				p.ids = appendNew(p.ids, id)
				return nil
			}
		}
		select {
		case r := <-p.resp:
			// Answered without a child: the page reads it as it comes.
			p.resp = nil
			if r.err != nil {
				return a.failed(p, r.err)
			}
			p.st, p.ans = "claimed", &r
			p.ids = appendNew(p.ids, r.row.ID)
			return nil
		default:
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("press %s: the POST neither started a child nor answered", p.name)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func appendNew(ids []string, id string) []string {
	for _, x := range ids {
		if x == id {
			return ids
		}
	}
	return append(ids, id)
}

// create is POST /api/sessions with the body the page sends.
func (a *createRacesAdapter) create(ctx context.Context, body map[string]string) (serve.Row, error) {
	b, _ := json.Marshal(body)
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.s.URL+"/api/sessions", bytes.NewReader(b))
	if err != nil {
		return serve.Row{}, err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return serve.Row{}, err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return serve.Row{}, err
	}
	if resp.StatusCode/100 != 2 {
		return serve.Row{}, &servetest.APIError{Status: resp.StatusCode, Msg: strings.TrimSpace(string(raw))}
	}
	var r struct {
		Session serve.Row `json:"session"`
	}
	if err := json.Unmarshal(raw, &r); err != nil {
		return serve.Row{}, fmt.Errorf("create: %w: %s", err, raw)
	}
	return r.Session, nil
}

// answer waits for press p's POST to answer.
func (a *createRacesAdapter) answer(p *crPress) (createResult, error) {
	select {
	case r := <-p.resp:
		p.resp = nil
		return r, nil
	case <-time.After(actionTimeout):
		return createResult{}, fmt.Errorf("press %s: the POST did not answer", p.name)
	}
}

// settle is the page reading press p's answer as the spec's fair steps
// do: a 201 is "claimed" until the page lands it, anything else is the
// toast.
func (a *createRacesAdapter) settle(p *crPress) error {
	r, err := a.answer(p)
	if err != nil {
		return err
	}
	if r.err != nil {
		return a.failed(p, r.err)
	}
	p.st, p.ans = "claimed", &r
	p.ids = appendNew(p.ids, r.row.ID)
	return nil
}

// failed is the page's "Couldn't start a session" toast for press p. A
// 5xx and a dropped connection are the same to it; anything else (a
// 4xx) is the page sending the wrong request.
func (a *createRacesAdapter) failed(p *crPress, err error) error {
	var ae *servetest.APIError
	if errors.As(err, &ae) && ae.Status/100 != 5 {
		return fmt.Errorf("press %s: %w", p.name, err)
	}
	p.st, a.toast = "failed", p.name
	a.starting = inflightSt(a.other(p).st)
	return nil
}

func (a *createRacesAdapter) other(p *crPress) *crPress {
	if p == a.a {
		return a.b
	}
	return a.a
}

func (a *createRacesAdapter) press(p *crPress, kind string) error {
	a.n++
	p.kind, p.pageID, p.prompt = kind, history.NewID(), ""
	if kind == "typed" {
		p.prompt = fmt.Sprintf("first prompt %04d", a.n)
	}
	a.moved, a.starting, a.last = false, true, p.name
	if p == a.b && a.a.st == "spawned" && a.timesOutAFirst() {
		time.Sleep(time.Until(a.a.at.Add(createRacesGap)))
	}
	return a.post(p)
}

// timesOutAFirst says whether the walk times A out before B, pressed
// now, is done waiting for its file (see the header).
func (a *createRacesAdapter) timesOutAFirst() bool {
	for _, s := range a.rest {
		switch s {
		case "CreateTimeoutA", "HistoryAfterDeadlineA":
			return true
		case "WriteHistoryB", "CreateTimeoutB", "ServeRestart", "Init":
			return false
		}
	}
	return false
}

// ---- the person ----

func (a *createRacesAdapter) OpenPalette() error {
	if a.gate.pass(!a.palette && (a.a.st == "none" || a.inflight())) {
		a.palette = true
		if a.inflight() {
			a.moved = true
		}
	}
	return nil
}

func (a *createRacesAdapter) Escape() error {
	if a.gate.pass(a.palette) {
		a.palette = false
	}
	return nil
}

func (a *createRacesAdapter) Navigate() error {
	if a.gate.pass(!a.palette && a.route != "chosen" && a.inflight()) {
		a.route, a.moved = "chosen", true
	}
	return nil
}

func (a *createRacesAdapter) StartTyped() error {
	if !a.gate.pass(a.palette && (a.a.st == "none" || (a.b.st == "none" && inflightSt(a.a.st)))) {
		return nil
	}
	a.palette = false
	if a.a.st == "none" {
		return a.press(a.a, "typed")
	}
	return a.press(a.b, "typed")
}

func (a *createRacesAdapter) OverviewNew() error {
	if !a.gate.pass(a.route == "overview" && !a.palette && a.a.st == "none" && !inflightSt(a.b.st)) {
		return nil
	}
	return a.press(a.a, "new")
}

func (a *createRacesAdapter) DismissToast() error {
	if a.gate.pass(a.toast != "none") {
		a.toast = "none"
	}
	return nil
}

func (a *createRacesAdapter) RetryA() error { return a.retry(a.a) }
func (a *createRacesAdapter) RetryB() error { return a.retry(a.b) }

// retry is the toast's Retry: the same press, POSTed again with the
// same id.
func (a *createRacesAdapter) retry(p *crPress) error {
	if !a.gate.pass(a.toast == p.name && !a.inflight()) {
		return nil
	}
	a.toast, a.last, a.moved, a.starting = "none", p.name, false, true
	return a.post(p)
}

// Reset ends the round: the sessions it made stay on the list, and are
// archived so their children do not pile up over a hundred walks.
func (a *createRacesAdapter) Reset() error {
	if !a.gate.pass(!a.inflight() && a.toast == "none" && a.a.st != "none" && !a.a.sendng && !a.b.sendng) {
		return nil
	}
	if err := a.archiveRound(); err != nil {
		return err
	}
	g := a.gate
	a.Init()
	a.gate = g
	return nil
}

// ---- a press's create ----

func (a *createRacesAdapter) WriteHistoryA() error { return a.writeHistory(a.a) }
func (a *createRacesAdapter) WriteHistoryB() error { return a.writeHistory(a.b) }

// writeHistory lets the press's parked child go on to write its file.
func (a *createRacesAdapter) writeHistory(p *crPress) error {
	if !a.gate.pass(p.st == "spawned") {
		return nil
	}
	control.ReleaseBoot(a.t, a.dir, p.cur)
	deadline := time.Now().Add(actionTimeout)
	for {
		if _, err := os.Stat(filepath.Join(a.hist, p.cur+".jsonl")); err == nil {
			p.st = "hist"
			return nil
		}
		select {
		case r := <-p.resp:
			return fmt.Errorf("WriteHistory%s: the POST answered (%v) before the file landed", p.name, r.err)
		default:
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("WriteHistory%s: %s wrote no history", p.name, p.cur)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func (a *createRacesAdapter) ClaimA() error { return a.claim(a.a) }
func (a *createRacesAdapter) ClaimB() error { return a.claim(a.b) }

func (a *createRacesAdapter) claim(p *crPress) error {
	if !a.gate.pass(p.st == "hist") {
		return nil
	}
	return a.settle(p)
}

func (a *createRacesAdapter) CreateTimeoutA() error { return a.timeout(a.a) }
func (a *createRacesAdapter) CreateTimeoutB() error { return a.timeout(a.b) }

// HistoryAfterDeadlineA: see the header; the race's outcome, driven as
// the timeout.
func (a *createRacesAdapter) HistoryAfterDeadlineA() error { return a.timeout(a.a) }

// timeout waits out serve's create timeout with the child parked.
func (a *createRacesAdapter) timeout(p *crPress) error {
	if !a.gate.pass(p.st == "spawned") {
		return nil
	}
	return a.settle(p)
}

func (a *createRacesAdapter) ChildExitsEarlyA() error {
	if !a.gate.pass(a.a.st == "spawned") {
		return nil
	}
	if a.exitAsRelease {
		control.ReleaseBoot(a.t, a.dir, a.a.cur)
	} else {
		control.ExitBoot(a.t, a.dir, a.a.cur)
	}
	return a.settle(a.a)
}

// PromptWriteFailsA lets the parked child write its file with its stdin
// dropped: serve claims it and its write of the first prompt fails.
func (a *createRacesAdapter) PromptWriteFailsA() error {
	if !a.gate.pass(a.a.st == "spawned" && a.a.kind == "typed") {
		return nil
	}
	control.BreakStdinBoot(a.t, a.dir, a.a.cur)
	control.ReleaseBoot(a.t, a.dir, a.a.cur)
	return a.settle(a.a)
}

func (a *createRacesAdapter) DeliverA() error { return a.deliver(a.a) }
func (a *createRacesAdapter) DeliverB() error { return a.deliver(a.b) }

// deliver is the page landing a 201: StartingStatus stays for the other
// press, the typed prompt shows as Sending, and the page moves only for
// the last press when the person has not moved since.
func (a *createRacesAdapter) deliver(p *crPress) error {
	if !a.gate.pass(p.st == "claimed") {
		return nil
	}
	id := p.ans.row.ID
	ctx, cancel := actionCtx()
	defer cancel()
	if _, _, err := a.s.GetSession(ctx, id); err != nil {
		return fmt.Errorf("Deliver%s: the page opens %s: %w", p.name, id, err)
	}
	p.st, p.landed, p.ans = "done", id, nil
	a.starting = inflightSt(a.other(p).st)
	p.sendng = p.kind == "typed"
	if a.last == p.name && !a.moved {
		a.route, a.palette = p.name, false
	}
	return nil
}

func (a *createRacesAdapter) RecordedA() error { return a.recorded(a.a) }
func (a *createRacesAdapter) RecordedB() error { return a.recorded(a.b) }

// recorded is the page's next read finding the first prompt recorded,
// once, as the session's only input.
func (a *createRacesAdapter) recorded(p *crPress) error {
	if !a.gate.pass(p.sendng) {
		return nil
	}
	deadline := time.Now().Add(actionTimeout)
	for {
		ctx, cancel := actionCtx()
		_, lines, err := a.s.GetSession(ctx, p.landed)
		cancel()
		if err != nil {
			return err
		}
		var inputs []string
		for _, l := range lines {
			if l.Kind == "input" {
				inputs = append(inputs, l.Text)
			}
		}
		if len(inputs) > 0 {
			if len(inputs) != 1 || inputs[0] != p.prompt {
				return fmt.Errorf("Recorded%s: inputs %q, want just %q", p.name, inputs, p.prompt)
			}
			p.sendng = false
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("Recorded%s: %q never recorded in %s", p.name, p.prompt, p.landed)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// ---- serve ----

// ServeRestart stops serve with SIGTERM while a create is in flight and
// starts it again on the same HOME. Every POST in flight ends for the
// page: a 201 it had not read is lost with the restart (the session
// stays), anything else is the toast. The last press's toast wins.
func (a *createRacesAdapter) ServeRestart() error {
	if !a.gate.pass(a.inflight()) {
		return nil
	}
	a.s.Shutdown()
	for _, p := range []*crPress{a.a, a.b} {
		if !inflightSt(p.st) {
			continue
		}
		made := p.ans != nil
		if p.resp != nil {
			r, err := a.answer(p)
			if err != nil {
				return err
			}
			made = r.err == nil
			if made {
				p.ids = appendNew(p.ids, r.row.ID)
			}
		}
		p.ans = nil
		if made {
			p.st = "lost"
		} else {
			p.st = "failed"
		}
		a.toast = p.name
	}
	a.starting = false
	if err := a.s.Resume(""); err != nil {
		return fmt.Errorf("ServeRestart: %w", err)
	}
	return nil
}

// Cleanup ends what the walk left: a parked child is told to exit, a
// POST in flight is waited for, and the round's sessions are archived.
func (a *createRacesAdapter) Cleanup() error {
	if a.a == nil {
		return nil
	}
	if !a.up() {
		if err := a.s.Resume(""); err != nil {
			return err
		}
	}
	held := control.Booting(a.dir)
	for _, p := range []*crPress{a.a, a.b} {
		for _, id := range p.ids {
			if _, ok := held[id]; ok {
				control.ExitBoot(a.t, a.dir, id)
			}
		}
		if p.resp != nil {
			if r, err := a.answer(p); err == nil && r.err == nil {
				p.ids = appendNew(p.ids, r.row.ID)
			}
		}
	}
	return a.archiveRound()
}

func (a *createRacesAdapter) up() bool {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	_, err := a.s.Build(ctx)
	return err == nil
}

// archiveRound archives every session this round's presses made, which
// kills their children. The landed ones' transcripts are kept first:
// an archived session that never ran a turn loses its file.
func (a *createRacesAdapter) archiveRound() error {
	for _, p := range []*crPress{a.a, a.b} {
		if p.landed != "" {
			es, err := history.Read(filepath.Join(a.hist, p.landed+".jsonl"))
			if err != nil {
				return fmt.Errorf("the transcript of %s, which press %s landed: %w", p.landed, p.name, err)
			}
			a.transcripts = append(a.transcripts, es)
		}
		for _, id := range p.ids {
			if _, err := os.Stat(filepath.Join(a.hist, id+".jsonl")); err != nil {
				continue
			}
			ctx, cancel := actionCtx()
			_, err := a.s.Archive(ctx, id)
			cancel()
			if err != nil {
				return fmt.Errorf("archive %s: %w", id, err)
			}
		}
	}
	return nil
}

var createRacesActions = map[string]func(*createRacesAdapter) error{
	"OpenPalette":           (*createRacesAdapter).OpenPalette,
	"Escape":                (*createRacesAdapter).Escape,
	"Navigate":              (*createRacesAdapter).Navigate,
	"StartTyped":            (*createRacesAdapter).StartTyped,
	"OverviewNew":           (*createRacesAdapter).OverviewNew,
	"DismissToast":          (*createRacesAdapter).DismissToast,
	"RetryA":                (*createRacesAdapter).RetryA,
	"RetryB":                (*createRacesAdapter).RetryB,
	"Reset":                 (*createRacesAdapter).Reset,
	"WriteHistoryA":         (*createRacesAdapter).WriteHistoryA,
	"ClaimA":                (*createRacesAdapter).ClaimA,
	"CreateTimeoutA":        (*createRacesAdapter).CreateTimeoutA,
	"ChildExitsEarlyA":      (*createRacesAdapter).ChildExitsEarlyA,
	"HistoryAfterDeadlineA": (*createRacesAdapter).HistoryAfterDeadlineA,
	"PromptWriteFailsA":     (*createRacesAdapter).PromptWriteFailsA,
	"DeliverA":              (*createRacesAdapter).DeliverA,
	"RecordedA":             (*createRacesAdapter).RecordedA,
	"WriteHistoryB":         (*createRacesAdapter).WriteHistoryB,
	"ClaimB":                (*createRacesAdapter).ClaimB,
	"CreateTimeoutB":        (*createRacesAdapter).CreateTimeoutB,
	"DeliverB":              (*createRacesAdapter).DeliverB,
	"RecordedB":             (*createRacesAdapter).RecordedB,
	"ServeRestart":          (*createRacesAdapter).ServeRestart,
}

// walk runs one path, comparing the adapter's state with the spec's
// after every step.
func (a *createRacesAdapter) walk(trace []tracecheck.Step) (err error) {
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	names := make([]string, len(trace))
	for j, s := range trace {
		names[j] = strings.TrimPrefix(s.Action, "Room#0.")
	}
	for j, s := range trace {
		a.rest = names[j+1:]
		if s.Action == "Init" {
			err = a.Init()
		} else if f, ok := createRacesActions[names[j]]; ok {
			err = f(a)
		} else {
			err = fmt.Errorf("no adapter action for %s", s.Action)
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", j, names[j], err)
		}
		got, _ := a.GetState()
		var diff []string
		for k, v := range s.State {
			f, ok := strings.CutPrefix(k, "Room#0.")
			if !ok {
				continue
			}
			if w, ok := v.(float64); ok {
				v = int(w)
			}
			if got[f] != v {
				diff = append(diff, fmt.Sprintf("%s is %v, the spec says %v", f, got[f], v))
			}
		}
		if len(diff) > 0 {
			sort.Strings(diff)
			return &walkDiff{step: j, action: names[j], diff: diff}
		}
	}
	return nil
}

// walkDiff is a state mismatch, kept typed so a run can count them by
// where they happen.
type walkDiff struct {
	step   int
	action string
	diff   []string
}

func (d *walkDiff) Error() string {
	return fmt.Sprintf("step %d (%s): %s", d.step, d.action, strings.Join(d.diff, "; "))
}

// createRacesShards is how many serves a run splits the walks over: a
// walk spends most of its time waiting on serve's timeout and drain, so
// four walk side by side.
const createRacesShards = 4

// createRacesWalks is testdata/create_races_failures' walks for cover.
func createRacesWalks(t *testing.T, cover tracecheck.Cover) [][]tracecheck.Step {
	t.Helper()
	b, err := pathsJSONCover("create_races_failures", cover)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		t.Fatal(err)
	}
	walks := make([][]tracecheck.Step, len(doc.Paths))
	for i, p := range doc.Paths {
		walks[i] = p.Trace
	}
	return walks
}

// walkCreateRaces walks walks over createRacesShards serves. It returns
// every failure, and the transcripts of the sessions presses landed.
func walkCreateRaces(t *testing.T, walks [][]tracecheck.Step, setup func(*createRacesAdapter)) (errs []error, transcripts [][]history.Entry) {
	t.Helper()
	type result struct {
		errs []error
		a    *createRacesAdapter
	}
	results := make([]result, createRacesShards)
	t.Run("shards", func(t *testing.T) {
		for i := range createRacesShards {
			t.Run(fmt.Sprint(i), func(t *testing.T) {
				t.Parallel()
				a := newCreateRacesAdapter(t)
				if setup != nil {
					setup(a)
				}
				results[i].a = a
				for k := i; k < len(walks); k += createRacesShards {
					start := time.Now()
					err := a.walk(walks[k])
					t.Logf("walk %d: %d steps in %s, ok=%v", k, len(walks[k]), time.Since(start).Round(time.Millisecond), err == nil)
					if err != nil {
						results[i].errs = append(results[i].errs, fmt.Errorf("walk %d: %w", k, err))
					}
				}
			})
		}
	})
	for _, r := range results {
		errs = append(errs, r.errs...)
		if r.a != nil {
			transcripts = append(transcripts, r.a.transcripts...)
		}
	}
	return errs, transcripts
}

// summarize groups failures by step and field, so a run that fails in
// a hundred walks for one reason says so in a few lines.
func summarize(errs []error) string {
	groups := map[string][]string{}
	for _, e := range errs {
		key := e.Error()
		var d *walkDiff
		if errors.As(e, &d) {
			key = d.action + ": " + strings.Join(d.diff, "; ")
		} else if i := strings.Index(key, "): "); i >= 0 {
			if j := strings.Index(key, " ("); j >= 0 && j < i {
				key = key[j+2:]
			}
		}
		groups[key] = append(groups[key], e.Error())
	}
	keys := make([]string, 0, len(groups))
	for k := range groups {
		keys = append(keys, k)
	}
	sort.Slice(keys, func(i, j int) bool { return len(groups[keys[i]]) > len(groups[keys[j]]) })
	var sb strings.Builder
	fmt.Fprintf(&sb, "%d failing walks:\n", len(errs))
	for _, k := range keys {
		fmt.Fprintf(&sb, "  %4d × %s\n       e.g. %s\n", len(groups[k]), k, groups[k][0])
	}
	return sb.String()
}

// TestCreateRacesFailuresPaths walks every generated walk (every
// settled state; every link under MODEL_COVER=transitions) against real
// serves, then trace-checks the transcript of every session a press
// landed.
func TestCreateRacesFailuresPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	errs, transcripts := walkCreateRaces(t, createRacesWalks(t, envCover()), nil)
	if len(errs) > 0 {
		t.Errorf("spec walks: %s", summarize(errs))
	}
	g, err := tracecheck.Load(fizzCheck(t, "create_races_failures"))
	if err != nil {
		t.Fatal(err)
	}
	for _, es := range transcripts {
		checkHistory(t, g, es, createRacesHistory)
	}
	if len(transcripts) == 0 {
		t.Fatal("no walk landed a session")
	}
	t.Logf("trace-checked %d landed sessions' histories", len(transcripts))
}

// A run whose ChildExitsEarly lets the child start instead must fail,
// or a green TestCreateRacesFailuresPaths proves nothing. The wrong
// release shows on the ChildExitsEarlyA link, which only a walk of
// every link takes; each walk that takes it is cut right after it.
func TestCreateRacesFailuresCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	var walks [][]tracecheck.Step
	for _, w := range createRacesWalks(t, tracecheck.CoverTransitions) {
		for j, s := range w {
			if s.Action == "Room#0.ChildExitsEarlyA" {
				walks = append(walks, w[:j+1])
				break
			}
		}
		if len(walks) == createRacesShards {
			break
		}
	}
	errs, _ := walkCreateRaces(t, walks, func(a *createRacesAdapter) { a.exitAsRelease = true })
	caught := false
	for _, e := range errs {
		var d *walkDiff
		if errors.As(e, &d) && d.action == "ChildExitsEarlyA" {
			caught = true
		}
	}
	if !caught {
		t.Fatalf("a run whose ChildExitsEarlyA lets the child start was not caught at that step; the walks are not checking state:\n%s", summarize(errs))
	}
	t.Logf("caught as expected:\n%s", summarize(errs))
}

// createRacesHistory reads a landed session's transcript as a path: a
// transcript cannot say which press made it, or which button, so one
// with an input is press A's palette Start and one without is the
// Overview's New. The meta entry is the file landing and the claim; the
// page lands it; the first input is the Sending preview giving way.
func createRacesHistory(entries []history.Entry) []tracecheck.Step {
	typed := false
	for _, e := range entries {
		typed = typed || e.Kind == "input"
	}
	q := func(kv ...any) map[string]any {
		m := map[string]any{}
		for i := 0; i < len(kv); i += 2 {
			m["Room#0."+kv[i].(string)] = kv[i+1]
		}
		return m
	}
	steps := []tracecheck.Step{{Action: "Init", State: q("a", "none", "sa", 0)}}
	if typed {
		steps = append(steps,
			tracecheck.Step{Action: "Room#0.OpenPalette", State: q("palette", true)},
			tracecheck.Step{Action: "Room#0.StartTyped", State: q("a", "spawned", "ka", "typed", "starting", true)})
	} else {
		steps = append(steps, tracecheck.Step{Action: "Room#0.OverviewNew", State: q("a", "spawned", "ka", "new", "starting", true)})
	}
	recorded := false
	for _, e := range entries {
		switch {
		case e.Kind == "meta":
			steps = append(steps,
				tracecheck.Step{Action: "Room#0.WriteHistoryA", State: q("a", "hist", "sa", 1)},
				tracecheck.Step{Action: "Room#0.ClaimA", State: q("a", "claimed")},
				tracecheck.Step{Action: "Room#0.DeliverA", State: q("a", "done", "route", "A", "pa", typed)})
		case e.Kind == "input" && !recorded:
			recorded = true
			steps = append(steps, tracecheck.Step{Action: "Room#0.RecordedA", State: q("pa", false)})
		}
	}
	return steps
}

func init() { historyProjections["create_races_failures"] = createRacesHistory }

// The projection is only a check if a transcript the model forbids is
// refused: a first prompt recorded with no history file before it.
func TestCreateRacesFailuresHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(fizzCheck(t, "create_races_failures"))
	if err != nil {
		t.Fatal(err)
	}
	meta := history.Entry{Kind: "meta", Data: map[string]any{"cwd": "/w"}}
	input := history.Entry{Kind: "input", Data: map[string]any{"text": "hi"}}
	done := history.Entry{Kind: "done"}
	for name, es := range map[string][]history.Entry{
		"typed": {meta, input, done, input, done},
		"new":   {meta},
	} {
		if v := g.Check(createRacesHistory(es)); v != nil {
			t.Errorf("%s: %v", name, v)
		}
	}
	if v := g.Check(createRacesHistory([]history.Entry{input, done})); v == nil {
		t.Error("a prompt recorded without the session's history file passed the trace check")
	} else {
		t.Logf("refused as expected: %v", v)
	}
}
