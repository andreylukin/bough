//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
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

// specs/session_create.fizz against a real serve: the page's side of a
// create (palette, folder prompt, Overview New, the "Sending…" preview)
// is the adapter's own state, since it is the page; the server's side
// (the child, its history file, the POST's answer, the recorded first
// prompt) is read off the serve and the filesystem.
//
// The spec splits a create into steps the product runs back to back.
// llm-control's start hold (control.HoldStart) parks every spawned child
// before its history row writes the file, so the adapter decides when
// the file lands (WriteHistory) or whether the child dies first
// (ChildExits). The POST runs in the background; the page only learns
// its answer at Claim, Timeout or ChildExits, which is when the adapter
// reads it. Timeout is the supervisor's real createTimeout (10 s).

type createResult struct {
	row serve.Row
	err error
}

type sessionCreateAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's dir: the start hold lives there
	hist string // the serve's history dir
	work string // the folder every create starts in
	gate gate

	// The page's state, as the spec names it.
	ui, query, first  string
	other             bool
	sending, recorded bool

	// The create in flight or just answered.
	before  map[string]bool   // history ids before the POST; nil when no create is tracked
	resp    chan createResult // the POST's answer, until read
	pid     int               // the spawned child, parked at the hold
	id      string            // the claimed session
	otherID string            // the unrelated session OtherWrites wrote
	prompt  string
	n       int
	ids     []string // claimed sessions, for the trace check

	// exitAsRelease is the deliberate bug
	// TestSessionCreateCatchesWrongAdapter injects: ChildExits lets the
	// child start instead of killing it.
	exitAsRelease bool
}

func newSessionCreateAdapter(t *testing.T) *sessionCreateAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &sessionCreateAdapter{
		t: t, s: s, dir: control.Dir(s.Home),
		hist: filepath.Join(s.Home, ".bough", "history"),
		work: s.Dir(t, "work"),
	}
}

func (a *sessionCreateAdapter) Init() error {
	a.ui, a.query, a.first = "idle", "none", "none"
	a.other, a.sending, a.recorded = false, false, false
	a.before, a.resp, a.pid, a.id, a.otherID, a.prompt = nil, nil, 0, "", "", ""
	a.gate.reset()
	return nil
}

// Cleanup ends whatever the walk left: a POST still in flight (its
// parked child is told to exit) and a claimed child, which is archived
// so a hundred walks do not leave a hundred idle children.
func (a *sessionCreateAdapter) Cleanup() error {
	if a.resp != nil {
		control.ExitStart(a.t, a.dir)
		r := <-a.resp
		a.resp = nil
		if r.err == nil {
			a.id = r.row.ID
		}
	}
	control.ReleaseStart(a.t, a.dir)
	if a.id != "" {
		ctx, cancel := actionCtx()
		defer cancel()
		if _, err := a.s.Archive(ctx, a.id); err != nil {
			return err
		}
	}
	return nil
}

func (a *sessionCreateAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Create", Index: 0}: a}, nil
}

func (a *sessionCreateAdapter) GetState() (map[string]any, error) {
	child, err := a.child()
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"ui": a.ui, "query": a.query, "first": a.first,
		"child": child, "hist": a.histWritten(), "other": a.other,
		"sending": a.sending, "recorded": a.recorded,
	}, nil
}

// child is this create's child as the server has it: live once the
// serve lists the claimed session live, spawned while the parked
// process is alive and unclaimed, none otherwise.
func (a *sessionCreateAdapter) child() (string, error) {
	if a.id != "" {
		ctx, cancel := actionCtx()
		defer cancel()
		row, _, err := a.s.GetSession(ctx, a.id)
		if err != nil {
			return "", err
		}
		if row.Live {
			return "live", nil
		}
		return "none", nil
	}
	if a.pid != 0 && alive(a.pid) {
		return "spawned", nil
	}
	return "none", nil
}

// histWritten is whether this create's child has a history file: the
// claimed id's, or before the claim any new file but OtherWrites'.
func (a *sessionCreateAdapter) histWritten() bool {
	if a.before == nil {
		return false
	}
	if a.id != "" {
		// The claimed file must be this child's: its meta names the
		// create's folder, which the unrelated session's does not.
		es, err := history.Read(filepath.Join(a.hist, a.id+".jsonl"))
		if err != nil {
			return false
		}
		for _, e := range es {
			if e.Kind == "meta" {
				return e.Data["cwd"] == a.work
			}
		}
		return false
	}
	for _, id := range a.histIDs() {
		if !a.before[id] && id != a.otherID {
			return true
		}
	}
	return false
}

