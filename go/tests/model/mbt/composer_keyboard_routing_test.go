//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
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

// specs/composer_keyboard_routing.fizz at the server: which handler wins
// a key typed in one session's composer.
//
// Almost all of the Composer role is the page's own state -- the draft,
// the overlays, the IME, focus, the Cmd+Enter queue, a failed load, an
// act() in flight, the refusal on screen -- which no API reports. The
// adapter keeps it as the spec's contract says the page must (not as
// app.tsx does today: the spec's header lists where the page departs,
// and the browser stage is where those are red). What reaches the server
// is done for real and read back off it:
//
//	send     an Enter (or Cmd+Enter off a running turn, or the queue's
//	         flush) the composer does not refuse is POST .../prompt; a
//	         refused one, or one a picker or an IME took, posts nothing
//	Taken    the turn's prompt is in the transcript, the row says
//	         running and the model has the request (llm-control holds it)
//	Esc      a free Esc on a sending or running turn is POST .../interrupt
//	Stopped  that stop is recorded as a cancel, and the session settles
//	Finish   the model answers until the session is idle again
//	ActDone  the act the page had in flight lands: POST .../effort on this
//	         session or on the other one it was started on
//	Reload   the thread's Retry: GET the session again
//
// and on every step an idle turn is checked against the row: a session
// the spec calls idle that the server says is running (a send that was
// not refused after all, a line that became a turn of its own) fails.
//
// A line sent while the turn is live is a steer; what becomes of it is
// steer_queue.fizz's and stop_interrupt.fizz's. Here Finish and Stopped
// settle every line the walk sent -- a steer carries the turn on, or
// arrives after it and runs as its own turn, which is answered too -- so
// the next step starts from the idle session the spec has.
type ckrAdapter struct {
	t     *testing.T
	s     *servetest.Server
	dir   string // llm-control's queue
	cwd   string
	gate  gate
	other string // the session an act elsewhere lands on

	walk  int
	id    string
	ids   []string
	turns int // llm-control turn names, unique across walks
	sends int
	sent  []sent // lines posted this walk and not yet landed
	// stopAfter is the newest seq when Esc interrupted: Stopped needs a
	// cancel after it.
	stopAfter int64
	efforts   int

	// The page's state, field for field with the spec's role.
	turn, draft, overlay, focus, transcript, acting, refused, last string
	queued, composing                                              bool

	stats map[string]int

	// startsRunning is the deliberate bug TestComposerKeyboardRouting-
	// CatchesWrongAdapter injects: Init opens the session on a first
	// prompt, so the server runs a turn where the spec's is idle. The
	// runner's walks seldom get as far as a Finish (one run: 7 turns in
	// 300 walks), so the bug it must catch is one every walk starts on.
	startsRunning bool
	// sendOverPicker is the one TestComposerKeyboardRoutingPaths-
	// CatchWrongAdapter injects: Enter over an open caret picker reaches
	// send() (the picker's capture handler not taking it). It shows on
	// one transition only, Enter with the picker open.
	sendOverPicker bool
}

func newCKRAdapter(t *testing.T) *ckrAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &ckrAdapter{t: t, s: s, dir: control.Dir(s.Home), cwd: s.Dir(t, "work"), stats: map[string]int{}}
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := s.CreateSession(ctx, a.cwd, "")
	if err != nil {
		t.Fatal(err)
	}
	a.other = row.ID
	return a
}

// Init starts each walk on a fresh idle session with the composer
// focused and empty: the spec's Init.
func (a *ckrAdapter) Init() error {
	a.walk++
	a.turn, a.draft, a.overlay, a.focus = "idle", "", "none", "composer"
	a.transcript, a.acting, a.refused, a.last = "ok", "", "", ""
	a.queued, a.composing = false, false
	a.sent, a.stopAfter = nil, 0
	a.gate.reset()
	a.topUp()
	ctx, cancel := actionCtx()
	defer cancel()
	first := ""
	if a.startsRunning {
		first = fmt.Sprintf("walk %d first prompt", a.walk)
	}
	row, err := a.s.CreateSession(ctx, a.cwd, first)
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	if a.startsRunning {
		_, _, err = a.waitFor(actionTimeout, "the first turn to run", func(r serve.Row, _ []serve.Line) bool {
			return r.Status == serve.StatusRunning
		})
	}
	return err
}

