//go:build !windows

package mbt

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/me_page.fizz: the Me page (#/me) against a real serve. The
// brief job is the real `bough wiki brief` the serve spawns, with its
// headless session answered by llm-control: the job holds the wiki
// lock while its one model turn is held, so the adapter decides when
// it ends and how. llm-control cannot call tools, so on BriefWritten
// the adapter makes the agent's writes (today's brief, signals.json)
// before it releases the turn; on BriefFailed the turn fails and
// nothing is written.
//
// view and refreshing are the page's own state, so the adapter keeps
// them; everything else is read back from GET /api/me and the lock.

// meSignalKey is the one row's key: its cite (signalKey in me.tsx).
const meSignalKey = "gh:acme/web#7"

type mePageAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	view       string // loading | error | shown
	refreshing bool
	turn       int    // brief turn names, unique across walks
	held       string // the brief job's held turn, "" when no job
	daysBack   int    // NextDay renames today's brief this many days back

	// The deliberate bugs the CatchesWrongAdapter tests inject.
	// loadFailOK: LoadFail sends the real token, so its read succeeds
	// (shallow enough for the runner's random walks to reach).
	// failWrites: BriefFailed makes the agent's writes before failing
	// (for the path walk, which reaches it).
	loadFailOK, failWrites bool

	ran  map[string]int // actions taken past the gate, by name
	errs []string       // action errors: fmbt's runner drops them, see meAct
}

func newMePageAdapter(t *testing.T) *mePageAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &mePageAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

func (a *mePageAdapter) wiki() string    { return filepath.Join(a.s.Home, ".bough", "wiki") }
func (a *mePageAdapter) meDir() string   { return filepath.Join(a.wiki(), "topics", "me") }
func (a *mePageAdapter) today() string   { return time.Now().Format("2006-01-02") }
func (a *mePageAdapter) logPath() string { return filepath.Join(a.wiki(), "ingest.log") }

// Init starts a walk with no profile, no brief and no rows: topics/me
// is the whole of the page's disk state, so removing it is a fresh page.
func (a *mePageAdapter) Init() error {
	if err := os.RemoveAll(a.meDir()); err != nil {
		return err
	}
	a.view, a.refreshing, a.held, a.daysBack = "loading", false, "", 0
	a.gate.reset()
	return nil
}

// Cleanup ends a job the walk left running, so its lock and its turn
// do not leak into the next walk.
func (a *mePageAdapter) Cleanup() error {
	if a.held == "" {
		return nil
	}
	control.ReleaseWith(a.t, a.dir, a.held, control.Turn{Mode: "error", Error: "walk over"})
	a.held = ""
	return a.waitJobEnded()
}

func (a *mePageAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Me", Index: 0}: a}, nil
}

// meData is the part of GET /api/me the spec's fields come from.
type meData struct {
	HasProfile bool     `json:"hasProfile"`
	Stale      bool     `json:"stale"`
	Days       []string `json:"days"`
	Signals    *struct {
		Items []struct {
			Cite string `json:"cite"`
		} `json:"items"`
	} `json:"signals"`
	Triage meTriage `json:"triage"`
}

type meTriage struct {
	Dismissed map[string]string `json:"dismissed"`
	Pinned    []string          `json:"pinned"`
}

// signal is the row's state as the page shows it: absent, or its triage.
func (t meTriage) signal(listed bool) string {
	switch {
	case !listed:
		return "none"
	case t.Dismissed[meSignalKey] != "":
		return "dismissed"
	}
	for _, k := range t.Pinned {
		if k == meSignalKey {
			return "pinned"
		}
	}
	return "shown"
}

func (d meData) brief() string {
	switch {
	case len(d.Days) == 0:
		return "none"
	case d.Stale:
		return "stale"
	}
	return "today"
}

func (d meData) signal() string {
	listed := false
	if d.Signals != nil {
		for _, it := range d.Signals.Items {
			listed = listed || it.Cite == meSignalKey
		}
	}
	return d.Triage.signal(listed)
}

func (a *mePageAdapter) me() (meData, error) {
	var d meData
	err := a.call(http.MethodGet, "/api/me", a.s.Token, nil, &d)
	return d, err
}

