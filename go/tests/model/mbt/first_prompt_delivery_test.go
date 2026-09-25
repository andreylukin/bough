//go:build !windows

package mbt

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"reflect"
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

// specs/first_prompt_delivery.fizz against a real serve: the first
// prompt of a create, from the 201 to the entry the child records for
// it, by kind and by what happens to the child in that gap.
//
// The gap is real but short: Create writes the line to the child's stdin
// as soon as its history file has the meta line, and the child only
// reads stdin once its last row (ui) has mounted. The adapter makes the
// gap a state it can hold: llm-control's start hold parks the child
// before history, the adapter lets it go and SIGSTOPs it the moment the
// meta line lands (before the supervisor's next 50 ms poll has even
// written the prompt), so the 201 comes back with the line unread.
// Record is the SIGCONT; ChildExitsBeforeRecord a SIGKILL of the frozen
// child. A second send to a live child freezes it the same way; to a
// dead one it holds the respawn at the start hold. A Stop that is not
// held SIGINTs the frozen child, which is then let go (thaw): the signal
// is already there when it resumes, as it is for a real child still
// mounting its rows.
//
// child, recorded, inputs, turn, second and title are read off the serve
// (the row's live flag, the transcript). kind, sending and stopping are
// the page's: sending is the first preview computed from the real
// transcript by app.tsx's landing rules (count rule, sameText, and for a
// "/" line cmdLanded or cmdPassed), stopping is set by Stop ("failed" on a real 404) and
// cleared when app.tsx's `live` goes false. title is history's opening
// line rule (the first input's first line); the auto title the
// session-title row writes after a turn ("control" under llm-control) is
// left out, since the spec is about which prompt names the session.
// HoldExpires waits out the supervisor's real holdLimit (20 s).

// fpdHoldLimit is serve's holdLimit plus slack for the timer to fire.
const fpdHoldLimit = 20*time.Second + time.Second

type fpdAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's dir
	hist string
	work string
	gate gate

	id  string
	ids []string
	n   int

	kind, first string // the first prompt's kind and text
	pid         int    // the session's child, 0 when it has none
	frozen      bool   // pid is SIGSTOPped
	heldStart   bool   // a respawn is parked at the start hold
	turnName    string // the held llm turn, "" when none
	stopAt      time.Time

	// The page's side.
	firstUp        bool   // the first prompt's "Sending…" preview
	stopping       string // "" | stopping | failed
	second         string // none | sent (recorded is read off the transcript)
	secondText     string
	secondAfter    int64 // newest seq when the second was sent
	secondSteer    bool  // sent while the first preview was up
	secondPending  bool  // its preview is still unlanded
	lastLines      []serve.Line
	exitAsRelease  bool // TestFirstPromptDeliveryCatchesWrongAdapter's bug
	stopFirstError bool
}

func newFPDAdapter(t *testing.T) *fpdAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &fpdAdapter{
		t: t, s: s, dir: control.Dir(s.Home),
		hist: filepath.Join(s.Home, ".bough", "history"),
		work: s.Dir(t, "work"),
	}
}

func (a *fpdAdapter) Init() error {
	a.id, a.kind, a.first = "", "none", ""
	a.pid, a.frozen, a.heldStart, a.turnName = 0, false, false, ""
	a.firstUp, a.stopping, a.second = false, "", "none"
	a.secondText, a.secondAfter, a.secondSteer, a.secondPending = "", 0, false, false
	a.gate.reset()
	return nil
}

// Cleanup archives the walk's session (which kills its child, frozen or
// parked), lifts the start hold and empties the llm queue, so a turn
// this walk queued never answers the next walk's prompt.
func (a *fpdAdapter) Cleanup() error {
	if a.id != "" {
		ctx, cancel := actionCtx()
		_, err := a.s.Archive(ctx, a.id)
		cancel()
		if err != nil {
			return err
		}
	}
	if a.frozen && a.pid != 0 {
		syscall.Kill(a.pid, syscall.SIGKILL)
	}
	control.ReleaseStart(a.t, a.dir)
	ents, _ := os.ReadDir(a.dir)
	for _, e := range ents {
		n := e.Name()
		switch {
		case strings.HasSuffix(n, ".json"):
			os.Remove(filepath.Join(a.dir, n))
		case strings.HasSuffix(n, ".taken"):
			if _, err := os.Stat(filepath.Join(a.dir, strings.TrimSuffix(n, ".taken")+".release")); err != nil {
				control.Release(a.t, a.dir, strings.TrimSuffix(n, ".taken"))
			}
		}
	}
	return nil
}

