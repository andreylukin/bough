//go:build !windows

package mbt

import (
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

// specs/session_title_writers.fizz against a real serve: every writer of
// a session's name (this tab's rename, a second tab, a client posting a
// blank title, the small model, a serve restart mid-save) and what this
// tab's page and a second tab show.
//
// The server half is real: hist is history's title, meta is serve's
// title in memory (what the row shows over history's), saved is
// meta.json on disk, title is the row. The page half (sidebar, header,
// tab, the read in flight, the dialog, the second tab) is the adapter's
// own, driven by what it reads off the API: the page's read guard is the
// browser walk's to check.
//
// On the wire: the prompt is literally "prompt" so history's opening line
// needs no mapping, llm-control names every session "control" ("auto"),
// "blank" is three spaces, and "untitled" is what a page shows for an
// empty or blank title.
const (
	stwPrompt = "prompt"
	stwAuto   = "control"
	stwBlank  = "   "
)

// stwAbstract maps a title the server holds to the spec's value.
func stwAbstract(s string) string {
	switch {
	case s == stwAuto:
		return "auto"
	case s != "" && strings.TrimSpace(s) == "":
		return "blank"
	}
	return s
}

// stwWire is the text a spec value posts.
func stwWire(v string) string {
	if v == "blank" {
		return stwBlank
	}
	return v
}

// stwShown is the spec's shown(): what a page shows for a row title.
func stwShown(t string) string {
	if t == "" || t == "blank" {
		return "untitled"
	}
	return t
}

type sessionTitleWritersAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	id   string
	held string // the first turn, held until AutoTitle; "" once over
	turn int
	ids  []string

	sidebar, header, tab string
	inflight             string
	acked                bool
	dialog, typed        string
	other                string

	// dirty is set once a walk's action ran; a walk whose first pick was
	// disabled left the session as Init made it, and the next Init keeps
	// it (a fresh session is a child and a held turn).
	dirty bool

	// handBackNoop is TestSessionTitleWritersCatchesWrongAdapter's bug:
	// SaveLands does not post an empty name, as a page that took an empty
	// dialog for a cancel would, so the name is never handed back.
	handBackNoop bool
}

func newSessionTitleWritersAdapter(t *testing.T) *sessionTitleWritersAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &sessionTitleWritersAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

// Init starts a session named by its opening line: its first turn is
// held, so the small model has not named it yet.
func (a *sessionTitleWritersAdapter) Init() error {
	a.gate.reset()
	if a.id != "" && !a.dirty {
		return nil
	}
	if err := a.Cleanup(); err != nil {
		return err
	}
	a.dirty = false
	name := a.nextTurn()
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), stwPrompt)
	if err != nil {
		return err
	}
	a.id, a.held = row.ID, name
	a.ids = append(a.ids, row.ID)
	control.WaitTaken(a.t, a.dir, name, actionTimeout)
	if err := a.waitHist(stwPrompt); err != nil {
		return err
	}
	a.sidebar, a.header, a.tab = "prompt", "prompt", "prompt"
	a.inflight, a.acked = "none", false
	a.dialog, a.typed, a.other = "closed", "", "prompt"
	return nil
}

func (a *sessionTitleWritersAdapter) nextTurn() string {
	a.turn++
	return fmt.Sprintf("w%05d", a.turn)
}