// Cleanup archives the walk's session, which kills its child: a child
// left alive would take the next walk's queued turns.
func (a *ckrAdapter) Cleanup() error {
	if a.id == "" {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Archive(ctx, a.id)
	a.releaseAll(nil)
	a.id = ""
	return err
}

func (a *ckrAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Composer", Index: 0}: a}, nil
}

// GetState is the page's state; an idle turn is the server's word too.
func (a *ckrAdapter) GetState() (map[string]any, error) {
	turn := a.turn
	if turn == "idle" {
		ctx, cancel := actionCtx()
		defer cancel()
		row, _, err := a.s.GetSession(ctx, a.id)
		if err != nil {
			return nil, err
		}
		turn = pageStatus(row.Status)
	}
	return map[string]any{
		"turn": turn, "draft": a.draft, "queued": a.queued,
		"overlay": a.overlay, "composing": a.composing, "focus": a.focus,
		"transcript": a.transcript, "acting": a.acting, "refused": a.refused,
		"last": a.last, "send_enabled": a.sendEnabled(),
	}, nil
}

// ---- the page's derived values (the spec's funcs) ----

func (a *ckrAdapter) sendEnabled() bool {
	return a.draft != "" && a.transcript == "ok" && a.acting != "here"
}

func (a *ckrAdapter) plain() bool {
	return a.overlay == "none" && !a.composing && a.focus == "composer" && a.transcript == "ok" && a.acting == ""
}

func (a *ckrAdapter) inComposer() bool {
	return a.focus == "composer" && (a.overlay == "none" || a.overlay == "picker")
}

func (a *ckrAdapter) running() bool { return a.turn == "running" || a.turn == "stopping" }

func (a *ckrAdapter) modal() bool {
	return a.overlay == "dialog" || a.overlay == "sheet" || a.overlay == "palette"
}

// send is the composer's send(): refused with its reason, or the draft
// goes to the server. A line sent while the turn is live is a steer.
func (a *ckrAdapter) send() error {
	if a.draft == "" {
		return nil
	}
	switch {
	case a.transcript == "failed":
		a.refused = "load"
		return nil
	case a.acting == "here":
		a.refused = "busy"
		return nil
	}
	a.draft, a.refused = "", ""
	if err := a.post(); err != nil {
		return err
	}
	if a.turn == "idle" {
		a.turn = "sending"
	}
	return nil
}

// ---- typing in the composer ----

func (a *ckrAdapter) typed(ok bool, shutPicker bool) error {
	if !a.gate.pass(ok) {
		return nil
	}
	a.draft, a.last = "typed", "print"
	if shutPicker {
		a.overlay = "none"
	}
	return nil
}

func (a *ckrAdapter) Type() error         { return a.typed(a.inComposer() && !a.composing, false) }
func (a *ckrAdapter) TypeQuestion() error { return a.typed(a.inComposer() && !a.composing, false) }
func (a *ckrAdapter) OptionN() error      { return a.typed(a.inComposer() && !a.composing, false) }
func (a *ckrAdapter) OptionI() error      { return a.typed(a.inComposer() && !a.composing, false) }
func (a *ckrAdapter) ShiftEnter() error   { return a.typed(a.inComposer() && !a.composing, true) }

func (a *ckrAdapter) Slash() error {
	if a.gate.pass(a.plain()) {
		a.draft, a.overlay, a.last = "typed", "picker", "print"
	}
	return nil
}

func (a *ckrAdapter) Compose() error {
	if a.gate.pass(a.plain() && a.draft != "") {
		a.composing, a.last = true, "print"
	}
	return nil
}

// ComposeEnd is the Enter that confirms the composition: never a send.
func (a *ckrAdapter) ComposeEnd() error {
	if a.gate.pass(a.composing) {
		a.composing, a.last = false, "enter"
	}
	return nil
}

// ---- Enter, Cmd+Enter, Esc ----

func (a *ckrAdapter) Enter() error {
	if !a.gate.pass(a.inComposer() && !a.composing) {
		return nil
	}
	a.last = "enter"
	if a.overlay == "picker" && !a.sendOverPicker {
		a.overlay = "none" // the picker's capture handler picks
		return nil
	}
	a.overlay = "none"
	return a.send()
}

// ModEnter queues while the turn runs; otherwise it is Enter.
func (a *ckrAdapter) ModEnter() error {
	if !a.gate.pass(a.inComposer() && !a.composing) {
		return nil
	}
	a.last = "mod_enter"
	switch {
	case a.overlay == "picker":
		a.overlay = "none"
	case a.running():
		if a.draft != "" {
			a.draft, a.queued = "", true
		}
	default:
		return a.send()
	}
	return nil
}

func (a *ckrAdapter) Esc() error {
	if !a.gate.pass(a.inComposer() || a.modal()) {
		return nil
	}
	a.last = "esc"
	switch {
	case a.modal():
		a.overlay, a.focus = "none", "composer"
	case a.composing:
		a.composing = false
	case a.overlay == "picker":
		a.overlay = "none"
	case a.turn == "sending" || a.turn == "running":
		a.stopAfter = a.newest()
		ctx, cancel := actionCtx()
		defer cancel()
		if err := a.s.Interrupt(ctx, a.id); err != nil {
			return err
		}
		a.turn = "stopping"
	}
	return nil
}

// ---- shortcuts ----

func (a *ckrAdapter) palette() error {
	ok := !a.composing && a.overlay != "dialog" && a.overlay != "sheet" &&
		(a.focus == "page" || a.overlay == "palette" || a.plain() || a.overlay == "picker")
	if a.gate.pass(ok) {
		a.overlay, a.focus, a.last = "palette", "composer", "shortcut"
	}
	return nil
}

func (a *ckrAdapter) ModP() error { return a.palette() }
func (a *ckrAdapter) ModK() error { return a.palette() }

func (a *ckrAdapter) fromPage(overlay string) error {
	if a.gate.pass(a.focus == "page" && a.overlay == "none") {
		a.overlay, a.focus, a.last = overlay, "composer", "shortcut"
	}
	return nil
}

func (a *ckrAdapter) PageQuestion() error { return a.fromPage("sheet") }
func (a *ckrAdapter) PageOptionN() error  { return a.fromPage("palette") }
func (a *ckrAdapter) PageOptionI() error  { return a.fromPage("none") }

// ---- clicks and the rest of the page ----

func (a *ckrAdapter) ClickPage() error {
	if a.gate.pass(a.plain()) {
		a.focus, a.last = "page", ""
	}
	return nil
}

func (a *ckrAdapter) OpenDialog() error {
	if a.gate.pass(a.plain()) {
		a.overlay, a.last = "dialog", ""
	}
	return nil
}

// ChangeModel and ActElsewhere start an act(); its request is in flight
// until ActDone, which is when the server sees it.
func (a *ckrAdapter) ChangeModel() error {
	if a.gate.pass(a.plain()) {
		a.acting, a.last = "here", ""
	}
	return nil
}

func (a *ckrAdapter) ActElsewhere() error {
	if a.gate.pass(a.plain()) {
		a.acting, a.last = "elsewhere", ""
	}
	return nil
}

// ActDone: the act lands, on the session it was started on.
func (a *ckrAdapter) ActDone() error {
	if !a.gate.pass(a.acting != "") {
		return nil
	}
	id := a.id
	if a.acting == "elsewhere" {
		id = a.other
	}
	a.efforts++
	level := []string{"high", "low"}[a.efforts%2]
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Effort(ctx, id, level); err != nil {
		return fmt.Errorf("the act on %s: %w", id, err)
	}
	a.acting, a.last = "", ""
	if a.refused == "busy" {
		a.refused = ""
	}
	return nil
}

// LoadFails is the page's GET failing: nothing the server records.
func (a *ckrAdapter) LoadFails() error {
	if a.gate.pass(a.plain()) {
		a.transcript, a.last = "failed", ""
	}
	return nil
}

// Reload is the thread's Retry, which loads the transcript again.
func (a *ckrAdapter) Reload() error {
	if !a.gate.pass(a.transcript == "failed") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if _, _, err := a.s.GetSession(ctx, a.id); err != nil {
		return err
	}
	a.transcript, a.last = "ok", ""
	if a.refused == "load" {
		a.refused = ""
	}
	return nil
}

// ---- the turn ----

// Taken: the turn's prompt is in the transcript, the row says running
// and the model has the request.
func (a *ckrAdapter) Taken() error {
	if !a.gate.pass(a.turn == "sending") {
		return nil
	}
	p := a.sent[0]
	_, _, err := a.waitFor(actionTimeout, "the prompt to be taken", func(r serve.Row, ls []serve.Line) bool {
		return landedAt(ls, p) != 0 && r.Status == serve.StatusRunning && len(a.held()) > 0
	})
	if err != nil {
		return err
	}
	a.turn, a.last = "running", ""
	return nil
}

func (a *ckrAdapter) Finish() error {
	if !a.gate.pass(a.turn == "running") {
		return nil
	}
	row, err := a.settle("the turn to finish", nil)
	if err != nil {
		return err
	}
	if row.Status != serve.StatusDone {
		return fmt.Errorf("finish: the row says %s, want done", row.Status)
	}
	a.turn, a.last = "idle", ""
	return nil
}

// Stopped: the stop is recorded as a cancel, then the session settles.
func (a *ckrAdapter) Stopped() error {
	if !a.gate.pass(a.turn == "stopping") {
		return nil
	}
	// Nothing is answered until the cancel is in: a request answered
	// first would end the turn as done, and hide a stop that did nothing.
	if _, _, err := a.waitFor(stopWait, "the stop's cancel", func(_ serve.Row, ls []serve.Line) bool {
		return hasKindAfter(ls, "cancelled", a.stopAfter)
	}); err != nil {
		return err
	}
	if _, err := a.settle("the stopped session to settle", nil); err != nil {
		return err
	}
	a.turn, a.last = "idle", ""
	return nil
}

// Flush: the queued message goes out once the turn has ended.
func (a *ckrAdapter) Flush() error {
	if !a.gate.pass(a.turn == "idle" && a.queued && a.acting != "here" && a.transcript == "ok") {
		return nil
	}
	if err := a.post(); err != nil {
		return err
	}
	a.queued, a.turn, a.last = false, "sending", ""
	return nil
}

// ---- helpers ----

// post sends the next line of the walk the way deliver() does.
func (a *ckrAdapter) post() error {
	a.sends++
	p := sent{id: a.sends, text: fmt.Sprintf("walk %d line %d", a.walk, a.sends), after: a.newest()}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, p.text); err != nil {
		return err
	}
	a.sent = append(a.sent, p)
	return nil
}