func (a *fpdAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "First", Index: 0}: a}, nil
}

func (a *fpdAdapter) GetState() (map[string]any, error) {
	st := map[string]any{
		"kind": a.kind, "child": "none", "recorded": "none", "inputs": 0, "turn": "none",
		"sending": false, "stopping": a.stopping, "second": a.second, "title": "none",
	}
	if a.id == "" {
		return st, nil
	}
	row, lines, err := a.view()
	if err != nil {
		return nil, err
	}
	a.settle(row, lines)
	st["child"] = "gone"
	if row.Live {
		st["child"] = "live"
	}
	var inputs []serve.Line
	for _, l := range lines {
		switch l.Kind {
		case "input":
			inputs = append(inputs, l)
			if l.Text == a.first && st["recorded"] == "none" {
				st["recorded"] = "input"
			}
			if a.secondText != "" && l.Text == a.secondText {
				st["second"] = "recorded"
			}
		case "command":
			if strings.TrimSpace(l.Text) == a.first && st["recorded"] == "none" {
				st["recorded"] = "command"
			}
		}
	}
	st["inputs"] = len(inputs)
	st["turn"] = fpdTurn(row, lines)
	st["sending"], st["stopping"] = a.firstUp, a.stopping
	if len(inputs) > 0 {
		switch firstLine(inputs[0].Text) {
		case firstLine(a.first):
			st["title"] = "first"
		case firstLine(a.secondText):
			st["title"] = "second"
		default:
			st["title"] = "other: " + inputs[0].Text
		}
	}
	return st, nil
}

func firstLine(s string) string { return strings.SplitN(s, "\n", 2)[0] }

// fpdTurn is the last turn as the transcript and row have it: an open
// turn is running only while the row says so (anything else is a turn
// the spec has no name for, and fails the comparison).
func fpdTurn(row serve.Row, lines []serve.Line) string {
	open, last, any := false, "", false
	for _, l := range lines {
		switch l.Kind {
		case "input":
			open, last, any = true, "", true
		case "done", "cancelled":
			if l.Kind == "done" && !open && last == "cancelled" {
				continue // the bookkeeping done after a cancel
			}
			if open {
				open, last = false, l.Kind
			}
		}
	}
	switch {
	case !any:
		return "none"
	case open && row.Status == serve.StatusRunning:
		return "running"
	case open:
		return "open, row " + string(row.Status)
	}
	return last
}

func (a *fpdAdapter) view() (serve.Row, []serve.Line, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, lines, err := a.s.GetSession(ctx, a.id)
	if err == nil {
		a.lastLines = lines
	}
	return row, lines, err
}

// settle is the page reading the transcript: previews land by app.tsx's
// rules, and with nothing live its stop state clears.
func (a *fpdAdapter) settle(row serve.Row, lines []serve.Line) {
	var inputs []serve.Line
	for _, l := range lines {
		if l.Kind == "input" {
			inputs = append(inputs, l)
		}
	}
	sameText := func(text string) bool {
		want := strings.TrimSpace(text)
		if len(want) > 200 {
			want = want[:200]
		}
		for _, l := range inputs {
			if strings.HasPrefix(strings.TrimSpace(l.Text), want) {
				return true
			}
		}
		return false
	}
	countAfter := func(after int64) int {
		n := 0
		for _, l := range inputs {
			if l.Seq > after {
				n++
			}
		}
		return n
	}
	firstCmd := fpdIsCmd(a.first)
	if a.firstUp {
		var landed bool
		if firstCmd {
			// cmdPassed: nothing was sent before the first line, so any
			// input claims a "/" line that never recorded its command.
			landed = countAfter(0) > 0
			verb := strings.Fields(a.first)[0]
			for _, l := range lines {
				if f := strings.Fields(l.Text); l.Kind == "command" && len(f) > 0 && f[0] == verb {
					landed = true
				}
			}
		} else {
			landed = countAfter(0) > 0 || sameText(a.first)
		}
		if landed {
			a.firstUp = false
		}
	}
	if a.secondPending {
		idx := 0
		if a.firstUp && !firstCmd {
			idx = 1
		}
		if countAfter(a.secondAfter) > idx || sameText(a.secondText) {
			a.secondPending = false
		}
	}
	live := row.Status == serve.StatusRunning || a.firstUp || (a.secondPending && !a.secondSteer)
	if !live {
		a.stopping = ""
	}
}