// Cleanup archives the walk's session, which kills its child: a held
// turn must not outlive the walk, and a shard's serve would otherwise
// keep a hundred idle children.
func (a *sessionTitleWritersAdapter) Cleanup() error {
	if a.id == "" {
		return nil
	}
	if a.held != "" {
		control.Release(a.t, a.dir, a.held)
		a.held = ""
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Archive(ctx, a.id)
	a.id = ""
	return err
}

func (a *sessionTitleWritersAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

func (a *sessionTitleWritersAdapter) GetState() (map[string]any, error) {
	hist, err := a.hist()
	if err != nil {
		return nil, err
	}
	title, err := a.title()
	if err != nil {
		return nil, err
	}
	saved, err := a.saved()
	if err != nil {
		return nil, err
	}
	// The row shows meta when it is non-empty, else history's title; no
	// name serve holds is ever history's, so the row says what meta is.
	meta := ""
	if title != hist {
		meta = title
	}
	return map[string]any{
		"hist": stwAbstract(hist), "meta": stwAbstract(meta), "saved": stwAbstract(saved), "title": stwAbstract(title),
		"sidebar": a.sidebar, "header": a.header, "tab": a.tab,
		"inflight": a.inflight, "acked": a.acked, "dialog": a.dialog, "typed": a.typed, "other": a.other,
	}, nil
}

// hist is history's title as the product derives it (history.List).
func (a *sessionTitleWritersAdapter) hist() (string, error) {
	infos, err := history.List(filepath.Join(a.s.Home, ".bough", "history"))
	if err != nil {
		return "", err
	}
	for _, in := range infos {
		if in.ID == a.id {
			return in.Title, nil
		}
	}
	return "", nil
}

// title is the row as GET /api/sessions/{id} (the page's catch-up) has it.
func (a *sessionTitleWritersAdapter) title() (string, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return "", err
	}
	return row.Title, nil
}

func (a *sessionTitleWritersAdapter) metaPath() string {
	return filepath.Join(a.s.Home, ".bough", "serve", "meta.json")
}

// saved is the session's title in meta.json: what a restart reads back.
func (a *sessionTitleWritersAdapter) saved() (string, error) {
	b, err := os.ReadFile(a.metaPath())
	if errors.Is(err, os.ErrNotExist) {
		return "", nil
	}
	if err != nil {
		return "", err
	}
	var f struct {
		Sessions map[string]serve.SessionMeta `json:"sessions"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		return "", fmt.Errorf("parse %s: %w", a.metaPath(), err)
	}
	return f.Sessions[a.id].Title, nil
}

// shownNow is shown(title) for the row as serve answers it now.
func (a *sessionTitleWritersAdapter) shownNow() (string, error) {
	t, err := a.title()
	return stwShown(stwAbstract(t)), err
}

// waitHist waits for history's title to read want: entries land in the
// transcript a moment after the API answers.
func (a *sessionTitleWritersAdapter) waitHist(want string) error {
	ctx, cancel := actionCtx()
	defer cancel()
	var got string
	for {
		h, err := a.hist()
		if err != nil {
			return err
		}
		if got = h; got == want {
			return nil
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("waiting for history's title %q: still %q", want, got)
		case <-time.After(50 * time.Millisecond):
		}
	}
}

func (a *sessionTitleWritersAdapter) post(text string) error {
	ctx, cancel := actionCtx()
	defer cancel()
	return a.s.Rename(ctx, a.id, stwWire(text))
}

// land is a read's answer replacing the row: every surface moves.
func (a *sessionTitleWritersAdapter) land(t string) {
	s := stwShown(t)
	a.sidebar, a.header, a.tab = s, s, s
}

// AutoTitle is the small model naming the session. The held first turn
// ends; after a restart killed it, the person's next prompt resumes the
// session (llm-control answers an empty queue at once) and that turn's
// end names it.
func (a *sessionTitleWritersAdapter) AutoTitle() error {
	hist, err := a.hist()
	if err != nil {
		return err
	}
	if !a.gate.pass(hist == stwPrompt) {
		return nil
	}
	before, err := a.title()
	if err != nil {
		return err
	}
	if a.held != "" {
		control.Release(a.t, a.dir, a.held)
		a.held = ""
	} else {
		ctx, cancel := actionCtx()
		err := a.s.Prompt(ctx, a.id, "go on")
		cancel()
		if err != nil {
			return err
		}
	}
	if err := a.waitHist(stwAuto); err != nil {
		return err
	}
	after, err := a.title()
	if err != nil {
		return err
	}
	if after != before {
		a.acked = false
	}
	return nil
}

// ReadStart is the list poll or an event's catchUp leaving: it reads the
// row as serve answers it now.
func (a *sessionTitleWritersAdapter) ReadStart() error {
	t, err := a.title()
	if err != nil {
		return err
	}
	t = stwAbstract(t)
	if !a.gate.pass(a.inflight == "none" && a.header != stwShown(t)) {
		return nil
	}
	a.inflight = t
	return nil
}

func (a *sessionTitleWritersAdapter) ReadLand() error {
	if !a.gate.pass(a.inflight != "none") {
		return nil
	}
	a.land(a.inflight)
	a.inflight = "none"
	return nil
}

func (a *sessionTitleWritersAdapter) OpenRename() error {
	if a.gate.pass(a.dialog == "closed") {
		a.dialog = "open"
	}
	return nil
}

// Submit is Rename pressed: the POST leaves; which way it ends is
// SaveLands, SaveFails or ServeRestart.
func (a *sessionTitleWritersAdapter) Submit(args []fmbt.Arg) error {
	text, ok := argOf(args, "text").(string)
	if !ok {
		return fmt.Errorf("Submit: want a string choice \"text\", got %v", args)
	}
	if !a.gate.pass((a.dialog == "open" || a.dialog == "failed") && (text == "" || text != a.header)) {
		return nil
	}
	a.typed, a.dialog = text, "saving"
	return nil
}

// SaveLands is the POST answering; onRename's awaited refresh lands and
// drops every read that started before it.
func (a *sessionTitleWritersAdapter) SaveLands() error {
	if !a.gate.pass(a.dialog == "saving") {
		return nil
	}
	if a.typed != "" || !a.handBackNoop {
		if err := a.post(a.typed); err != nil {
			return err
		}
	}
	t, err := a.title()
	if err != nil {
		return err
	}
	a.land(stwAbstract(t))
	a.inflight, a.acked, a.dialog, a.typed = "none", true, "closed", ""
	return nil
}

// SaveFails makes serve's meta save fail (a directory where it writes
// meta.json.tmp), posts the dialog's text and puts the path back.
func (a *sessionTitleWritersAdapter) SaveFails() error {
	if !a.gate.pass(a.dialog == "saving") {
		return nil
	}
	tmp := a.metaPath() + ".tmp"
	if err := os.MkdirAll(tmp, 0o755); err != nil {
		return err
	}
	perr := a.post(a.typed)
	if err := os.Remove(tmp); err != nil {
		return err
	}
	if perr == nil {
		return fmt.Errorf("SaveFails: rename %q succeeded with meta.json.tmp blocked", a.typed)
	}
	a.dialog, a.typed = "failed", ""
	return nil
}

func (a *sessionTitleWritersAdapter) Dismiss() error {
	if a.gate.pass(a.dialog == "open" || a.dialog == "failed") {
		a.dialog = "closed"
	}
	return nil
}

// RenameElsewhere is a second tab's rename: this tab hears of it only
// from a read.
func (a *sessionTitleWritersAdapter) RenameElsewhere() error {
	saved, err := a.saved()
	if err != nil {
		return err
	}
	if !a.gate.pass(stwAbstract(saved) != "theirs") {
		return nil
	}
	if err := a.post("theirs"); err != nil {
		return err
	}
	s, err := a.shownNow()
	if err != nil {
		return err
	}
	a.acked, a.other = false, s
	return nil
}

// RenameBlank is another client (curl) posting a whitespace-only title.
func (a *sessionTitleWritersAdapter) RenameBlank() error {
	saved, err := a.saved()
	if err != nil {
		return err
	}
	if !a.gate.pass(saved != "") {
		return nil
	}
	if err := a.post("blank"); err != nil {
		return err
	}
	a.acked = false
	return nil
}

// ServeRestart is SIGTERM to serve and a start on the same HOME. With a
// rename out, the runner names the branch: "landed" posts it first (serve
// saved it and died before the page heard the answer), else serve died
// before it saved. Either way the page shows the error. The held first
// turn dies with serve's children.
func (a *sessionTitleWritersAdapter) ServeRestart(args []fmbt.Arg) error {
	if !a.gate.pass(true) {
		return nil
	}
	before, err := a.title()
	if err != nil {
		return err
	}
	if a.dialog == "saving" {
		landed, ok := argOf(args, "landed").(bool)
		if !ok {
			return fmbt.ErrNotImplemented
		}
		if landed {
			if err := a.post(a.typed); err != nil {
				return err
			}
		}
		a.dialog, a.typed = "failed", ""
	}
	a.s.Shutdown()
	a.held = ""
	if err := a.s.Resume(""); err != nil {
		return err
	}
	after, err := a.title()
	if err != nil {
		return err
	}
	if after != before {
		a.acked = false
	}
	a.inflight = "none"
	return nil
}

// PollOther is the second tab's own poll.
func (a *sessionTitleWritersAdapter) PollOther() error {
	s, err := a.shownNow()
	if err != nil {
		return err
	}
	if a.gate.pass(a.other != s) {
		a.other = s
	}
	return nil
}

// stwAction marks the walk dirty once an action ran (the gate was open
// before it and still is).
func stwAction(f func(*sessionTitleWritersAdapter, []fmbt.Arg) error) fmbt.ActionFunc {
	return func(m any, args []fmbt.Arg) (any, error) {
		a := m.(*sessionTitleWritersAdapter)
		open := !a.gate.off
		err := f(a, args)
		if open && !a.gate.off {
			a.dirty = true
		}
		return nil, err
	}
}

func stwNoArgs(f func(*sessionTitleWritersAdapter) error) func(*sessionTitleWritersAdapter, []fmbt.Arg) error {
	return func(a *sessionTitleWritersAdapter, _ []fmbt.Arg) error { return f(a) }
}

var sessionTitleWritersActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"AutoTitle":       stwAction(stwNoArgs((*sessionTitleWritersAdapter).AutoTitle)),
	"ReadStart":       stwAction(stwNoArgs((*sessionTitleWritersAdapter).ReadStart)),
	"ReadLand":        stwAction(stwNoArgs((*sessionTitleWritersAdapter).ReadLand)),
	"OpenRename":      stwAction(stwNoArgs((*sessionTitleWritersAdapter).OpenRename)),
	"Submit":          stwAction((*sessionTitleWritersAdapter).Submit),
	"SaveLands":       stwAction(stwNoArgs((*sessionTitleWritersAdapter).SaveLands)),
	"SaveFails":       stwAction(stwNoArgs((*sessionTitleWritersAdapter).SaveFails)),
	"Dismiss":         stwAction(stwNoArgs((*sessionTitleWritersAdapter).Dismiss)),
	"RenameElsewhere": stwAction(stwNoArgs((*sessionTitleWritersAdapter).RenameElsewhere)),
	"RenameBlank":     stwAction(stwNoArgs((*sessionTitleWritersAdapter).RenameBlank)),
	"ServeRestart":    stwAction((*sessionTitleWritersAdapter).ServeRestart),
	"PollOther":       stwAction(stwNoArgs((*sessionTitleWritersAdapter).PollOther)),
}}

// Most random walks stop at a disabled first pick and cost nothing
// (dirty); the paths below are the cover.
func sessionTitleWritersOptions() map[string]any {
	return map[string]any{"max-seq-runs": 200, "max-actions": 8, "max-parallel-runs": 0}
}

// sessionTitleWritersHistory reads the trace off a transcript. History
// holds no rename, only what the session and the small model wrote: the
// opening line (Init) and each "title" entry (AutoTitle), checked on hist
// alone. A second final name is a second AutoTitle, which the spec never
// enables.
func sessionTitleWritersHistory(entries []history.Entry) []tracecheck.Step {
	hist := func(s string) map[string]any { return map[string]any{"Session#0.hist": s} }
	var steps []tracecheck.Step
	for _, e := range entries {
		switch {
		case e.Kind == "input" && len(steps) == 0:
			steps = append(steps, tracecheck.Step{Action: "Init", State: hist(stwAbstract(history.Prompt(e)))})
		case e.Kind == "title":
			text, _ := e.Data["text"].(string)
			steps = append(steps, tracecheck.Step{Action: "Session#0.AutoTitle", State: hist(stwAbstract(text))})
		}
	}
	return steps
}

func init() { historyProjections["session_title_writers"] = sessionTitleWritersHistory }

func checkSessionTitleWritersHistories(t *testing.T, a *sessionTitleWritersAdapter) {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "session_title_writers"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), sessionTitleWritersHistory)
	}
	t.Logf("trace-checked %d sessions' histories", len(a.ids))
}

func TestSessionTitleWriters(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSessionTitleWritersAdapter(t)
	if err := runMBT(t, "session_title_writers", a, sessionTitleWritersActions, sessionTitleWritersOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	if err := a.Cleanup(); err != nil {
		t.Fatal(err)
	}
	checkSessionTitleWritersHistories(t, a)
}

// stwForkArgs is the choice the runner would pass for a step that forks,
// read off the path's next state.
func stwForkArgs(action string, before, after map[string]any) []fmbt.Arg {
	switch action {
	case "Submit":
		return []fmbt.Arg{{Name: "text", Value: after["typed"]}}
	case "ServeRestart":
		if before["dialog"] == "saving" {
			return []fmbt.Arg{{Name: "landed", Value: after["saved"] == before["typed"]}}
		}
	}
	return nil
}

// walkSessionTitleWritersPath runs one path from Init and compares the
// whole role state after every step.
func walkSessionTitleWritersPath(a *sessionTitleWritersAdapter, p genPath) (err error) {
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	a.dirty = true // every path starts from a fresh session
	var names []string
	for i, st := range p.Trace {
		want := roleState(st.State)
		if i == 0 {
			err = a.Init()
		} else {
			name := strings.TrimPrefix(st.Action, "Session#0.")
			names = append(names, name)
			f := sessionTitleWritersActions["Session"][name]
			if f == nil {
				return fmt.Errorf("no adapter action for %q", st.Action)
			}
			_, err = f(a, stwForkArgs(name, roleState(p.Trace[i-1].State), want))
			if err == nil && a.gate.off {
				err = errors.New("the adapter refused a step the spec enables (its require disagrees)")
			}
		}
		if err != nil {
			return fmt.Errorf("%v: %w", names, err)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("%v: state: %w", names, err)
		}
		if !reflect.DeepEqual(jsonRound(got), jsonRound(want)) {
			return fmt.Errorf("%v: state\n got %v\nwant %v", names, jsonRound(got), jsonRound(want))
		}
	}
	return nil
}

// walkSessionTitleWritersPaths walks the cover's paths in shards, each
// on its own serve; failFast stops a shard at its first divergence.
func walkSessionTitleWritersPaths(t *testing.T, cover tracecheck.Cover, failFast bool, setup func(*sessionTitleWritersAdapter)) []error {
	t.Helper()
	b, err := pathsJSONCover("session_title_writers", cover)
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Paths []genPath `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		t.Fatal(err)
	}
	if len(f.Paths) == 0 {
		t.Fatal("no paths")
	}
	const shards = 4
	errs := make([][]error, shards)
	t.Run("walk", func(t *testing.T) {
		for sh := range shards {
			t.Run(fmt.Sprintf("shard%d", sh), func(t *testing.T) {
				t.Parallel()
				a := newSessionTitleWritersAdapter(t)
				if setup != nil {
					setup(a)
				}
				for i := sh; i < len(f.Paths); i += shards {
					if err := walkSessionTitleWritersPath(a, f.Paths[i]); err != nil {
						errs[sh] = append(errs[sh], fmt.Errorf("path %d %w", i, err))
						if failFast {
							return
						}
					}
				}
				if setup == nil {
					checkSessionTitleWritersHistories(t, a)
				}
			})
		}
	})
	var all []error
	for _, e := range errs {
		all = append(all, e...)
	}
	t.Logf("%d paths (%s) in %d shards", len(f.Paths), cover, shards)
	return all
}

// TestSessionTitleWritersPaths walks every generated path against a real
// serve (every state; MODEL_COVER=transitions takes every link) and
// trace-checks each session's history.
func TestSessionTitleWritersPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	if errs := walkSessionTitleWritersPaths(t, envCover(), false, nil); len(errs) > 0 {
		t.Fatalf("%d paths diverged:\n%v", len(errs), errors.Join(errs...))
	}
}