// GetState: profile, brief and signal from GET /api/me; job from the
// wiki lock, which a brief run holds from start to exit; view and
// refreshing are the adapter's, being the page.
func (a *mePageAdapter) GetState() (map[string]any, error) {
	d, err := a.me()
	if err != nil {
		return nil, err
	}
	job := "idle"
	if a.lockHeld() {
		job = "running"
	}
	return map[string]any{
		"view": a.view, "profile": d.HasProfile, "brief": d.brief(), "job": job,
		"refreshing": a.refreshing, "signal": d.signal(),
	}, nil
}

// lockHeld probes the wiki's .ingest.lock the way the next run would:
// a non-blocking flock that is dropped at once when it succeeds.
func (a *mePageAdapter) lockHeld() bool {
	f, err := os.OpenFile(filepath.Join(a.wiki(), ".ingest.lock"), os.O_RDWR, 0)
	if err != nil {
		return false
	}
	defer f.Close()
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		return true
	}
	syscall.Flock(int(f.Fd()), syscall.LOCK_UN)
	return false
}

func (a *mePageAdapter) waitJobEnded() error {
	deadline := time.Now().Add(actionTimeout)
	for a.lockHeld() {
		if time.Now().After(deadline) {
			return fmt.Errorf("brief job still holds the wiki lock after %s; ingest.log:\n%s", actionTimeout, a.log())
		}
		time.Sleep(20 * time.Millisecond)
	}
	return nil
}

func (a *mePageAdapter) log() string {
	b, _ := os.ReadFile(a.logPath())
	return string(b)
}

// call is one API request; token is the bearer sent, so LoadFail can
// send a wrong one.
func (a *mePageAdapter) call(method, path, token string, body, out any) error {
	ctx, cancel := actionCtx()
	defer cancel()
	var rd io.Reader
	if body != nil {
		b, _ := json.Marshal(body)
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, a.s.URL+path, rd)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	if resp.StatusCode/100 != 2 {
		return &servetest.APIError{Status: resp.StatusCode, Msg: strings.TrimSpace(string(raw))}
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(raw, out)
}

// quiet is the spec's "page at rest" part of the triage and steer
// requires.
func (a *mePageAdapter) quiet() bool { return a.view == "shown" && a.held == "" && !a.refreshing }

func (a *mePageAdapter) Load() error {
	if !a.gate.pass(a.view == "loading") {
		return nil
	}
	return a.firstRead(a.s.Token)
}

// LoadFail is the page's first read failing. The server is asked with
// a token it does not know, so the read really fails.
func (a *mePageAdapter) LoadFail() error {
	if !a.gate.pass(a.view == "loading") {
		return nil
	}
	token := "not-the-token"
	if a.loadFailOK {
		token = a.s.Token
	}
	return a.firstRead(token)
}

// firstRead is useLoad's first GET /api/me: the page shows what it got,
// or the error state when the server refused it.
func (a *mePageAdapter) firstRead(token string) error {
	err := a.call(http.MethodGet, "/api/me", token, nil, nil)
	var e *servetest.APIError
	switch {
	case err == nil:
		a.view = "shown"
	case errors.As(err, &e):
		a.view = "error"
	default:
		return err
	}
	return nil
}

func (a *mePageAdapter) Retry() error {
	if a.gate.pass(a.view == "error") {
		a.view = "loading"
	}
	return nil
}

// WriteProfile is the empty state's "Write your profile": it opens
// topics/me/profile.md in the wiki page view and saves it from the
// editor there.
func (a *mePageAdapter) WriteProfile() error {
	d, err := a.me()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.view == "shown" && !d.HasProfile) {
		return nil
	}
	const path = "topics/me/profile.md"
	var page struct {
		Body string `json:"body"`
	}
	if err := a.call(http.MethodGet, "/api/wiki/page?path="+url.QueryEscape(path), a.s.Token, nil, &page); err != nil {
		return fmt.Errorf("open %s: %w", path, err)
	}
	body := page.Body + "\nI own acme/web.\n"
	return a.call(http.MethodPut, "/api/wiki/page", a.s.Token, map[string]string{"path": path, "body": body}, nil)
}