// settle answers every request the session makes (as release says when
// it is not nil) until the row is not running, nothing is held and every
// line the walk sent has landed -- or, after a stop, was swallowed: the
// session sat quiet for swallowWait with the line never recorded.
func (a *ckrAdapter) settle(what string, release *control.Turn) (serve.Row, error) {
	ctx, cancel := context.WithTimeout(context.Background(), actionTimeout)
	defer cancel()
	var row serve.Row
	var lines []serve.Line
	var quiet time.Time
	for {
		r, ls, err := a.s.GetSession(ctx, a.id)
		if err == nil {
			row, lines = r, ls
			held := len(a.held()) > 0
			if held {
				a.releaseAll(release)
			}
			var left []sent
			for _, p := range a.sent {
				if landedAt(ls, p) == 0 {
					left = append(left, p)
				}
			}
			a.sent = left
			switch {
			case held || r.Status == serve.StatusRunning:
				quiet = time.Time{}
			case len(left) == 0:
				return row, nil
			case quiet.IsZero():
				quiet = time.Now()
			case time.Since(quiet) > swallowWait && a.stopAfter != 0:
				a.stats["swallowed"] += len(left)
				a.sent = nil
				return row, nil
			}
		}
		select {
		case <-ctx.Done():
			return row, fmt.Errorf("waiting for %s: last status %q, %d lines unlanded, transcript %s", what, row.Status, len(a.sent), kinds(lines))
		case <-time.After(50 * time.Millisecond):
		}
	}
}