// fpdIsCmd is app.tsx's isCmd: "/" and a letter, and nothing else.
func fpdIsCmd(text string) bool {
	t := strings.TrimSpace(text)
	return len(t) > 1 && t[0] == '/' && (t[1]|0x20 >= 'a' && t[1]|0x20 <= 'z')
}

// state is GetState for a gate: along a checked path it is the spec's.
func (a *fpdAdapter) state() map[string]any {
	st, err := a.GetState()
	if err != nil {
		return map[string]any{}
	}
	return st
}

func (a *fpdAdapter) StartPlain() error     { return a.start("plain") }
func (a *fpdAdapter) StartMultiline() error { return a.start("multiline") }
func (a *fpdAdapter) StartSlash() error     { return a.start("slash") }
func (a *fpdAdapter) StartBang() error      { return a.start("bang") }

// start is the palette's Start row: POST /api/sessions with the prompt,
// the child frozen in the gap between its meta line and reading stdin.
func (a *fpdAdapter) start(kind string) error {
	if !a.gate.pass(a.kind == "none") {
		return nil
	}
	a.n++
	a.first = map[string]string{
		"plain":     fmt.Sprintf("first %04d", a.n),
		"multiline": fmt.Sprintf("first %04d\nand its second line", a.n),
		"slash":     "/think medium",
		"bang":      fmt.Sprintf("!echo first %04d", a.n),
	}[kind]
	control.HoldStart(a.t, a.dir)
	before := map[string]bool{}
	for _, id := range a.histIDs() {
		before[id] = true
	}
	resp := make(chan createResult, 1)
	go func(prompt string) {
		ctx, cancel := context.WithTimeout(context.Background(), actionTimeout)
		defer cancel()
		row, err := a.s.CreateSession(ctx, a.work, prompt)
		resp <- createResult{row, err}
	}(a.first)
	pid, err := waitHeldPID(a.dir)
	if err != nil {
		return err
	}
	a.pid = pid
	control.ReleaseStart(a.t, a.dir)
	if err := a.freezeAtMeta(before); err != nil {
		return err
	}
	var r createResult
	select {
	case r = <-resp:
	case <-time.After(actionTimeout):
		return errors.New("start: the create's POST did not answer")
	}
	if r.err != nil {
		return fmt.Errorf("start: %w", r.err)
	}
	a.id = r.row.ID
	a.ids = append(a.ids, a.id)
	a.kind, a.firstUp = kind, true
	_, lines, err := a.view()
	if err != nil {
		return err
	}
	for _, l := range lines {
		if l.Kind == "input" || l.Kind == "command" {
			return fmt.Errorf("start: the child read its prompt before it was frozen: %s", kinds(lines))
		}
	}
	return nil
}

func waitHeldPID(dir string) (int, error) {
	deadline := time.Now().Add(actionTimeout)
	for {
		if pid := control.Held(dir); pid != 0 {
			return pid, nil
		}
		if time.Now().After(deadline) {
			return 0, errors.New("no child parked at the start hold")
		}
		time.Sleep(5 * time.Millisecond)
	}
}