// Refresh is the button: POST /api/me/refresh and the 20 s spinner.
// With no job running it starts one whose model turn is held; with one
// running, the new `bough wiki brief` loses the lock and exits, which
// the adapter waits to see in ingest.log so no stray run is left to
// take a later walk's turn.
func (a *mePageAdapter) Refresh() error {
	d, err := a.me()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.view == "shown" && d.HasProfile && !a.refreshing) {
		return nil
	}
	a.refreshing = true
	if a.held != "" {
		before := strings.Count(a.log(), "already running")
		if err := a.call(http.MethodPost, "/api/me/refresh", a.s.Token, nil, nil); err != nil {
			return err
		}
		deadline := time.Now().Add(actionTimeout)
		for strings.Count(a.log(), "already running") <= before {
			if time.Now().After(deadline) {
				return fmt.Errorf("second brief run did not report the held lock; ingest.log:\n%s", a.log())
			}
			time.Sleep(20 * time.Millisecond)
		}
		return nil
	}
	a.turn++
	name := fmt.Sprintf("b%04d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "brief " + name})
	if err := a.call(http.MethodPost, "/api/me/refresh", a.s.Token, nil, nil); err != nil {
		return err
	}
	a.held = name
	control.WaitTaken(a.t, a.dir, name, actionTimeout)
	return nil
}

// Settle is the spinner's timer: off, and one reload.
func (a *mePageAdapter) Settle() error {
	if !a.gate.pass(a.refreshing) {
		return nil
	}
	a.refreshing = false
	_, err := a.me()
	return err
}

// BriefWritten plays the brief agent's writes, then lets its turn end.
func (a *mePageAdapter) BriefWritten() error {
	if !a.gate.pass(a.held != "") {
		return nil
	}
	if err := a.writeBrief(); err != nil {
		return err
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	return a.waitJobEnded()
}

// writeBrief is what the agent writes: today's brief and signals.json
// with the one row. Earlier days' briefs are left alone.
func (a *mePageAdapter) writeBrief() error {
	briefs := filepath.Join(a.meDir(), "briefs")
	if err := os.MkdirAll(briefs, 0o755); err != nil {
		return err
	}
	brief := "# " + a.today() + "\n\nReviewing acme/web#7 today. `gh:acme/web#7`\n"
	if err := os.WriteFile(filepath.Join(briefs, a.today()+".md"), []byte(brief), 0o644); err != nil {
		return err
	}
	signals := `{"asOf":"` + time.Now().Format(time.RFC3339) + `","items":[{"kind":"needs-you","source":"github","title":"Review acme/web#7","repo":"acme/web","author":"dependabot","cite":"` + meSignalKey + `"}],"sources":[{"name":"github","ok":true}]}`
	return os.WriteFile(filepath.Join(a.meDir(), "signals.json"), []byte(signals), 0o644)
}

func (a *mePageAdapter) BriefFailed() error {
	if !a.gate.pass(a.held != "") {
		return nil
	}
	if a.failWrites {
		if err := a.writeBrief(); err != nil {
			return err
		}
	}
	control.ReleaseWith(a.t, a.dir, a.held, control.Turn{Mode: "error", Error: "brief agent failed"})
	a.held = ""
	return a.waitJobEnded()
}

// NextDay moves today's brief to an earlier date, as midnight would
// leave it. Each one goes a day further back, so none overwrites another.
func (a *mePageAdapter) NextDay() error {
	d, err := a.me()
	if err != nil {
		return err
	}
	if !a.gate.pass(d.brief() == "today" && a.held == "" && !a.refreshing) {
		return nil
	}
	a.daysBack++
	briefs := filepath.Join(a.meDir(), "briefs")
	past := time.Now().AddDate(0, 0, -a.daysBack).Format("2006-01-02")
	return os.Rename(filepath.Join(briefs, a.today()+".md"), filepath.Join(briefs, past+".md"))
}

// triage is one row menu choice; the page shows the answer's triage at
// once, so the answer must already say want.
func (a *mePageAdapter) triage(from []string, action, rule, want string) error {
	d, err := a.me()
	if err != nil {
		return err
	}
	sig := d.signal()
	ok := false
	for _, f := range from {
		ok = ok || sig == f
	}
	if !a.gate.pass(a.quiet() && ok) {
		return nil
	}
	var ans struct {
		Triage meTriage `json:"triage"`
	}
	if err := a.call(http.MethodPost, "/api/me/triage", a.s.Token, map[string]string{"action": action, "key": meSignalKey, "rule": rule}, &ans); err != nil {
		return err
	}
	if got := ans.Triage.signal(true); got != want {
		return fmt.Errorf("triage %s answered a row that is %s, want %s", action, got, want)
	}
	return nil
}

func (a *mePageAdapter) Pin() error   { return a.triage([]string{"shown"}, "pin", "", "pinned") }
func (a *mePageAdapter) Unpin() error { return a.triage([]string{"pinned"}, "unpin", "", "shown") }

// Dismiss takes the menu's "anything by this author" choice, so the
// rule half of the request runs too.
func (a *mePageAdapter) Dismiss() error {
	return a.triage([]string{"shown", "pinned"}, "dismiss", "Skip anything by dependabot", "dismissed")
}

// steer sends a sentence from the steering line and checks the section
// the confirmation names: the spec's Steer actions change no state.
func (a *mePageAdapter) steer(text, want string) error {
	d, err := a.me()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.quiet() && d.HasProfile && d.brief() != "none") {
		return nil
	}
	var ans struct {
		Section string `json:"section"`
	}
	if err := a.call(http.MethodPost, "/api/me/steer", a.s.Token, map[string]string{"text": text}, &ans); err != nil {
		return err
	}
	if ans.Section != want {
		return fmt.Errorf("steer %q filed under %q, want %q", text, ans.Section, want)
	}
	return nil
}

