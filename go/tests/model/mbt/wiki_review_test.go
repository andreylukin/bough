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
	"os/exec"
	"path/filepath"
	"reflect"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/servepid"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/wiki"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/wiki_review.fizz against a real serve: the wiki's pages,
// Review's claim decisions and the ingests Activity starts.
//
// The ingest is the real one. POST /api/wiki/ingest spawns `bough wiki
// run`, which takes the wiki lock and runs a headless ingest session
// whose model is llm-control, so each run can be held in "Ingesting"
// and then landed or stopped. What the ingest agent would write with
// its tools (the page, the log.md heading) the adapter writes while the
// run's turn is held: llm-control answers with text only.

const (
	wrPage    = "topics/demo/alpha.md"
	wrMissing = "topics/demo/none.md"
	wrSeed    = "seed-session"  // the good citation's session
	wrGhost   = "ghost-session" // the dangling citation's: no such file
)

// wrPageBody is the one page. The lede carries the dangling citation
// (a claim with one would be unsupported whatever Review decided), the
// first fact the good one, and the second fact is the flagged claim:
// uncited, so MarkInference and Drop both clear it. version is what an
// ingest rewrote it to; every Land writes the claim's lines anew.
func wrPageBody(version int) string {
	return "# Alpha\n\nThe demo page, begun in `" + wrGhost + "#3`.\n\n## Facts\n\n" +
		"- The good fact rests on `" + wrSeed + "#2`.\n" +
		fmt.Sprintf("- The flagged fact, version %d, has no entry behind it.\n", version)
}

// wikiReviewAdapter plays the control room's wiki screens against one
// serve. Screen, Back and each screen's own state (Review's list and
// its per-claim error, Activity's note, the nav badge) are the page's,
// so the adapter keeps them; the store's state is read off the API.
type wikiReviewAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	wiki string // HOME/.bough/wiki
	pid  int    // serve's pid: the `wiki run`s it spawns are its children
	gate gate

	// The page.
	screen, back string
	listed       bool
	shown        wiki.Flag // the claim Review shows, when listed
	conflict     bool
	note         string
	badge        bool
	body         string // the editor's text

	// The world.
	spawned bool     // Ingest clicked, the run not yet at the lock
	only    string   // the session a per-session Ingest names
	version int      // what the page's flagged claim says
	pends   []string // the sessions this walk finished
	nsess   int
	turn    int
	held    []wrRun // ingest runs holding a model turn

	// bug is a deliberate wiring bug a CatchesWrongAdapter test injects:
	// "nav-forgets-back" or "land-skips-log".
	bug string
}

// wrRun is one `bough wiki run` whose headless ingest has taken turn.
type wrRun struct {
	turn string
	pid  int
	ids  []string
}

func newWikiReviewAdapter(t *testing.T) *wikiReviewAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &wikiReviewAdapter{t: t, s: s, dir: control.Dir(s.Home), wiki: filepath.Join(s.Home, ".bough", "wiki")}
	b, err := os.ReadFile(filepath.Join(s.Home, ".bough", "serve.pid"))
	if err != nil {
		t.Fatal(err)
	}
	if a.pid, _, _, _, _, err = servepid.Parse(string(b)); err != nil {
		t.Fatal(err)
	}
	if err := a.seedHistory(); err != nil {
		t.Fatal(err)
	}
	return a
}

// seedHistory writes the session the good citation names, dated before
// the wiki's baseline so it is never pending.
func (a *wikiReviewAdapter) seedHistory() error {
	at := time.Now().Add(-72 * time.Hour)
	return writeSession(filepath.Join(a.s.Home, ".bough", "history", wrSeed+".jsonl"), at, "how the demo works")
}

// writeSession writes a finished session's transcript by hand (input,
// assistant, done: seq 1..3) and dates the file at, so FindPending sees
// it as quiet.
func writeSession(path string, at time.Time, prompt string) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	var buf bytes.Buffer
	for i, e := range []history.Entry{
		{Kind: "input", Data: map[string]any{"text": prompt}},
		{Kind: "assistant", Data: map[string]any{"text": "it works like this"}},
		{Kind: "done", Data: map[string]any{}},
	} {
		e.Seq, e.At, e.Parent = int64(i+1), at, int64(i)
		b, err := json.Marshal(e)
		if err != nil {
			return err
		}
		buf.Write(append(b, '\n'))
	}
	if err := os.WriteFile(path, buf.Bytes(), 0o644); err != nil {
		return err
	}
	return os.Chtimes(path, at, at)
}