// freezeAtMeta SIGSTOPs the child as soon as a new history file (not in
// before) has its meta line: Create writes the prompt only after that,
// on its next 50 ms poll, so the line is still unread.
func (a *fpdAdapter) freezeAtMeta(before map[string]bool) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		for _, id := range a.histIDs() {
			if before[id] {
				continue
			}
			if b, err := os.ReadFile(filepath.Join(a.hist, id+".jsonl")); err == nil && bytes.IndexByte(b, '\n') >= 0 {
				if err := syscall.Kill(a.pid, syscall.SIGSTOP); err != nil {
					return fmt.Errorf("freeze child %d: %w", a.pid, err)
				}
				a.frozen = true
				return nil
			}
		}
		if time.Now().After(deadline) {
			return errors.New("the released child wrote no meta line")
		}
		time.Sleep(time.Millisecond)
	}
}

func (a *fpdAdapter) histIDs() []string {
	files, _ := filepath.Glob(filepath.Join(a.hist, "*.jsonl"))
	ids := make([]string, 0, len(files))
	for _, f := range files {
		ids = append(ids, strings.TrimSuffix(filepath.Base(f), ".jsonl"))
	}
	return ids
}

func (a *fpdAdapter) thaw() error {
	if !a.frozen {
		return nil
	}
	a.frozen = false
	if err := syscall.Kill(a.pid, syscall.SIGCONT); err != nil {
		return fmt.Errorf("resume child %d: %w", a.pid, err)
	}
	// A SIGINT sent while the child was stopped stays pending after the
	// SIGCONT on darwin until something else is delivered (probed: 4 of 4
	// children sat in main's <-sig for minutes, and a second signal let
	// each go at once). SIGURG is Go's own preemption signal, a no-op to
	// the child, so it is sent to have a pending SIGINT delivered, at
	// once: the child reads stdin within milliseconds of resuming.
	syscall.Kill(a.pid, syscall.SIGURG)
	return nil
}

func (a *fpdAdapter) queueTurn() string {
	a.n++
	a.turnName = fmt.Sprintf("t%05d", a.n)
	control.Queue(a.t, a.dir, a.turnName, control.Turn{Mode: "block", Text: "finished " + a.turnName})
	return a.turnName
}