func (a *ckrAdapter) newest() int64 {
	ctx, cancel := actionCtx()
	defer cancel()
	_, lines, err := a.s.GetSession(ctx, a.id)
	if err != nil || len(lines) == 0 {
		return 0
	}
	return lines[len(lines)-1].Seq
}

func (a *ckrAdapter) waitFor(d time.Duration, what string, ok func(serve.Row, []serve.Line) bool) (serve.Row, []serve.Line, error) {
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

// topUp keeps three block turns queued, so every request the session
// makes is held until the adapter answers it.
func (a *ckrAdapter) topUp() {
	names, _ := filepath.Glob(filepath.Join(a.dir, "*.json"))
	for n := len(names); n < 3; n++ {
		a.turns++
		control.Queue(a.t, a.dir, fmt.Sprintf("k%08d", a.turns), control.Turn{Mode: "block", Text: "answered"})
	}
}

// held lists the requests in flight: taken and not yet released.
func (a *ckrAdapter) held() []string {
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

func (a *ckrAdapter) releaseAll(turn *control.Turn) {
	for _, name := range a.held() {
		if turn == nil {
			control.Release(a.t, a.dir, name)
		} else {
			control.ReleaseWith(a.t, a.dir, name, *turn)
		}
	}
	a.topUp()
}

var ckrActionTable = map[string]map[string]func(*ckrAdapter) error{"Composer": {
	"Type":         (*ckrAdapter).Type,
	"TypeQuestion": (*ckrAdapter).TypeQuestion,
	"OptionN":      (*ckrAdapter).OptionN,
	"OptionI":      (*ckrAdapter).OptionI,
	"ShiftEnter":   (*ckrAdapter).ShiftEnter,
	"Slash":        (*ckrAdapter).Slash,
	"Compose":      (*ckrAdapter).Compose,
	"ComposeEnd":   (*ckrAdapter).ComposeEnd,
	"Enter":        (*ckrAdapter).Enter,
	"ModEnter":     (*ckrAdapter).ModEnter,
	"Esc":          (*ckrAdapter).Esc,
	"ModP":         (*ckrAdapter).ModP,
	"ModK":         (*ckrAdapter).ModK,
	"PageQuestion": (*ckrAdapter).PageQuestion,
	"PageOptionN":  (*ckrAdapter).PageOptionN,
	"PageOptionI":  (*ckrAdapter).PageOptionI,
	"ClickPage":    (*ckrAdapter).ClickPage,
	"OpenDialog":   (*ckrAdapter).OpenDialog,
	"ChangeModel":  (*ckrAdapter).ChangeModel,
	"ActElsewhere": (*ckrAdapter).ActElsewhere,
	"ActDone":      (*ckrAdapter).ActDone,
	"LoadFails":    (*ckrAdapter).LoadFails,
	"Reload":       (*ckrAdapter).Reload,
	"Taken":        (*ckrAdapter).Taken,
	"Finish":       (*ckrAdapter).Finish,
	"Stopped":      (*ckrAdapter).Stopped,
	"Flush":        (*ckrAdapter).Flush,
}}

// ckrActions wraps the table for the runner, counting what each step
// did so a green run says how much of it was checked.
func ckrActions(a *ckrAdapter) map[string]map[string]fmbt.ActionFunc {
	acts := map[string]map[string]fmbt.ActionFunc{"Composer": {}}
	for name, f := range ckrActionTable["Composer"] {
		acts["Composer"][name] = func(m any, _ []fmbt.Arg) (any, error) {
			was := a.gate.off
			err := f(m.(*ckrAdapter))
			switch {
			case err != nil:
				a.stats["failed "+name]++
			case was || a.gate.off:
				a.stats["skipped"]++
			default:
				a.stats["done"]++
			}
			return nil, err
		}
	}
	return acts
}

func ckrOptions() map[string]any {
	return map[string]any{"max-seq-runs": 300, "max-actions": 8, "max-parallel-runs": 0}
}

// composerKeyboardRoutingHistory reads the trace off a transcript. Only
// the turn reaches the server, so the projection takes the one path of
// keys every transcript also is: a line that starts a turn was typed,
// sent with Enter and taken; a steer was typed and sent into the running
// turn; a cancel is Esc and Stopped; a close is Finish. The check is on
// the turn alone.
func composerKeyboardRoutingHistory(entries []history.Entry) []tracecheck.Step {
	st := func(turn string) map[string]any { return map[string]any{"Composer#0.turn": turn} }
	step := func(action, turn string) tracecheck.Step {
		return tracecheck.Step{Action: "Composer#0." + action, State: st(turn)}
	}
	steps := []tracecheck.Step{{Action: "Init", State: st("idle")}}
	cur := "idle"
	for _, e := range entries {
		switch e.Kind {
		case "input":
			if cur == "idle" {
				steps = append(steps, step("Type", "idle"), step("Enter", "sending"), step("Taken", "running"))
			} else {
				steps = append(steps, step("Type", cur), step("Enter", cur))
			}
			cur = "running"
		case "cancelled":
			if cur == "running" {
				steps = append(steps, step("Esc", "stopping"), step("Stopped", "idle"))
			}
			cur = "idle"
		case "done":
			if cur == "running" {
				steps = append(steps, step("Finish", "idle"))
			}
			cur = "idle"
		}
	}
	return steps
}

func init() { historyProjections["composer_keyboard_routing"] = composerKeyboardRoutingHistory }

func checkCKRHistories(t *testing.T, a *ckrAdapter) {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "composer_keyboard_routing"))
	if err != nil {
		t.Fatal(err)
	}
	inputs := 0
	for _, id := range a.ids {
		entries := sessionHistory(t, a.s.Home, id)
		for _, e := range entries {
			if e.Kind == "input" {
				inputs++
			}
		}
		checkHistory(t, g, entries, composerKeyboardRoutingHistory)
	}
	t.Logf("%d sessions, %d inputs replayed", len(a.ids), inputs)
}