// Init puts the store back as seeded: the one page at version 0, the
// index listing it, a log whose baseline predates nothing pending, and
// no session waiting. Runs the last walk left are ended by Cleanup.
func (a *wikiReviewAdapter) Init() error {
	// The log is rewritten below, so a session the last walk ingested
	// would be pending again: its transcript goes.
	for _, id := range a.pends {
		if err := os.Remove(filepath.Join(a.s.Home, ".bough", "history", id+".jsonl")); err != nil {
			return err
		}
	}
	a.screen, a.back, a.listed, a.shown, a.conflict, a.note, a.badge, a.body = "list", "", false, wiki.Flag{}, false, "", true, ""
	a.spawned, a.only, a.version, a.pends, a.held = false, "", 0, nil, nil
	a.gate.reset()
	files := map[string]string{
		wrPage:     wrPageBody(0),
		"index.md": "# Wiki index\n\n## demo\n\n- [Alpha](" + wrPage + ") — the demo page\n",
		"log.md":   "# Wiki log\n\n<!-- baseline: " + time.Now().Add(-48*time.Hour).UTC().Format(time.RFC3339) + " -->\n",
	}
	for rel, body := range files {
		p := filepath.Join(a.wiki, filepath.FromSlash(rel))
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			return err
		}
		if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
			return err
		}
	}
	return nil
}

// Cleanup stops any run the walk left holding a turn, so the next walk
// starts with the lock free.
func (a *wikiReviewAdapter) Cleanup() error {
	for len(a.held) > 0 {
		if err := a.end(false); err != nil {
			return err
		}
	}
	return nil
}

func (a *wikiReviewAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Wiki", Index: 0}: a}, nil
}

// store is the part of the state the server holds.
type wrStore struct {
	flagged   bool
	flags     []wiki.Flag // Review's claim flags
	pending   []wiki.PendingRef
	ingesting int
}

func (a *wikiReviewAdapter) store() (wrStore, error) {
	var st wrStore
	var rv wiki.Review
	if err := a.api(http.MethodGet, "/api/wiki/review", nil, &rv); err != nil {
		return st, err
	}
	// Review also lists `wiki check` problems (the lede's dangling
	// citation is one); the spec's flagged claim is a claim flag.
	for _, f := range rv.Flags {
		if f.Kind != "problem" {
			st.flags = append(st.flags, f)
		}
	}
	st.flagged, st.pending = len(st.flags) > 0, rv.Pending
	var act wiki.Activity
	if err := a.api(http.MethodGet, "/api/wiki/activity", nil, &act); err != nil {
		return st, err
	}
	for _, r := range act.Runs {
		if r.Running {
			st.ingesting++
		}
	}
	return st, nil
}

// GetState reports the Wiki role. came is always back (every move
// pushes, Back records what it returns over), and the two ghosts are
// claims the adapter cannot see broken from here: false.
func (a *wikiReviewAdapter) GetState() (map[string]any, error) {
	st, err := a.store()
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"screen": a.screen, "back": a.back, "came": a.back,
		"flagged": st.flagged, "listed": a.listed, "moved": a.moved(st),
		"conflict": a.conflict, "pending": len(st.pending) > 0,
		"spawned": a.spawned, "ingesting": st.ingesting, "note": a.note,
		"badge": a.badge, "injected": false, "stalewrite": false,
	}, nil
}

// moved: the claim Review shows is no longer at those lines as shown.
func (a *wikiReviewAdapter) moved(st wrStore) bool {
	return a.screen == "review" && a.listed && !slices.ContainsFunc(st.flags, func(f wiki.Flag) bool {
		return f.Page == a.shown.Page && f.Line == a.shown.Line && f.End == a.shown.End && f.Raw == a.shown.Raw
	})
}