// wait polls the row and transcript until ok holds, for at most d.
func (a *fpdAdapter) wait(what string, d time.Duration, ok func(serve.Row, []serve.Line) bool) error {
	deadline := time.Now().Add(d)
	for {
		row, lines, err := a.view()
		if err == nil && ok(row, lines) {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting %s for %s: row %s live=%v, transcript %s (err %v)", d, what, row.Status, row.Live, kinds(lines), err)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func waitTakenErr(dir, name string) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		if _, err := os.Stat(filepath.Join(dir, name+".taken")); err == nil {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("llm turn %q not taken", name)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func hasLine(lines []serve.Line, kind, text string) bool {
	for _, l := range lines {
		if l.Kind == kind && strings.TrimSpace(l.Text) == strings.TrimSpace(text) {
			return true
		}
	}
	return false
}

// Record lets the frozen child read its first line.
func (a *fpdAdapter) Record() error {
	st := a.state()
	if !a.gate.pass(st["child"] == "live" && st["recorded"] == "none" && a.kind != "none" && a.second == "none") {
		return nil
	}
	switch a.kind {
	case "plain", "multiline":
		name := a.queueTurn()
		held := a.stopping == "stopping"
		if err := a.thaw(); err != nil {
			return err
		}
		if held {
			// The held Stop goes out as the child takes the prompt: the
			// turn opens and is cancelled, and the child exits.
			if err := a.wait("the held stop to cancel the first prompt", actionTimeout, func(r serve.Row, l []serve.Line) bool {
				return hasLine(l, "input", a.first) && lastCloseCancelled(l) && !r.Live
			}); err != nil {
				return err
			}
			a.pid = 0
			return nil
		}
		if err := a.wait("the first prompt to become a running turn", actionTimeout, func(r serve.Row, l []serve.Line) bool {
			return hasLine(l, "input", a.first) && r.Status == serve.StatusRunning
		}); err != nil {
			return err
		}
		return waitTakenErr(a.dir, name)
	default:
		if err := a.thaw(); err != nil {
			return err
		}
		return a.wait("the first line's command entry", actionTimeout, func(_ serve.Row, l []serve.Line) bool {
			return hasLine(l, "command", a.first)
		})
	}
}

// ChildExitsBeforeRecord kills the frozen child: a crash in the gap.
func (a *fpdAdapter) ChildExitsBeforeRecord() error {
	st := a.state()
	if !a.gate.pass(st["child"] == "live" && st["recorded"] == "none" && a.kind != "none" && a.second == "none") {
		return nil
	}
	if a.exitAsRelease {
		return a.thaw()
	}
	if err := syscall.Kill(a.pid, syscall.SIGKILL); err != nil {
		return fmt.Errorf("kill child %d: %w", a.pid, err)
	}
	a.pid, a.frozen = 0, false
	return a.wait("the killed child's lease to go", actionTimeout, func(r serve.Row, _ []serve.Line) bool { return !r.Live })
}

func (a *fpdAdapter) Finish() error {
	if !a.gate.pass(a.state()["turn"] == "running") {
		return nil
	}
	control.Release(a.t, a.dir, a.turnName)
	a.turnName = ""
	return a.wait("the turn to finish", actionTimeout, func(r serve.Row, _ []serve.Line) bool { return r.Status != serve.StatusRunning })
}

// Stop is the composer's Stop: POST /interrupt.
func (a *fpdAdapter) Stop() error {
	st := a.state()
	if !a.gate.pass(a.stopping == "" && a.second != "sent" && (a.firstUp || st["turn"] == "running")) {
		return nil
	}
	a.stopping = "stopping"
	live, running := st["child"] == "live", st["turn"] == "running"
	held := live && !running && st["recorded"] == "none" && (a.kind == "plain" || a.kind == "multiline")
	ctx, cancel := actionCtx()
	defer cancel()
	err := a.s.Interrupt(ctx, a.id)
	switch {
	case !live:
		var e *servetest.APIError
		if !errors.As(err, &e) || e.Status != http.StatusNotFound {
			return fmt.Errorf("stop with no child: got %v, want a 404", err)
		}
		a.stopping = "failed"
		return nil
	case err != nil:
		return fmt.Errorf("stop: %w", err)
	case running:
		a.pid = 0
		return a.wait("the stop to cancel the running turn", stopBound, func(r serve.Row, l []serve.Line) bool {
			return lastCloseCancelled(l) && !r.Live
		})
	case held:
		// Held: the child is frozen with the line unread, and stays.
		a.stopAt = time.Now()
		return nil
	default:
		// Not held: the SIGINT went straight out, to a frozen child that
		// has not read the line (or an idle one after a "!" line); the
		// child must exit without recording anything more.
		if err := a.thaw(); err != nil {
			return err
		}
		a.pid = 0
		return a.wait("the interrupted child to exit", stopBound, func(r serve.Row, _ []serve.Line) bool { return !r.Live })
	}
}

// HoldExpires waits out serve's holdLimit; its SIGINT is then pending on
// the frozen child, which is let go and must exit with the line unread.
func (a *fpdAdapter) HoldExpires() error {
	st := a.state()
	if !a.gate.pass(a.stopping == "stopping" && st["child"] == "live" && st["recorded"] == "none" &&
		(a.kind == "plain" || a.kind == "multiline") && a.second == "none") {
		return nil
	}
	time.Sleep(time.Until(a.stopAt.Add(fpdHoldLimit)))
	if err := a.thaw(); err != nil {
		return err
	}
	a.pid = 0
	return a.wait("the child to exit on the expired hold", stopBound, func(r serve.Row, _ []serve.Line) bool { return !r.Live })
}

// SendSecond is the composer's send once the first is out of its gap.
// The line is left unread (a live child frozen, a respawn parked at the
// start hold) until RecordSecond.
func (a *fpdAdapter) SendSecond() error {
	st := a.state()
	if !a.gate.pass(a.kind != "none" && a.second == "none" && st["turn"] != "running" &&
		(st["child"] != "live" || st["recorded"] != "none")) {
		return nil
	}
	a.n++
	a.secondText = fmt.Sprintf("second %04d", a.n)
	a.secondAfter = lastSeq(a.lastLines)
	a.secondSteer, a.secondPending = a.firstUp, true
	ctx, cancel := actionCtx()
	defer cancel()
	if st["child"] == "live" {
		if err := syscall.Kill(a.pid, syscall.SIGSTOP); err != nil {
			return fmt.Errorf("freeze child %d: %w", a.pid, err)
		}
		a.frozen = true
		if err := a.s.Prompt(ctx, a.id, a.secondText); err != nil {
			return fmt.Errorf("send second: %w", err)
		}
	} else {
		control.HoldStart(a.t, a.dir)
		a.heldStart = true
		if err := a.s.Prompt(ctx, a.id, a.secondText); err != nil {
			return fmt.Errorf("send second: %w", err)
		}
		pid, err := waitHeldPID(a.dir)
		if err != nil {
			return err
		}
		a.pid = pid
	}
	a.second = "sent"
	return nil
}

func (a *fpdAdapter) RecordSecond() error {
	if !a.gate.pass(a.second == "sent") {
		return nil
	}
	name := a.queueTurn()
	if a.heldStart {
		a.heldStart = false
		control.ReleaseStart(a.t, a.dir)
	} else if err := a.thaw(); err != nil {
		return err
	}
	if err := a.wait("the second message to become a running turn", actionTimeout, func(r serve.Row, l []serve.Line) bool {
		return hasLine(l, "input", a.secondText) && r.Status == serve.StatusRunning
	}); err != nil {
		return err
	}
	a.second = "recorded"
	return waitTakenErr(a.dir, name)
}

// Reload drops the page's in-memory previews and stop state.
func (a *fpdAdapter) Reload() error {
	if !a.gate.pass((a.firstUp || a.stopping != "") && a.second != "sent") {
		return nil
	}
	a.firstUp, a.secondPending, a.stopping = false, false, ""
	return nil
}

// end is the runner's name for the self-link of a state nothing leaves.
func (a *fpdAdapter) end() error { return nil }

var fpdActions = map[string]map[string]fmbt.ActionFunc{"": {"end": action((*fpdAdapter).end)}, "First": {
	"end":                    action((*fpdAdapter).end),
	"StartPlain":             action((*fpdAdapter).StartPlain),
	"StartMultiline":         action((*fpdAdapter).StartMultiline),
	"StartSlash":             action((*fpdAdapter).StartSlash),
	"StartBang":              action((*fpdAdapter).StartBang),
	"Record":                 action((*fpdAdapter).Record),
	"ChildExitsBeforeRecord": action((*fpdAdapter).ChildExitsBeforeRecord),
	"Finish":                 action((*fpdAdapter).Finish),
	"Stop":                   action((*fpdAdapter).Stop),
	"HoldExpires":            action((*fpdAdapter).HoldExpires),
	"SendSecond":             action((*fpdAdapter).SendSecond),
	"RecordSecond":           action((*fpdAdapter).RecordSecond),
	"Reload":                 action((*fpdAdapter).Reload),
}}

func fpdOptions() map[string]any {
	return map[string]any{"max-seq-runs": 60, "max-actions": 8, "max-parallel-runs": 0}
}

// walkFPDPaths walks the derived paths through the adapter, comparing
// the whole role state after every step. Every path runs, so one run
// reports every divergence, unless stopFirstError.
func walkFPDPaths(t *testing.T, a *fpdAdapter, cover tracecheck.Cover) error {
	t.Helper()
	b, err := pathsJSONCover("first_prompt_delivery", cover)
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
	var errs []error
	for i, p := range doc.Paths {
		if err := a.walk(p.Trace); err != nil {
			var names []string
			for _, s := range p.Trace {
				names = append(names, strings.TrimPrefix(s.Action, "First#0."))
			}
			errs = append(errs, fmt.Errorf("path %d %v: %w", i, names, err))
			if a.stopFirstError {
				break
			}
		}
	}
	t.Logf("first_prompt_delivery: %d paths (%s), %d diverged", len(doc.Paths), cover, len(errs))
	return errors.Join(errs...)
}

func (a *fpdAdapter) walk(trace []tracecheck.Step) (err error) {
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	for j, s := range trace {
		name := strings.TrimPrefix(s.Action, "First#0.")
		if name == "Init" {
			err = a.Init()
		} else {
			f, ok := fpdActions["First"][name]
			if !ok {
				return fmt.Errorf("step %d: no adapter action %q", j, s.Action)
			}
			_, err = f(a, nil)
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", j, name, err)
		}
		if a.gate.off {
			return fmt.Errorf("step %d (%s): the adapter says the spec's require does not hold", j, name)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w", j, name, err)
		}
		want := map[string]any{}
		for k, v := range s.State {
			if f, ok := strings.CutPrefix(k, "First#0."); ok {
				if n, isNum := v.(float64); isNum {
					v = int(n)
				}
				want[f] = v
			}
		}
		if !reflect.DeepEqual(got, want) {
			return fmt.Errorf("step %d (%s): state\n got  %v\n want %v\n transcript %s", j, name, got, want, kinds(a.lastLines))
		}
	}
	return nil
}

// firstPromptDeliveryHistory reads a created session's transcript as a
// path. The page's side (previews, Reload, a Stop that 404ed or was held
// and dropped) leaves nothing there, so only what the child recorded is
// read: the first recorded line's kind picks the Start, and a transcript
// with no line at all is a child that died in the gap. A transcript
// cannot tell a first prompt from a second one that claimed a lost
// first, so an input opens as the first prompt, which the graph also has.
func firstPromptDeliveryHistory(entries []history.Entry) []tracecheck.Step {
	q := func(kv ...any) map[string]any {
		m := map[string]any{}
		for i := 0; i < len(kv); i += 2 {
			m["First#0."+kv[i].(string)] = kv[i+1]
		}
		return m
	}
	step := func(action string, state map[string]any) tracecheck.Step {
		return tracecheck.Step{Action: "First#0." + action, State: state}
	}
	var ev []history.Entry
	for _, e := range entries {
		switch e.Kind {
		case "input", "command", "done", "cancelled":
			ev = append(ev, e)
		}
	}
	steps := []tracecheck.Step{{Action: "Init", State: q("kind", "none", "child", "none")}}
	if len(ev) == 0 {
		return append(steps, step("StartPlain", q("child", "live", "sending", true)),
			step("ChildExitsBeforeRecord", q("child", "gone", "recorded", "none")))
	}
	inputs := 0
	// ends appends the close of the turn ev[0] opened, and returns the
	// entries after it.
	ends := func(ev []history.Entry) []history.Entry {
		if len(ev) == 0 {
			return ev
		}
		switch ev[0].Kind {
		case "done":
			steps = append(steps, step("Finish", q("turn", "done")))
			return ev[1:]
		case "cancelled":
			steps = append(steps, step("Stop", q("turn", "cancelled", "child", "gone")))
			ev = ev[1:]
			if len(ev) > 0 && ev[0].Kind == "done" {
				ev = ev[1:] // the bookkeeping done after a cancel
			}
		}
		return ev
	}
	first := ev[0]
	text := history.Prompt(first)
	if first.Kind == "command" {
		if strings.HasPrefix(text, "/") {
			steps = append(steps, step("StartSlash", q("kind", "slash")))
		} else {
			steps = append(steps, step("StartBang", q("kind", "bang")))
		}
		steps = append(steps, step("Record", q("recorded", "command", "inputs", 0, "turn", "none")))
		ev = ev[1:]
	} else {
		if strings.Contains(text, "\n") {
			steps = append(steps, step("StartMultiline", q("kind", "multiline")))
		} else {
			steps = append(steps, step("StartPlain", q("kind", "plain")))
		}
		inputs++
		steps = append(steps, step("Record", q("recorded", "input", "inputs", inputs, "turn", "running", "title", "first")))
		ev = ends(ev[1:])
	}
	// The second message: the next input, and the close of its turn.
	for len(ev) > 0 && ev[0].Kind != "input" {
		ev = ev[1:]
	}
	if len(ev) == 0 {
		return steps
	}
	inputs++
	steps = append(steps, step("SendSecond", q("second", "sent")),
		step("RecordSecond", q("second", "recorded", "inputs", inputs, "turn", "running")))
	ends(ev[1:])
	return steps
}

func init() { historyProjections["first_prompt_delivery"] = firstPromptDeliveryHistory }

func fpdGraph(t *testing.T) *tracecheck.Graph {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("first_prompt_delivery")), "..", "testdata", "first_prompt_delivery"))
	if err != nil {
		t.Fatal(err)
	}
	return g
}