func TestComposerKeyboardRouting(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newCKRAdapter(t)
	if err := runMBT(t, "composer_keyboard_routing", a, ckrActions(a), ckrOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("walk steps: %v", a.stats)
	checkCKRHistories(t, a)
}

// The runner picks among 27 actions at random, disabled ones included,
// and a walk ends at its first disabled one, so it rarely gets a turn
// running. The same adapter walks the graph's walks step by step: each
// action must be enabled in the adapter's view where the graph enables
// it, and the whole role state after it must be the graph's.
func TestComposerKeyboardRoutingPaths(t *testing.T) {
	t.Parallel()
	a := newCKRAdapter(t)
	if err := walkCKRPaths(t, a, envCover()); err != nil {
		t.Fatal(err)
	}
	t.Logf("walk steps: %v", a.stats)
	checkCKRHistories(t, a)
}

// walkCKRPaths drives a down every walk over the checked-in graph and
// returns the first step that errs, that the adapter's require refuses,
// or whose state is not the walk's.
func walkCKRPaths(t *testing.T, a *ckrAdapter, cover tracecheck.Cover) error {
	t.Helper()
	b, err := pathsJSONCover("composer_keyboard_routing", cover)
	if err != nil {
		return err
	}
	var file struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &file); err != nil {
		return err
	}
	if len(file.Paths) == 0 {
		return fmt.Errorf("no walks over testdata/composer_keyboard_routing")
	}
	acts := ckrActions(a)
	steps := 0
	for pi, p := range file.Paths {
		err := walkCKRPath(a, acts, pi, p.Trace)
		steps += len(p.Trace)
		if cerr := a.Cleanup(); err == nil && cerr != nil {
			err = fmt.Errorf("path %d cleanup: %w", pi, cerr)
		}
		if err != nil {
			return err
		}
	}
	t.Logf("%d walks, %d steps", len(file.Paths), steps)
	return nil
}