// view is the state the gates read: the page's own plus the store's.
func (a *wikiReviewAdapter) view() wrStore {
	st, err := a.store()
	if err != nil {
		a.t.Logf("wiki_review: reading the store for a gate: %v", err)
	}
	return st
}

func (a *wikiReviewAdapter) settled(st wrStore) bool {
	return !a.spawned && st.ingesting == 0 && a.badge == st.flagged
}

func (a *wikiReviewAdapter) seeded(st wrStore) bool {
	return a.settled(st) && st.flagged && len(st.pending) == 0
}

var wrLive = []string{"review", "activity"}

func live(screen string) bool { return slices.Contains(wrLive, screen) }

// --- moving ---------------------------------------------------------

// arrive enters a screen: Review reads the store afresh, and every
// screen's own state from the last visit is gone.
func (a *wikiReviewAdapter) arrive(to string) error {
	a.listed, a.shown, a.conflict, a.note, a.screen = false, wiki.Flag{}, false, "", to
	switch to {
	case "review":
		return a.readReview()
	case "activity":
		var act wiki.Activity
		return a.api(http.MethodGet, "/api/wiki/activity", nil, &act)
	case "index":
		var ix wiki.Index
		return a.api(http.MethodGet, "/api/wiki", nil, &ix)
	}
	return nil
}

func (a *wikiReviewAdapter) goTo(to string) error {
	a.back = a.screen
	return a.arrive(to)
}

// readReview is Review's read: the first claim flag is the one shown.
func (a *wikiReviewAdapter) readReview() error {
	st, err := a.store()
	if err != nil {
		return err
	}
	a.listed = len(st.flags) > 0
	if a.listed {
		a.shown = st.flags[0]
	}
	return nil
}

func (a *wikiReviewAdapter) Nav() error {
	if !a.gate.pass(a.screen == "list") {
		return nil
	}
	if a.bug == "nav-forgets-back" {
		return a.arrive("index")
	}
	return a.goTo("index")
}

// openPage is the page view's load: a page that is not there is the
// 404 screen, whatever the adapter meant to open.
func (a *wikiReviewAdapter) openPage(path string) error {
	a.back = a.screen
	a.arrive("page")
	var pg wiki.Page
	err := a.api(http.MethodGet, "/api/wiki/page?path="+url.QueryEscape(path), nil, &pg)
	if status(err) == http.StatusNotFound {
		a.screen = "missing"
		return nil
	}
	return err
}

func (a *wikiReviewAdapter) OpenPage() error {
	if !a.gate.pass((a.screen == "index" || a.screen == "review") && a.seeded(a.view())) {
		return nil
	}
	return a.openPage(wrPage)
}

func (a *wikiReviewAdapter) OpenMissing() error {
	if !a.gate.pass(a.screen == "index" && a.seeded(a.view())) {
		return nil
	}
	return a.openPage(wrMissing)
}

// cite opens the source pane on the page's citation that pick chooses:
// a 404 from /api/wiki/source is the pane's error state.
func (a *wikiReviewAdapter) cite(pick func(wiki.Cite) bool) error {
	var pg wiki.Page
	if err := a.api(http.MethodGet, "/api/wiki/page?path="+url.QueryEscape(wrPage), nil, &pg); err != nil {
		return err
	}
	var c *wiki.Cite
	for _, b := range pg.Blocks {
		for i := range b.Cites {
			if c == nil && pick(b.Cites[i]) {
				c = &b.Cites[i]
			}
		}
	}
	if c == nil {
		return fmt.Errorf("the page has no such citation: %s", pg.Body)
	}
	a.back = a.screen
	a.arrive("source")
	var src wiki.Source
	err := a.api(http.MethodGet, fmt.Sprintf("/api/wiki/source?session=%s&seq=%d", url.QueryEscape(c.Session), c.Seq), nil, &src)
	if status(err) == http.StatusNotFound {
		a.screen = "source_error"
		return nil
	}
	return err
}

func (a *wikiReviewAdapter) Cite() error {
	if !a.gate.pass(a.screen == "page") {
		return nil
	}
	return a.cite(func(c wiki.Cite) bool { return c.Session == wrSeed })
}