func (a *sessionCreateAdapter) histIDs() []string {
	files, _ := filepath.Glob(filepath.Join(a.hist, "*.jsonl"))
	ids := make([]string, 0, len(files))
	for _, f := range files {
		ids = append(ids, strings.TrimSuffix(filepath.Base(f), ".jsonl"))
	}
	return ids
}

func alive(pid int) bool { return syscall.Kill(pid, 0) == nil }

// spawned is the spec's child == "spawned", read off the adapter's view.
func (a *sessionCreateAdapter) spawned() bool { return a.resp != nil && a.id == "" }

func (a *sessionCreateAdapter) OpenPalette() error {
	if a.gate.pass(a.ui == "idle") {
		a.ui, a.query = "palette", "none"
	}
	return nil
}

func (a *sessionCreateAdapter) Escape() error {
	if a.gate.pass(a.ui == "palette" || a.ui == "folder") {
		a.ui, a.query = "idle", "none"
	}
	return nil
}

func (a *sessionCreateAdapter) TypeText() error {
	if a.gate.pass(a.ui == "palette" && a.query != "text") {
		a.query = "text"
	}
	return nil
}

func (a *sessionCreateAdapter) TypePath() error {
	if a.gate.pass(a.ui == "palette" && a.query != "path") {
		a.query = "path"
	}
	return nil
}

func (a *sessionCreateAdapter) PickFolder() error {
	if a.gate.pass(a.ui == "palette" && a.query == "path") {
		a.query = "none"
	}
	return nil
}

func (a *sessionCreateAdapter) StartTyped() error {
	if !a.gate.pass(a.ui == "palette" && a.query != "none") {
		return nil
	}
	return a.start("text")
}

func (a *sessionCreateAdapter) StartEmpty() error {
	if !a.gate.pass(a.ui == "palette") {
		return nil
	}
	return a.start("empty")
}

func (a *sessionCreateAdapter) OverviewNew() error {
	if !a.gate.pass(a.ui == "idle") {
		return nil
	}
	return a.start("empty")
}

func (a *sessionCreateAdapter) AskFolder() error {
	if a.gate.pass(a.ui == "palette") {
		a.ui, a.query = "folder", "none"
	}
	return nil
}

func (a *sessionCreateAdapter) FolderStart() error {
	if !a.gate.pass(a.ui == "folder") {
		return nil
	}
	return a.start("empty")
}

// FolderMissing answers the folder prompt with a path that is not a
// directory: the POST must be refused (400) without spawning anything.
func (a *sessionCreateAdapter) FolderMissing() error {
	if !a.gate.pass(a.ui == "folder") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, filepath.Join(a.s.Root, "no-such-folder"), "")
	a.ui = "failed"
	if err == nil {
		// Wrong, and the state says so: a session was made.
		a.before, a.id, a.ui = map[string]bool{}, row.ID, "opened"
		return nil
	}
	if ae := (*servetest.APIError)(nil); !errors.As(err, &ae) || ae.Status != 400 {
		return fmt.Errorf("FolderMissing: want a 400, got %w", err)
	}
	return nil
}

// start is the page's POST /api/sessions, left in flight: the child it
// spawns parks at the hold before writing history.
func (a *sessionCreateAdapter) start(first string) error {
	a.n++
	a.prompt = ""
	if first == "text" {
		a.prompt = fmt.Sprintf("first prompt %04d", a.n)
	}
	control.HoldStart(a.t, a.dir)
	a.before = map[string]bool{}
	for _, id := range a.histIDs() {
		a.before[id] = true
	}
	a.resp = make(chan createResult, 1)
	go func(resp chan createResult, prompt string) {
		// Longer than createTimeout, so the serve answers first.
		ctx, cancel := context.WithTimeout(context.Background(), actionTimeout)
		defer cancel()
		row, err := a.s.CreateSession(ctx, a.work, prompt)
		resp <- createResult{row, err}
	}(a.resp, a.prompt)
	a.ui, a.query, a.first = "creating", "none", first
	deadline := time.Now().Add(actionTimeout)
	for a.pid = control.Held(a.dir); a.pid == 0; a.pid = control.Held(a.dir) {
		if time.Now().After(deadline) {
			return errors.New("start: no child parked at the hold")
		}
		time.Sleep(10 * time.Millisecond)
	}
	return nil
}