func (a *mePageAdapter) SteerWatch() error {
	return a.steer("Keep an eye on the release train", "Watch")
}

func (a *mePageAdapter) SteerNotMine() error {
	return a.steer("Ignore the docs site", "Not mine")
}

// mePageActions wraps each action to count the ones a walk really
// took (past the gate): the runner picks at random, and the test says
// which actions it never reached rather than passing silently.
var mePageActions = map[string]map[string]fmbt.ActionFunc{"Me": {
	"Load":         meAct("Load", (*mePageAdapter).Load),
	"LoadFail":     meAct("LoadFail", (*mePageAdapter).LoadFail),
	"Retry":        meAct("Retry", (*mePageAdapter).Retry),
	"WriteProfile": meAct("WriteProfile", (*mePageAdapter).WriteProfile),
	"Refresh":      meAct("Refresh", (*mePageAdapter).Refresh),
	"Settle":       meAct("Settle", (*mePageAdapter).Settle),
	"BriefWritten": meAct("BriefWritten", (*mePageAdapter).BriefWritten),
	"BriefFailed":  meAct("BriefFailed", (*mePageAdapter).BriefFailed),
	"NextDay":      meAct("NextDay", (*mePageAdapter).NextDay),
	"Pin":          meAct("Pin", (*mePageAdapter).Pin),
	"Unpin":        meAct("Unpin", (*mePageAdapter).Unpin),
	"Dismiss":      meAct("Dismiss", (*mePageAdapter).Dismiss),
	"SteerWatch":   meAct("SteerWatch", (*mePageAdapter).SteerWatch),
	"SteerNotMine": meAct("SteerNotMine", (*mePageAdapter).SteerNotMine),
}}

func meAct(name string, f func(*mePageAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*mePageAdapter)
		err := f(a)
		if err != nil {
			// The 0.2.0 runner does not fail a walk on an action's
			// error (a failing WriteProfile passed 100 walks), so the
			// adapter keeps them for the test and ends the walk here.
			a.errs = append(a.errs, name+": "+err.Error())
			a.gate.off = true
			return nil, err
		}
		if !a.gate.off {
			if a.ran == nil {
				a.ran = map[string]int{}
			}
			a.ran[name]++
		}
		return nil, err
	}
}

func mePageOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 10, "max-parallel-runs": 0}
}

// mePageHistory reads a brief job's headless transcript: the job's one
// input is the Refresh that started it, and its close is BriefFailed
// when an error came inside the turn, else BriefWritten. A transcript
// is one job, so the trace is the shortest path to one: Load,
// WriteProfile, Refresh, then the ending.
func mePageHistory(entries []history.Entry) []tracecheck.Step {
	job := func(s string) map[string]any { return map[string]any{"Me#0.job": s} }
	steps := []tracecheck.Step{
		{Action: "Init", State: job("idle")},
		{Action: "Me#0.Load", State: job("idle")},
		{Action: "Me#0.WriteProfile", State: job("idle")},
	}
	failed := false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			failed = false
			steps = append(steps, tracecheck.Step{Action: "Me#0.Refresh", State: job("running")})
		case "error":
			failed = true
		case "done":
			end := "Me#0.BriefWritten"
			if failed {
				end = "Me#0.BriefFailed"
			}
			steps = append(steps, tracecheck.Step{Action: end, State: job("idle")})
		}
	}
	return steps
}

func init() { historyProjections["me_page"] = mePageHistory }