func (a *wikiReviewAdapter) CiteDangling() error {
	if !a.gate.pass(a.screen == "page") {
		return nil
	}
	return a.cite(func(c wiki.Cite) bool { return c.Session == wrGhost })
}

func (a *wikiReviewAdapter) CloseSource() error {
	if !a.gate.pass(a.screen == "source" || a.screen == "source_error") {
		return nil
	}
	return a.openPage(wrPage)
}

// Edit opens the editor on the page's text; Save puts it back
// unchanged, which is PUT /api/wiki/page like any save.
func (a *wikiReviewAdapter) Edit() error {
	if !a.gate.pass(a.screen == "page") {
		return nil
	}
	var pg wiki.Page
	if err := a.api(http.MethodGet, "/api/wiki/page?path="+url.QueryEscape(wrPage), nil, &pg); err != nil {
		return err
	}
	a.body, a.screen = pg.Body, "editing"
	return nil
}

func (a *wikiReviewAdapter) Save() error {
	if !a.gate.pass(a.screen == "editing") {
		return nil
	}
	if err := a.api(http.MethodPut, "/api/wiki/page", map[string]string{"path": wrPage, "body": a.body}, nil); err != nil {
		return err
	}
	a.screen = "page"
	return nil
}

func (a *wikiReviewAdapter) ToIndex() error {
	on := slices.Contains([]string{"page", "missing", "source", "source_error", "review", "activity"}, a.screen)
	if !a.gate.pass(on && (!live(a.screen) || a.settled(a.view()))) {
		return nil
	}
	return a.goTo("index")
}

func (a *wikiReviewAdapter) ToReview() error {
	if !a.gate.pass(a.screen == "index") {
		return nil
	}
	return a.goTo("review")
}

func (a *wikiReviewAdapter) ToActivity() error {
	if !a.gate.pass(a.screen == "index") {
		return nil
	}
	return a.goTo("activity")
}

// Back keeps one level, as the spec does: under Review and Activity is
// the index, under anything else the adapter does not know.
func (a *wikiReviewAdapter) Back() error {
	inWiki := slices.Contains([]string{"index", "page", "missing", "source", "source_error", "editing", "review", "activity"}, a.screen)
	if !a.gate.pass(inWiki && a.back != "" && (!live(a.screen) || a.settled(a.view()))) {
		return nil
	}
	to := a.back
	a.back = ""
	if live(to) {
		a.back = "index"
	}
	switch to {
	case "page", "missing":
		back := a.back
		path := wrPage
		if to == "missing" {
			path = wrMissing
		}
		err := a.openPage(path)
		a.back = back
		return err
	case "source", "source_error":
		back := a.back
		err := a.cite(func(c wiki.Cite) bool { return (c.Session == wrSeed) == (to == "source") })
		a.back = back
		return err
	}
	return a.arrive(to)
}

// --- acting ---------------------------------------------------------

// decide is Review's button: POST /api/wiki/claim with the lines it
// showed. A 409 is the per-claim "Did not save"; success re-reads.
func (a *wikiReviewAdapter) decide(action string) error {
	if !a.gate.pass(a.screen == "review" && a.listed && !a.conflict) {
		return nil
	}
	f := a.shown
	err := a.api(http.MethodPost, "/api/wiki/claim", map[string]any{"path": f.Page, "line": f.Line, "end": f.End, "raw": f.Raw, "action": action}, nil)
	if status(err) == http.StatusConflict {
		a.conflict = true
		return nil
	}
	if err != nil {
		return err
	}
	return a.readReview()
}

func (a *wikiReviewAdapter) MarkInference() error { return a.decide("inference") }
func (a *wikiReviewAdapter) Drop() error          { return a.decide("drop") }

// IngestNow and IngestSession are the clicks. The POST they send is
// held back until Start: the spec lets a spawned run reach the lock at
// any later point, and the real one gets there within milliseconds, so
// sending it on the click would decide the run before a Session the
// spec interleaves could make anything pending.
func (a *wikiReviewAdapter) IngestNow() error {
	if !a.gate.pass(a.screen == "activity" && !a.spawned) {
		return nil
	}
	var act wiki.Activity
	if err := a.api(http.MethodGet, "/api/wiki/activity", nil, &act); err != nil {
		return err
	}
	a.spawned, a.only, a.note = true, "", "nothing"
	if act.Pending > 0 {
		a.note = "started"
	}
	return nil
}