// WriteHistory lets the parked child go on: its history row writes the
// file, which is all this step promises.
func (a *sessionCreateAdapter) WriteHistory() error {
	if !a.gate.pass(a.spawned() && !a.histWritten()) {
		return nil
	}
	control.ReleaseStart(a.t, a.dir)
	deadline := time.Now().Add(actionTimeout)
	for !a.histWritten() {
		if time.Now().After(deadline) {
			return errors.New("WriteHistory: the released child wrote no history")
		}
		time.Sleep(10 * time.Millisecond)
	}
	return nil
}

// OtherWrites is an unrelated bough starting a session while Create is
// discovering its child's id: a second new history file.
func (a *sessionCreateAdapter) OtherWrites() error {
	if !a.gate.pass(a.spawned() && !a.other) {
		return nil
	}
	a.otherID = history.NewID()
	st, err := history.Open(filepath.Join(a.hist, a.otherID+".jsonl"))
	if err != nil {
		return err
	}
	st.Append("meta", map[string]any{"cwd": a.s.Root, "mode": "local", "origin": "tui"})
	if err := st.Close(); err != nil {
		return err
	}
	a.other = true
	return nil
}

func (a *sessionCreateAdapter) Claim() error {
	if !a.gate.pass(a.spawned() && a.histWritten()) {
		return nil
	}
	return a.answer()
}

func (a *sessionCreateAdapter) Timeout() error {
	if !a.gate.pass(a.spawned() && !a.histWritten()) {
		return nil
	}
	return a.answer()
}

// ChildExits kills the parked child before it writes history.
func (a *sessionCreateAdapter) ChildExits() error {
	if !a.gate.pass(a.spawned() && !a.histWritten()) {
		return nil
	}
	if a.exitAsRelease {
		control.ReleaseStart(a.t, a.dir)
	} else {
		control.ExitStart(a.t, a.dir)
	}
	return a.answer()
}

// answer is the page reading the POST's reply: a 201 opens the session
// (and shows the typed prompt as "Sending…"), anything else is the
// "Couldn't start a session" toast. A failed create's child must be
// gone; it is given a moment to be reaped.
func (a *sessionCreateAdapter) answer() error {
	var r createResult
	select {
	case r = <-a.resp:
	case <-time.After(actionTimeout):
		return errors.New("the create's POST did not answer")
	}
	a.resp = nil
	control.ReleaseStart(a.t, a.dir)
	if r.err != nil {
		if ae := (*servetest.APIError)(nil); !errors.As(r.err, &ae) {
			return r.err
		}
		a.ui = "failed"
		for deadline := time.Now().Add(5 * time.Second); alive(a.pid) && time.Now().Before(deadline); {
			time.Sleep(20 * time.Millisecond)
		}
		return nil
	}
	a.id = r.row.ID
	a.ids = append(a.ids, a.id)
	a.sending = a.first == "text"
	ctx, cancel := actionCtx()
	defer cancel()
	a.ui = "opened"
	if _, _, err := a.s.GetSession(ctx, a.id); err != nil {
		// The page's first read 404s: it would show "starting".
		if ae := (*servetest.APIError)(nil); errors.As(err, &ae) && ae.Status == 404 {
			a.ui = "starting"
			return nil
		}
		return err
	}
	return nil
}