// briefSessions is every headless session a brief job wrote: they run
// with the wiki as their cwd.
func briefSessions(t *testing.T, home string) []string {
	t.Helper()
	files, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
	var ids []string
	for _, f := range files {
		ids = append(ids, strings.TrimSuffix(filepath.Base(f), ".jsonl"))
	}
	return ids
}

func TestMePage(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newMePageAdapter(t)
	if err := runMBT(t, "me_page", a, mePageActions, mePageOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	if len(a.errs) > 0 {
		t.Fatalf("actions failed during the run:\n%s", strings.Join(a.errs, "\n"))
	}
	g, err := tracecheck.Load(fizzCheck(t, "me_page"))
	if err != nil {
		t.Fatal(err)
	}
	ids := briefSessions(t, a.s.Home)
	if a.turn > 0 && len(ids) == 0 {
		t.Fatalf("%d brief jobs ran and no transcript was written", a.turn)
	}
	for _, id := range ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), mePageHistory)
	}
	t.Logf("%d brief jobs, %d transcripts checked; actions taken: %v", a.turn, len(ids), a.ran)
}

// A first read that fails but shows the page must fail the run.
func TestMePageCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newMePageAdapter(t)
	a.loadFailOK = true
	if err := runMBT(t, "me_page", a, mePageActions, mePageOptions()); err == nil {
		t.Fatal("a run whose LoadFail read succeeds passed; the runner is not checking state")
	}
}

// A failed brief that still wrote today's brief must fail the walk.
func TestMePagePathsCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newMePageAdapter(t)
	a.failWrites = true
	for _, p := range loadMePaths(t, "me_page") {
		if err := walkMePath(a, p); err != nil {
			t.Logf("caught: %v", err)
			return
		}
	}
	t.Fatal("every path passed with BriefFailed writing a brief; the walk is not checking state")
}

// The runner's walks are uniform over all 14 actions and end at the
// first disabled one, so they rarely get past Load (100 walks took
// Load 5 times and never reached Refresh). TestMePagePaths walks every
// path the generator wrote for the browser (testdata/me_page/paths.json,
// every transition at least once) against the same adapter, checking
// the state after each step as the runner does.
func TestMePagePaths(t *testing.T) {
	t.Parallel()
	a := newMePageAdapter(t)
	for i, p := range loadMePaths(t, "me_page") {
		if err := walkMePath(a, p); err != nil {
			t.Fatalf("path %d (target %d): %v", i, p.Target, err)
		}
	}
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("me_page")), "..", "testdata", "me_page"))
	if err != nil {
		t.Fatal(err)
	}
	ids := briefSessions(t, a.s.Home)
	if len(ids) != a.turn {
		t.Errorf("%d brief jobs ran, %d transcripts written", a.turn, len(ids))
	}
	for _, id := range ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), mePageHistory)
	}
	t.Logf("%d brief jobs, %d transcripts checked; actions taken: %v", a.turn, len(ids), a.ran)
}

type modelPath struct {
	Target int `json:"target"`
	Trace  []struct {
		Action string         `json:"action"`
		State  map[string]any `json:"state"`
	} `json:"trace"`
}

func loadMePaths(t *testing.T, spec string) []modelPath {
	t.Helper()
	b, err := os.ReadFile(filepath.Join(filepath.Dir(specPath(spec)), "..", "testdata", spec, "paths.json"))
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Paths []modelPath `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		t.Fatal(err)
	}
	return f.Paths
}

// walkMePath runs one path from Init, comparing the adapter's state to
// the path's after every step. It always ends the walk's job.
func walkMePath(a *mePageAdapter, p modelPath) (err error) {
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	for i, step := range p.Trace {
		var aerr error
		if step.Action == "Init" {
			aerr = a.Init()
		} else {
			name := strings.TrimPrefix(step.Action, "Me#0.")
			f, ok := mePageActions["Me"][name]
			if !ok {
				return fmt.Errorf("step %d: no adapter action %q", i, step.Action)
			}
			_, aerr = f(a, nil)
			if aerr == nil && a.gate.off {
				aerr = fmt.Errorf("the adapter found it disabled")
			}
		}
		if aerr != nil {
			return fmt.Errorf("step %d (%s): %w", i, step.Action, aerr)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w", i, step.Action, err)
		}
		for k, want := range step.State {
			field, ok := strings.CutPrefix(k, "Me#0.")
			if !ok {
				continue
			}
			if got[field] != want {
				return fmt.Errorf("step %d (%s): %s = %v, model says %v (state %v)", i, step.Action, field, got[field], want, got)
			}
		}
	}
	return nil
}