func (a *wikiReviewAdapter) IngestSession() error {
	st := a.view()
	if !a.gate.pass(a.screen == "review" && len(st.pending) > 0 && !a.spawned) {
		return nil
	}
	a.spawned, a.only = true, st.pending[0].ID
	return nil
}

// --- the store and the ingest -----------------------------------------

// Session finishes a session: a transcript an hour quiet, so the next
// ingest finds it pending.
func (a *wikiReviewAdapter) Session() error {
	if !a.gate.pass(a.screen == "activity" && len(a.view().pending) == 0) {
		return nil
	}
	a.nsess++
	id := fmt.Sprintf("pend-%04d", a.nsess)
	if err := writeSession(filepath.Join(a.s.Home, ".bough", "history", id+".jsonl"), time.Now().Add(-time.Hour), "session "+id); err != nil {
		return err
	}
	a.pends = append(a.pends, id)
	return nil
}

// Start sends the Ingest POST and waits for the spawned `wiki run` to
// get past the lock: either its headless ingest takes the turn queued
// for it (a run), or it exits (lock held, or nothing pending).
func (a *wikiReviewAdapter) Start() error {
	if !a.gate.pass(a.spawned) {
		return nil
	}
	a.spawned = false
	st, err := a.store()
	if err != nil {
		return err
	}
	a.turn++
	name := fmt.Sprintf("ing%04d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "ingested"})
	before, err := a.wikiRuns()
	if err != nil {
		return err
	}
	var body any
	if a.only != "" {
		body = map[string]string{"session": a.only}
	}
	if err := a.api(http.MethodPost, "/api/wiki/ingest", body, nil); err != nil {
		return err
	}
	// spawnWiki's Start returns after exec, so the child is there now.
	after, err := a.wikiRuns()
	if err != nil {
		return err
	}
	pid := 0
	for _, p := range after {
		if !slices.Contains(before, p) {
			pid = p
		}
	}
	ids := []string{}
	for _, p := range st.pending {
		if a.only == "" || p.ID == a.only {
			ids = append(ids, p.ID)
		}
	}
	taken := filepath.Join(a.dir, name+".taken")
	return poll("the spawned wiki run to reach the lock", func() (bool, error) {
		if _, err := os.Stat(taken); err == nil {
			a.held = append(a.held, wrRun{turn: name, pid: pid, ids: ids})
			return true, a.waitIngesting(len(a.held))
		}
		if pid != 0 && alive(pid) {
			return false, nil
		}
		// It exited without asking the model: take the turn back,
		// unless it was taken between the two checks.
		if err := os.Remove(filepath.Join(a.dir, name+".json")); errors.Is(err, os.ErrNotExist) {
			return false, nil
		}
		return true, nil
	})
}

// Land is the ingest agent's work while its turn is held: rewrite the
// flagged claim's lines, record the sessions in log.md, then let the
// turn finish so the run commits and exits.
func (a *wikiReviewAdapter) Land() error {
	if !a.gate.pass(live(a.screen) && len(a.held) > 0) {
		return nil
	}
	r := a.held[0]
	a.version++
	if err := os.WriteFile(filepath.Join(a.wiki, filepath.FromSlash(wrPage)), []byte(wrPageBody(a.version)), 0o644); err != nil {
		return err
	}
	if a.bug != "land-skips-log" {
		f, err := os.OpenFile(filepath.Join(a.wiki, "log.md"), os.O_APPEND|os.O_WRONLY, 0o644)
		if err != nil {
			return err
		}
		for _, id := range r.ids {
			fmt.Fprintf(f, "\n## [%s] ingest | %s#3 | ingested | %s\n", time.Now().Format("2006-01-02"), id, wrPage)
		}
		f.Close()
	}
	return a.end(true)
}

// Stop is runTimeout: the run ends without writing anything. The
// adapter fails the held turn rather than waiting half an hour.
func (a *wikiReviewAdapter) Stop() error {
	if !a.gate.pass(live(a.screen) && len(a.held) > 0) {
		return nil
	}
	return a.end(false)
}

// end lets the oldest held run finish (ok) or fail, and waits for its
// `wiki run` to exit and Activity to stop counting it.
func (a *wikiReviewAdapter) end(ok bool) error {
	r := a.held[0]
	a.held = a.held[1:]
	if ok {
		control.Release(a.t, a.dir, r.turn)
	} else {
		control.ReleaseWith(a.t, a.dir, r.turn, control.Turn{Mode: "error", Error: "ingest timed out"})
	}
	if err := poll("the wiki run to exit", func() (bool, error) { return r.pid == 0 || !alive(r.pid), nil }); err != nil {
		return err
	}
	return a.waitIngesting(len(a.held))
}

// Poll is the nav's 60 s re-read of GET /api/wiki.
func (a *wikiReviewAdapter) Poll() error {
	st := a.view()
	if !a.gate.pass(live(a.screen) && a.badge != st.flagged) {
		return nil
	}
	var ix wiki.Index
	if err := a.api(http.MethodGet, "/api/wiki", nil, &ix); err != nil {
		return err
	}
	a.badge = ix.Health.Unsupported+ix.Health.Superseded+ix.Health.Uncited > 0
	return nil
}

// waitIngesting waits for Activity to count n running ingests: the
// headless session writes its input a moment after taking the turn.
func (a *wikiReviewAdapter) waitIngesting(n int) error {
	last := -1
	err := poll(fmt.Sprintf("Activity to show %d running", n), func() (bool, error) {
		st, err := a.store()
		last = st.ingesting
		return err == nil && st.ingesting == n, nil
	})
	if err != nil {
		return fmt.Errorf("%w (last %d)", err, last)
	}
	return nil
}

// wikiRuns lists serve's `bough wiki run` children.
func (a *wikiReviewAdapter) wikiRuns() ([]int, error) {
	out, err := exec.Command("ps", "-A", "-o", "pid=,ppid=,command=").Output()
	if err != nil {
		return nil, err
	}
	var pids []int
	for _, line := range strings.Split(string(out), "\n") {
		f := strings.Fields(line)
		if len(f) < 4 || f[1] != strconv.Itoa(a.pid) || !strings.Contains(line, " wiki run") {
			continue
		}
		if p, err := strconv.Atoi(f[0]); err == nil {
			pids = append(pids, p)
		}
	}
	return pids, nil
}

// alive: a reaped or zombie `wiki run` no longer counts.
func alive(pid int) bool {
	out, err := exec.Command("ps", "-o", "stat=,command=", "-p", strconv.Itoa(pid)).Output()
	return err == nil && !strings.HasPrefix(strings.TrimSpace(string(out)), "Z") && strings.Contains(string(out), " wiki run")
}

func poll(what string, done func() (bool, error)) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		ok, err := done()
		if err != nil || ok {
			return err
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for %s: timed out after %s", what, actionTimeout)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// api is one request to the serve's HTTP API; a non-2xx answer is a
// *servetest.APIError.
func (a *wikiReviewAdapter) api(method, path string, body, out any) error {
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
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return err
	}
	if resp.StatusCode/100 != 2 {
		return &servetest.APIError{Status: resp.StatusCode, Msg: strings.TrimSpace(string(raw))}
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(raw, out)
}

func status(err error) int {
	var e *servetest.APIError
	if errors.As(err, &e) {
		return e.Status
	}
	return 0
}

var wikiReviewActions = map[string]map[string]fmbt.ActionFunc{"Wiki": {
	"Nav":           action((*wikiReviewAdapter).Nav),
	"OpenPage":      action((*wikiReviewAdapter).OpenPage),
	"OpenMissing":   action((*wikiReviewAdapter).OpenMissing),
	"Cite":          action((*wikiReviewAdapter).Cite),
	"CiteDangling":  action((*wikiReviewAdapter).CiteDangling),
	"CloseSource":   action((*wikiReviewAdapter).CloseSource),
	"Edit":          action((*wikiReviewAdapter).Edit),
	"Save":          action((*wikiReviewAdapter).Save),
	"ToIndex":       action((*wikiReviewAdapter).ToIndex),
	"ToReview":      action((*wikiReviewAdapter).ToReview),
	"ToActivity":    action((*wikiReviewAdapter).ToActivity),
	"Back":          action((*wikiReviewAdapter).Back),
	"MarkInference": action((*wikiReviewAdapter).MarkInference),
	"Drop":          action((*wikiReviewAdapter).Drop),
	"IngestNow":     action((*wikiReviewAdapter).IngestNow),
	"IngestSession": action((*wikiReviewAdapter).IngestSession),
	"Session":       action((*wikiReviewAdapter).Session),
	"Start":         action((*wikiReviewAdapter).Start),
	"Land":          action((*wikiReviewAdapter).Land),
	"Stop":          action((*wikiReviewAdapter).Stop),
	"Poll":          action((*wikiReviewAdapter).Poll),
}}

func wikiReviewOptions() map[string]any {
	return map[string]any{"max-seq-runs": 150, "max-actions": 8, "max-parallel-runs": 0}
}

// wikiReviewHistory reads an ingest run's transcript as the steps that
// must have led to it: Activity's Ingest on a pending session, the run
// reaching the lock (its input), and its close, Stop when an error came
// inside the turn and Land otherwise. The navigation and the Session are
// implied, not recorded; what the check holds the run to is one input
// and at most one close, in that order.
func wikiReviewHistory(entries []history.Entry) []tracecheck.Step {
	w := func(s string) string { return "Wiki#0." + s }
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Wiki#0.ingesting": 0}}}
	failed := false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			failed = false
			steps = append(steps,
				tracecheck.Step{Action: w("Nav")},
				tracecheck.Step{Action: w("ToActivity")},
				tracecheck.Step{Action: w("Session"), State: map[string]any{"Wiki#0.pending": true}},
				tracecheck.Step{Action: w("IngestNow"), State: map[string]any{"Wiki#0.spawned": true}},
				tracecheck.Step{Action: w("Start"), State: map[string]any{"Wiki#0.ingesting": 1}})
		case "error":
			failed = true
		case "done":
			if failed {
				steps = append(steps, tracecheck.Step{Action: w("Stop"), State: map[string]any{"Wiki#0.ingesting": 0, "Wiki#0.pending": true}})
			} else {
				steps = append(steps, tracecheck.Step{Action: w("Land"), State: map[string]any{"Wiki#0.ingesting": 0, "Wiki#0.pending": false, "Wiki#0.flagged": true}})
			}
		}
	}
	return steps
}