// Land is the page's next read finding the first prompt recorded: it
// must be there exactly once, as the session's only input.
func (a *sessionCreateAdapter) Land() error {
	if !a.gate.pass(a.sending) {
		return nil
	}
	deadline := time.Now().Add(actionTimeout)
	for {
		ctx, cancel := actionCtx()
		_, lines, err := a.s.GetSession(ctx, a.id)
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
			if len(inputs) != 1 || inputs[0] != a.prompt {
				return fmt.Errorf("Land: inputs %q, want just %q", inputs, a.prompt)
			}
			a.sending, a.recorded = false, true
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("Land: %q never recorded", a.prompt)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// Close goes back to the overview; the session stays (Cleanup archives
// it when the walk ends).
func (a *sessionCreateAdapter) Close() error {
	if !a.gate.pass((a.ui == "opened") && !a.sending) {
		return nil
	}
	a.ui, a.first, a.recorded, a.other = "idle", "none", false, false
	a.forget()
	return nil
}

func (a *sessionCreateAdapter) Dismiss() error {
	if !a.gate.pass(a.ui == "failed") {
		return nil
	}
	a.ui, a.first, a.other = "idle", "none", false
	a.forget()
	return nil
}

// forget stops tracking the last create. A claimed session stays live
// and is archived now: nothing later in the walk looks at it.
func (a *sessionCreateAdapter) forget() {
	if a.id != "" {
		ctx, cancel := actionCtx()
		a.s.Archive(ctx, a.id)
		cancel()
	}
	a.before, a.pid, a.id, a.otherID, a.prompt = nil, 0, "", "", ""
}

var sessionCreateActions = map[string]map[string]fmbt.ActionFunc{"Create": {
	"OpenPalette":   action((*sessionCreateAdapter).OpenPalette),
	"Escape":        action((*sessionCreateAdapter).Escape),
	"TypeText":      action((*sessionCreateAdapter).TypeText),
	"TypePath":      action((*sessionCreateAdapter).TypePath),
	"PickFolder":    action((*sessionCreateAdapter).PickFolder),
	"StartTyped":    action((*sessionCreateAdapter).StartTyped),
	"StartEmpty":    action((*sessionCreateAdapter).StartEmpty),
	"OverviewNew":   action((*sessionCreateAdapter).OverviewNew),
	"AskFolder":     action((*sessionCreateAdapter).AskFolder),
	"FolderStart":   action((*sessionCreateAdapter).FolderStart),
	"FolderMissing": action((*sessionCreateAdapter).FolderMissing),
	"WriteHistory":  action((*sessionCreateAdapter).WriteHistory),
	"OtherWrites":   action((*sessionCreateAdapter).OtherWrites),
	"Claim":         action((*sessionCreateAdapter).Claim),
	"Timeout":       action((*sessionCreateAdapter).Timeout),
	"ChildExits":    action((*sessionCreateAdapter).ChildExits),
	"Land":          action((*sessionCreateAdapter).Land),
	"Close":         action((*sessionCreateAdapter).Close),
	"Dismiss":       action((*sessionCreateAdapter).Dismiss),
}}

// A walk of 8 reaches a create's outcome and the step after it; each
// Timeout costs the supervisor's real 10 s, so the run is kept short.
func sessionCreateOptions() map[string]any {
	return map[string]any{"max-seq-runs": 60, "max-actions": 8, "max-parallel-runs": 0}
}

// sessionCreateHistory reads a created session's transcript as a path:
// a transcript cannot say which button started it, so a session whose
// transcript has an input is the typed start (the palette's Start row)
// and one without is the Overview's empty start. Its meta entry is the
// history landing (and the claim right after it), the first input is
// the prompt landing; later turns are outside the spec.
func sessionCreateHistory(entries []history.Entry) []tracecheck.Step {
	typed := false
	for _, e := range entries {
		typed = typed || e.Kind == "input"
	}
	q := func(kv ...any) map[string]any {
		m := map[string]any{}
		for i := 0; i < len(kv); i += 2 {
			m["Create#0."+kv[i].(string)] = kv[i+1]
		}
		return m
	}
	steps := []tracecheck.Step{{Action: "Init", State: q("ui", "idle", "child", "none")}}
	if typed {
		steps = append(steps,
			tracecheck.Step{Action: "Create#0.OpenPalette", State: q("ui", "palette")},
			tracecheck.Step{Action: "Create#0.TypeText", State: q("query", "text")},
			tracecheck.Step{Action: "Create#0.StartTyped", State: q("ui", "creating", "first", "text", "child", "spawned")})
	} else {
		steps = append(steps, tracecheck.Step{Action: "Create#0.OverviewNew", State: q("ui", "creating", "first", "empty", "child", "spawned")})
	}
	landed := false
	for _, e := range entries {
		switch {
		case e.Kind == "meta":
			steps = append(steps,
				tracecheck.Step{Action: "Create#0.WriteHistory", State: q("hist", true)},
				tracecheck.Step{Action: "Create#0.Claim", State: q("ui", "opened", "child", "live", "sending", typed)})
		case e.Kind == "input" && !landed:
			landed = true
			steps = append(steps, tracecheck.Step{Action: "Create#0.Land", State: q("sending", false, "recorded", true)})
		}
	}
	return steps
}

func init() { historyProjections["session_create"] = sessionCreateHistory }

// walkSessionCreatePaths walks every path in testdata/session_create/
// paths.json (the generator's cover of all 23 states and 51
// transitions, the same walks the browser spec takes) and compares the
// adapter's state with the spec's after each step.
//
// fizzbee-mbt 0.2.0 picks each step uniformly from all 19 actions and
// stops checking at the first one the graph does not enable. Here one
// to four are enabled at a time, so its random walks almost never get
// past the first create's answer; the paths are what reach Claim, Land,
// Timeout after OtherWrites and ChildExits on every run.
func walkSessionCreatePaths(t *testing.T, a *sessionCreateAdapter) error {
	t.Helper()
	b, err := os.ReadFile(filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "session_create", "paths.json"))
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
	// Every path runs, so one run reports every divergence.
	var errs []error
	for i, p := range doc.Paths {
		if err := a.walk(p.Trace); err != nil {
			var names []string
			for _, s := range p.Trace {
				names = append(names, strings.TrimPrefix(s.Action, "Create#0."))
			}
			errs = append(errs, fmt.Errorf("path %d %v: %w", i, names, err))
		}
	}
	return errors.Join(errs...)
}

func (a *sessionCreateAdapter) walk(trace []tracecheck.Step) (err error) {
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	for j, s := range trace {
		if s.Action == "Init" {
			err = a.Init()
		} else {
			_, err = sessionCreateActions["Create"][strings.TrimPrefix(s.Action, "Create#0.")](a, nil)
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", j, s.Action, err)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w", j, s.Action, err)
		}
		for k, v := range s.State {
			f, ok := strings.CutPrefix(k, "Create#0.")
			if ok && got[f] != v {
				return fmt.Errorf("step %d (%s): %s is %v, the spec says %v (state %v)", j, s.Action, f, got[f], v, got)
			}
		}
	}
	return nil
}