// A SaveLands that never posts an empty name leaves the rename in place
// while the spec hands the name back to history. It shows on one link
// (SaveLands with "" typed over a name serve holds), so every link is
// walked; a shard stops at its first divergence.
func TestSessionTitleWritersCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	errs := walkSessionTitleWritersPaths(t, tracecheck.CoverTransitions, true, func(a *sessionTitleWritersAdapter) { a.handBackNoop = true })
	if len(errs) == 0 {
		t.Fatal("a run whose SaveLands never hands the name back passed; the paths are not checking state")
	}
	t.Logf("caught as expected: %v", errs[0])
}

// The projection is only a check if a transcript the model forbids is
// refused: a second final name.
func TestSessionTitleWritersHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "session_title_writers"))
	if err != nil {
		t.Fatal(err)
	}
	input := history.Entry{Kind: "input", Data: map[string]any{"text": stwPrompt}}
	title := history.Entry{Kind: "title", Data: map[string]any{"text": stwAuto, "final": true}}
	done := history.Entry{Kind: "done"}
	for name, es := range map[string][]history.Entry{
		"unnamed": {input},
		"named":   {input, done, title, input, done},
	} {
		if v := g.Check(sessionTitleWritersHistory(es)); v != nil {
			t.Errorf("%s: %v", name, v)
		}
	}
	if v := g.Check(sessionTitleWritersHistory([]history.Entry{input, done, title, title})); v == nil {
		t.Error("a second final name passed the trace check")
	} else {
		t.Logf("refused as expected: %v", v)
	}
}