func init() { historyProjections["wiki_review"] = wikiReviewHistory }

// ingestSessions lists the headless ingest sessions under the serve's
// HOME: those started in the wiki directory.
func (a *wikiReviewAdapter) ingestSessions(t *testing.T) []string {
	infos, err := history.List(filepath.Join(a.s.Home, ".bough", "history"))
	if err != nil {
		t.Fatal(err)
	}
	var ids []string
	for _, in := range infos {
		if in.Cwd != "" && sameDir(in.Cwd, a.wiki) {
			ids = append(ids, in.ID)
		}
	}
	return ids
}

func sameDir(x, y string) bool {
	rx, _ := filepath.EvalSymlinks(x)
	ry, _ := filepath.EvalSymlinks(y)
	return rx != "" && rx == ry
}

// TestWikiReview is the fizzbee-mbt run. Its runner picks each step
// uniformly from all 21 actions, enabled or not, and a walk ends at the
// first disabled one; from the start only Nav is enabled, so few walks
// get past the index. TestWikiReviewPaths is what reaches the ingests.
func TestWikiReview(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newWikiReviewAdapter(t)
	if err := runMBT(t, "wiki_review", a, wikiReviewActions, wikiReviewOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// The run above proves nothing unless an adapter that breaks the model
// fails it: here Nav forgets the screen it left, on the one step most
// walks take.
func TestWikiReviewCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newWikiReviewAdapter(t)
	a.bug = "nav-forgets-back"
	if err := runMBT(t, "wiki_review", a, wikiReviewActions, wikiReviewOptions()); err == nil {
		t.Fatal("a run whose Nav loses Back passed; the runner is not checking state")
	}
}

// wrTrace is one path of testdata/wiki_review/paths.json.
type wrTrace []struct {
	Action string         `json:"action"`
	State  map[string]any `json:"state"`
}

func wrTestdata() string {
	return filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "wiki_review")
}