// TestFirstPromptDeliveryPaths walks every derived path (every state by
// default, every link under MODEL_COVER=transitions) against a real
// serve, then replays each session's transcript on the graph.
func TestFirstPromptDeliveryPaths(t *testing.T) {
	t.Parallel()
	a := newFPDAdapter(t)
	if err := walkFPDPaths(t, a, envCover()); err != nil {
		t.Errorf("spec paths:\n%v", err)
	}
	g := fpdGraph(t)
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), firstPromptDeliveryHistory)
	}
	t.Logf("trace-checked %d sessions' histories", len(a.ids))
}

// TestFirstPromptDelivery is the runner's random walks over the same
// adapter; runMBT skips it outside the exhaustive run.
func TestFirstPromptDelivery(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newFPDAdapter(t)
	if err := runMBT(t, "first_prompt_delivery", a, fpdActions, fpdOptions()); err != nil {
		t.Errorf("model-based run: %v", err)
	}
	g := fpdGraph(t)
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), firstPromptDeliveryHistory)
	}
}

// The projection is only a check if a transcript the model forbids is
// refused: a second prompt recorded while the first turn still runs.
func TestFirstPromptDeliveryHistoryProjection(t *testing.T) {
	t.Parallel()
	g := fpdGraph(t)
	meta := history.Entry{Kind: "meta", Data: map[string]any{"cwd": "/w"}}
	in := func(s string) history.Entry { return history.Entry{Kind: "input", Data: map[string]any{"text": s}} }
	cmd := func(s string) history.Entry { return history.Entry{Kind: "command", Data: map[string]any{"text": s}} }
	sys := history.Entry{Kind: "system", Data: map[string]any{"text": "ok"}}
	done := history.Entry{Kind: "done"}
	cancelled := history.Entry{Kind: "cancelled"}
	for name, es := range map[string][]history.Entry{
		"lost":           {meta},
		"plain":          {meta, in("hi"), done, in("again"), done},
		"multiline":      {meta, in("a\nb"), done},
		"held stop":      {meta, in("hi"), cancelled, done, in("again"), cancelled, done},
		"slash":          {meta, cmd("/think medium"), sys, in("again"), done},
		"bang":           {meta, cmd("!echo hi"), sys, in("again")},
		"lost, then two": {meta, in("second"), done},
	} {
		if v := g.Check(firstPromptDeliveryHistory(es)); v != nil {
			t.Errorf("%s: %v", name, v)
		}
	}
	if v := g.Check(firstPromptDeliveryHistory([]history.Entry{meta, in("hi"), in("again")})); v == nil {
		t.Error("a second prompt recorded while the first turn ran passed the trace check")
	} else {
		t.Logf("refused as expected: %v", v)
	}
}

// A ChildExitsBeforeRecord wired to let the child go on instead of
// killing it must fail the paths, or a green run proves nothing. The
// wrong release shows on that one transition, so every link is walked.
func TestFirstPromptDeliveryCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newFPDAdapter(t)
	a.exitAsRelease, a.stopFirstError = true, true
	err := walkFPDPaths(t, a, tracecheck.CoverTransitions)
	if err == nil {
		t.Fatal("paths whose ChildExitsBeforeRecord lets the child live passed; the walk is not checking state")
	}
	t.Logf("caught as expected: %v", err)
}