func walkCKRPath(a *ckrAdapter, acts map[string]map[string]fmbt.ActionFunc, pi int, trace []tracecheck.Step) error {
	var names []string
	for si, step := range trace {
		if si == 0 {
			if err := a.Init(); err != nil {
				return fmt.Errorf("path %d init: %w", pi, err)
			}
		} else {
			name := strings.TrimPrefix(step.Action, "Composer#0.")
			names = append(names, name)
			f, ok := acts["Composer"][name]
			if !ok {
				return fmt.Errorf("path %d step %d: no adapter action for %q", pi, si, step.Action)
			}
			if _, err := f(a, nil); err != nil {
				return fmt.Errorf("path %d %v: %w", pi, names, err)
			}
			if a.gate.off {
				return fmt.Errorf("path %d %v: %s is enabled in the model but not in the adapter's view", pi, names, name)
			}
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("path %d %v: %w", pi, names, err)
		}
		want := map[string]any{}
		for k, v := range step.State {
			if f, ok := strings.CutPrefix(k, "Composer#0."); ok {
				want[f] = v
			}
		}
		if !reflect.DeepEqual(jsonRound(got), jsonRound(want)) {
			return fmt.Errorf("path %d %v: state\n got %v\nwant %v", pi, names, jsonRound(got), jsonRound(want))
		}
	}
	return nil
}

// A session that opens on a running turn is not the spec's idle one;
// the random run must say so.
func TestComposerKeyboardRoutingCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newCKRAdapter(t)
	a.startsRunning = true
	err := runMBT(t, "composer_keyboard_routing", a, ckrActions(a), ckrOptions())
	if err == nil {
		t.Fatal("a run whose sessions open running passed; the runner is not checking state")
	}
	t.Logf("caught: %v", err)
}

// Enter reaching send() over an open picker sends the draft and starts
// a turn the spec never has. It shows on one transition, so the walk
// takes every link.
func TestComposerKeyboardRoutingPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newCKRAdapter(t)
	a.sendOverPicker = true
	err := walkCKRPaths(t, a, tracecheck.CoverTransitions)
	if err == nil {
		t.Fatal("an Enter that sends over the picker walked every link; the walk is not checking state")
	}
	t.Logf("caught: %v", err)
}