func wrPaths(t *testing.T) []wrTrace {
	b, err := os.ReadFile(filepath.Join(wrTestdata(), "paths.json"))
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Paths []struct {
			Trace wrTrace `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		t.Fatal(err)
	}
	out := make([]wrTrace, len(f.Paths))
	for i, p := range f.Paths {
		out[i] = p.Trace
	}
	return out
}

// replay walks one generated path against the serve: every action must
// pass its gate, and after every step the state read must be the path's.
func (a *wikiReviewAdapter) replay(tr wrTrace) (err error) {
	if err := a.Init(); err != nil {
		return fmt.Errorf("Init: %w", err)
	}
	defer func() {
		if cerr := a.Cleanup(); err == nil && cerr != nil {
			err = fmt.Errorf("Cleanup: %w", cerr)
		}
	}()
	for i, step := range tr {
		name := strings.TrimPrefix(step.Action, "Wiki#0.")
		if i > 0 {
			f := wikiReviewActions["Wiki"][name]
			if f == nil {
				return fmt.Errorf("step %d: no action %q", i, name)
			}
			if _, err := f(a, nil); err != nil {
				return fmt.Errorf("step %d (%s): %w", i, name, err)
			}
			if a.gate.off {
				return fmt.Errorf("step %d (%s): the adapter reads it as disabled", i, name)
			}
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w", i, name, err)
		}
		raw, _ := json.Marshal(got)
		var have map[string]any
		if err := json.Unmarshal(raw, &have); err != nil {
			return err
		}
		var diff []string
		for k, want := range step.State {
			field, ok := strings.CutPrefix(k, "Wiki#0.")
			if ok && !reflect.DeepEqual(have[field], want) {
				diff = append(diff, fmt.Sprintf("%s: want %v, got %v", field, want, have[field]))
			}
		}
		if len(diff) > 0 {
			slices.Sort(diff)
			return fmt.Errorf("step %d (%s): %s", i, name, strings.Join(diff, "; "))
		}
	}
	return nil
}

// replayAll walks the generated paths in order on one serve and returns
// a line for each that failed.
func (a *wikiReviewAdapter) replayAll(t *testing.T, stopAtFirst bool) []string {
	var fails []string
	for i, tr := range wrPaths(t) {
		if err := a.replay(tr); err != nil {
			var names []string
			for _, s := range tr[1:] {
				names = append(names, strings.TrimPrefix(s.Action, "Wiki#0."))
			}
			fails = append(fails, fmt.Sprintf("path %d [%s]: %v", i, strings.Join(names, " "), err))
			if stopAtFirst {
				break
			}
		}
	}
	return fails
}

// TestWikiReviewPaths walks every path the generator wrote for the spec
// (the ones the browser test walks, covering every transition) against
// a real serve, then trace-checks the transcript of every ingest those
// walks ran. paths.json and the graph are the checked-in ones
// (TestSpecFixtures keeps them the spec's), so it needs no fizz tool,
// but it runs ~90 real ingests in two to three minutes: it runs where
// the rest of this package does, with the tools exported.
func TestWikiReviewPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newWikiReviewAdapter(t)
	for _, f := range a.replayAll(t, false) {
		t.Error(f)
	}
	g, err := tracecheck.Load(wrTestdata())
	if err != nil {
		t.Fatal(err)
	}
	ids := a.ingestSessions(t)
	if len(ids) == 0 {
		t.Fatal("the walks ran no ingest")
	}
	t.Logf("wiki_review: trace-checking %d ingest runs", len(ids))
	for _, id := range ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), wikiReviewHistory)
	}
}

// The path walk must fail on an adapter that breaks the model: here the
// ingest lands its page but never logs the session it ingested.
func TestWikiReviewPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newWikiReviewAdapter(t)
	a.bug = "land-skips-log"
	fails := a.replayAll(t, true)
	if len(fails) == 0 {
		t.Fatal("every path passed with an ingest that never logs its sessions")
	}
	t.Log(fails[0])
}