// TestSessionCreate lets fizzbee-mbt walk the spec at random against a
// real serve. It rarely gets past a create's answer (see
// walkSessionCreatePaths); TestSessionCreatePaths is the cover.
func TestSessionCreate(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSessionCreateAdapter(t)
	if err := runMBT(t, "session_create", a, sessionCreateActions, sessionCreateOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkSessionCreateHistories(t, a)
}

// TestSessionCreatePaths walks every generated path. It needs no graph
// server, so it does not queue behind the MBT lock; TestSpecFixtures
// keeps paths.json the graph of the spec.
func TestSessionCreatePaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSessionCreateAdapter(t)
	if err := walkSessionCreatePaths(t, a); err != nil {
		t.Fatalf("spec path: %v", err)
	}
	if len(a.ids) == 0 {
		t.Fatal("no path created a session")
	}
	checkSessionCreateHistories(t, a)
}

// Every session a walk created left a transcript that is a path too.
func checkSessionCreateHistories(t *testing.T, a *sessionCreateAdapter) {
	t.Helper()
	g, err := tracecheck.Load(fizzCheck(t, "session_create"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), sessionCreateHistory)
	}
	t.Logf("trace-checked %d created sessions' histories", len(a.ids))
}

// The projection is only a check if a transcript the model forbids is
// refused: a first prompt recorded with no history file before it.
func TestSessionCreateHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(fizzCheck(t, "session_create"))
	if err != nil {
		t.Fatal(err)
	}
	meta := history.Entry{Kind: "meta", Data: map[string]any{"cwd": "/w"}}
	input := history.Entry{Kind: "input", Data: map[string]any{"text": "hi"}}
	done := history.Entry{Kind: "done"}
	for name, es := range map[string][]history.Entry{
		"typed": {meta, input, done, input, done},
		"empty": {meta},
	} {
		if v := g.Check(sessionCreateHistory(es)); v != nil {
			t.Errorf("%s: %v", name, v)
		}
	}
	if v := g.Check(sessionCreateHistory([]history.Entry{input, done})); v == nil {
		t.Error("a prompt recorded without the session's history file passed the trace check")
	} else {
		t.Logf("refused as expected: %v", v)
	}
}

// A run that lets a child the spec kills start instead must fail, or a
// green TestSessionCreatePaths proves nothing.
func TestSessionCreateCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSessionCreateAdapter(t)
	a.exitAsRelease = true
	err := walkSessionCreatePaths(t, a)
	if err == nil {
		t.Fatal("a run whose ChildExits lets the child start passed; the paths are not checking state")
	}
	t.Logf("caught as expected: %v", err)
}
