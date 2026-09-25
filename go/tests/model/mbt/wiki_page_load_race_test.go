//go:build !windows

package mbt

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/wiki"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/wiki_page_load_race.fizz against a real serve: two wiki pages,
// the reads the page view starts as its route moves between them, and
// the editor and Save that act on whatever page is on screen.
//
// The adapter plays useLoad, WikiPage and WikiPageView (web/src/wiki.tsx)
// one model step at a time, as they are: every read lands whatever the
// route is by then, the view's head is the shown page's path, and Save
// PUTs the editor's text to the route it was clicked on. What the server
// does is real. A read is the page's GET, made at the step the spec
// answers it (the page may be sent earlier; the model only orders the
// answers). Its answer is what shown and the editor are made of: shown
// is the path the answer names, the editor the answer's body. A failed
// read is a page the server cannot read (mode 000: a 404), a failed save
// one it cannot write (mode 444: a 500). A save is PUT /api/wiki/page,
// read back to be the text sent, and committed in the wiki's git log,
// which the trace check replays.
//
// The spec's two ghosts are the page's: wrongwrite is a PUT whose text
// the editor took from one page landing on the other (read back off the
// server), late a read that changed the screen after its route had
// moved on.

var wplrPath = map[string]string{"A": "topics/race/a.md", "B": "topics/race/b.md"}

// wplrBody is each page's text as Init leaves it. The heading says whose
// text it is, so a commit shows which page an editor was seeded from.
func wplrBody(p string) string {
	return "# Page " + p + "\n\nThe " + p + " page of the race.\n"
}

func wplrName(path string) string {
	for n, p := range wplrPath {
		if p == path {
			return n
		}
	}
	return "?" + path
}

type wplrPut struct {
	from, to, body string
}

type wikiPageLoadRaceAdapter struct {
	t    *testing.T
	s    *servetest.Server
	wiki string
	gate gate

	// the page
	route string     // away | A | B
	data  *wiki.Page // useLoad's data: the last read that landed
	err   string     // the page whose failed read is on screen
	reads map[string]bool
	seed  *wiki.Page // the page the editor was opened on
	put   *wplrPut
	saves int
	oks   []int // per walk, the saves the server took: its commits

	wrong, late bool

	// keyed is TestWikiPageLoadRacePathsCatchWrongAdapter's bug: reads
	// are keyed, so one whose route moved on lands nowhere.
	keyed bool
}

func newWikiPageLoadRaceAdapter(t *testing.T) *wikiPageLoadRaceAdapter {
	// The serve commits every save; the host's git config (signing,
	// hooks) must not decide whether that commit happens.
	s := servetest.Start(t, servetest.Options{Config: controlConfig, Env: wplrGitEnv()})
	a := &wikiPageLoadRaceAdapter{t: t, s: s, wiki: filepath.Join(s.Home, ".bough", "wiki")}
	if err := os.MkdirAll(filepath.Join(a.wiki, "topics", "race"), 0o755); err != nil {
		t.Fatal(err)
	}
	index := "# Wiki index\n\n## race\n\n- [Page A](topics/race/a.md) — first\n- [Page B](topics/race/b.md) — second\n"
	if err := os.WriteFile(filepath.Join(a.wiki, "index.md"), []byte(index), 0o644); err != nil {
		t.Fatal(err)
	}
	if out, err := a.git("init", "-q"); err != nil {
		t.Fatalf("git init: %v\n%s", err, out)
	}
	return a
}

func wplrGitEnv() []string {
	return []string{"GIT_CONFIG_GLOBAL=" + os.DevNull, "GIT_CONFIG_NOSYSTEM=1"}
}

func (a *wikiPageLoadRaceAdapter) git(args ...string) (string, error) {
	cmd := exec.Command("git", append([]string{"-C", a.wiki, "-c", "user.name=test", "-c", "user.email=test@localhost", "-c", "commit.gpgsign=false"}, args...)...)
	cmd.Env = append(os.Environ(), wplrGitEnv()...)
	out, err := cmd.CombinedOutput()
	return string(out), err
}

func (a *wikiPageLoadRaceAdapter) file(p string) string {
	return filepath.Join(a.wiki, filepath.FromSlash(wplrPath[p]))
}

// Init writes both pages back and commits that as "reset", which is
// where the trace check splits the log into walks.
func (a *wikiPageLoadRaceAdapter) Init() error {
	a.gate.reset()
	a.route, a.data, a.err, a.reads, a.seed, a.put = "away", nil, "", map[string]bool{}, nil, nil
	a.wrong, a.late = false, false
	a.oks = append(a.oks, 0)
	for _, p := range []string{"A", "B"} {
		if err := os.Chmod(a.file(p), 0o644); err != nil && !os.IsNotExist(err) {
			return err
		}
		if err := os.WriteFile(a.file(p), []byte(wplrBody(p)), 0o644); err != nil {
			return err
		}
	}
	if out, err := a.git("add", "-A"); err != nil {
		return fmt.Errorf("git add: %v\n%s", err, out)
	}
	if out, err := a.git("commit", "-q", "--allow-empty", "-m", "reset"); err != nil {
		return fmt.Errorf("git commit: %v\n%s", err, out)
	}
	return nil
}

func (a *wikiPageLoadRaceAdapter) Cleanup() error { return nil }

func (a *wikiPageLoadRaceAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Wiki", Index: 0}: a}, nil
}

func (a *wikiPageLoadRaceAdapter) shown() string {
	if a.data == nil {
		return ""
	}
	return wplrName(a.data.Path)
}

func (a *wikiPageLoadRaceAdapter) editing() string {
	if a.seed == nil {
		return ""
	}
	return wplrName(a.seed.Path)
}

// head is WikiPageView's `at`: the shown page's path, else the route's.
func (a *wikiPageLoadRaceAdapter) head() string {
	if s := a.shown(); s != "" {
		return s
	}
	return a.route
}

func (a *wikiPageLoadRaceAdapter) GetState() (map[string]any, error) {
	put := ""
	if a.put != nil {
		put = a.put.from + ">" + a.put.to
	}
	return map[string]any{
		"route": a.route, "shown": a.shown(), "err": a.err,
		"readA": a.reads["A"], "readB": a.reads["B"],
		"editing": a.editing(), "put": put,
		"wrongwrite": a.wrong, "late": a.late,
	}, nil
}

func (a *wikiPageLoadRaceAdapter) api(method, path string, body, out any) error {
	return (&wikiReviewAdapter{s: a.s}).api(method, path, body, out)
}

func (a *wikiPageLoadRaceAdapter) getPage(p string) (wiki.Page, error) {
	var pg wiki.Page
	err := a.api(http.MethodGet, "/api/wiki/page?path="+url.QueryEscape(wplrPath[p]), nil, &pg)
	return pg, err
}

// open is a new page route: useLoad's key changes, data and error clear,
// a read of the new path starts. The view stays mounted from page to
// page, so the editor survives unless the head moves.
func (a *wikiPageLoadRaceAdapter) open(p string) error {
	before := ""
	if a.route != "away" {
		before = a.head()
	}
	a.route, a.data, a.err = p, nil, ""
	a.reads[p] = true
	if a.head() != before {
		a.seed = nil
	}
	return nil
}

func (a *wikiPageLoadRaceAdapter) OpenA() error {
	if !a.gate.pass(a.route != "A") {
		return nil
	}
	return a.open("A")
}

func (a *wikiPageLoadRaceAdapter) OpenB() error {
	if !a.gate.pass(a.route != "B") {
		return nil
	}
	return a.open("B")
}

func (a *wikiPageLoadRaceAdapter) Away() error {
	if !a.gate.pass(a.route != "away") {
		return nil
	}
	a.route, a.data, a.err, a.seed = "away", nil, "", nil
	return nil
}

func (a *wikiPageLoadRaceAdapter) Retry() error {
	if !a.gate.pass(a.route != "away" && a.data == nil && a.err != "") {
		return nil
	}
	a.err = ""
	a.reads[a.route] = true
	return nil
}

func (a *wikiPageLoadRaceAdapter) Edit() error {
	if !a.gate.pass(a.route != "away" && a.data != nil && a.seed == nil) {
		return nil
	}
	a.seed = a.data
	return nil
}

func (a *wikiPageLoadRaceAdapter) Cancel() error {
	if !a.gate.pass(a.seed != nil && a.data != nil) {
		return nil
	}
	a.seed = nil
	return nil
}

// Save sends the editor's text, the seed's body with a line typed under
// it (so every save is a commit), to the route's path.
func (a *wikiPageLoadRaceAdapter) Save() error {
	if !a.gate.pass(a.seed != nil && a.data != nil && a.put == nil) {
		return nil
	}
	a.saves++
	a.put = &wplrPut{from: a.editing(), to: a.route, body: a.seed.Body + fmt.Sprintf("\nedit %d\n", a.saves)}
	return nil
}

// land is setData/setErr and then the view's reset effect: the editor
// closes when the head moves.
func (a *wikiPageLoadRaceAdapter) land(p string, pg *wiki.Page) {
	a.reads[p] = false
	if a.route == "away" || (a.keyed && p != a.route) {
		return
	}
	before := a.head()
	nothing := a.data == nil
	if pg != nil {
		a.data, a.err = pg, ""
	} else if a.data == nil {
		a.err = p
	}
	a.late = a.late || (p != a.route && (pg != nil || nothing))
	if a.head() != before {
		a.seed = nil
	}
}

func (a *wikiPageLoadRaceAdapter) answer(p string) error {
	if !a.gate.pass(a.reads[p]) {
		return nil
	}
	pg, err := a.getPage(p)
	if err != nil {
		return fmt.Errorf("reading page %s: %w", p, err)
	}
	if pg.Path != wplrPath[p] {
		return fmt.Errorf("the read of %s answered with %s", wplrPath[p], pg.Path)
	}
	a.land(p, &pg)
	return nil
}

func (a *wikiPageLoadRaceAdapter) fail(p string) error {
	if !a.gate.pass(a.reads[p]) {
		return nil
	}
	if err := os.Chmod(a.file(p), 0); err != nil {
		return err
	}
	_, err := a.getPage(p)
	if cerr := os.Chmod(a.file(p), 0o644); cerr != nil {
		return cerr
	}
	if status(err) < 400 {
		return fmt.Errorf("the read of an unreadable %s answered %d (%v)", wplrPath[p], status(err), err)
	}
	a.land(p, nil)
	return nil
}

func (a *wikiPageLoadRaceAdapter) AnswerA() error { return a.answer("A") }
func (a *wikiPageLoadRaceAdapter) AnswerB() error { return a.answer("B") }
func (a *wikiPageLoadRaceAdapter) FailA() error   { return a.fail("A") }
func (a *wikiPageLoadRaceAdapter) FailB() error   { return a.fail("B") }

// SaveOk: the PUT lands, the server holds exactly its text, the editor
// closes and the saved path is read again.
func (a *wikiPageLoadRaceAdapter) SaveOk() error {
	if !a.gate.pass(a.put != nil) {
		return nil
	}
	pt := a.put
	if err := a.api(http.MethodPut, "/api/wiki/page", map[string]string{"path": wplrPath[pt.to], "body": pt.body}, nil); err != nil {
		return fmt.Errorf("saving %s: %w", wplrPath[pt.to], err)
	}
	pg, err := a.getPage(pt.to)
	if err != nil {
		return err
	}
	if pg.Body != pt.body {
		return fmt.Errorf("%s holds %q after a save of %q", wplrPath[pt.to], pg.Body, pt.body)
	}
	a.wrong = a.wrong || !strings.HasPrefix(pg.Body, a.headingOf(pt.to))
	a.put, a.seed = nil, nil
	a.reads[pt.to] = true
	a.oks[len(a.oks)-1]++
	return nil
}

// headingOf is the heading the text at page p had when the walk began;
// a save that lands another page's text there changes it.
func (a *wikiPageLoadRaceAdapter) headingOf(p string) string {
	return "# Page " + p + "\n"
}

// SaveFail: the server cannot write the page; the editor stays open.
func (a *wikiPageLoadRaceAdapter) SaveFail() error {
	if !a.gate.pass(a.put != nil) {
		return nil
	}
	pt := a.put
	if err := os.Chmod(a.file(pt.to), 0o444); err != nil {
		return err
	}
	err := a.api(http.MethodPut, "/api/wiki/page", map[string]string{"path": wplrPath[pt.to], "body": pt.body}, nil)
	if cerr := os.Chmod(a.file(pt.to), 0o644); cerr != nil {
		return cerr
	}
	if status(err) < 400 {
		return fmt.Errorf("a save to a read-only %s answered %d (%v)", wplrPath[pt.to], status(err), err)
	}
	a.put = nil
	return nil
}

var wikiPageLoadRaceActions = map[string]map[string]fmbt.ActionFunc{"Wiki": {
	"OpenA":    action((*wikiPageLoadRaceAdapter).OpenA),
	"OpenB":    action((*wikiPageLoadRaceAdapter).OpenB),
	"Away":     action((*wikiPageLoadRaceAdapter).Away),
	"Retry":    action((*wikiPageLoadRaceAdapter).Retry),
	"Edit":     action((*wikiPageLoadRaceAdapter).Edit),
	"Cancel":   action((*wikiPageLoadRaceAdapter).Cancel),
	"Save":     action((*wikiPageLoadRaceAdapter).Save),
	"AnswerA":  action((*wikiPageLoadRaceAdapter).AnswerA),
	"AnswerB":  action((*wikiPageLoadRaceAdapter).AnswerB),
	"FailA":    action((*wikiPageLoadRaceAdapter).FailA),
	"FailB":    action((*wikiPageLoadRaceAdapter).FailB),
	"SaveOk":   action((*wikiPageLoadRaceAdapter).SaveOk),
	"SaveFail": action((*wikiPageLoadRaceAdapter).SaveFail),
}}

// wplrCommit is one save the wiki's git log recorded.
type wplrCommit struct {
	Page, Body string // the page saved ("A"/"B") and the text it got
}

// wikiPageLoadRaceHistory reads one walk's commits (those after a
// "reset") as the steps that must have led to them. The heading of the
// text a commit wrote says whose text it began as, and so which page the
// editor can have been seeded from: one that has held that text in this
// walk (an editor may be seeded from a read older than the page's last
// save). A save of text the page itself has held is Open, Answer, Edit,
// Save, SaveOk; text only the other page has held needs that page's read
// to land late on this route, and is the wrong write; text neither held
// is no step of the model. Everything else a walk did (failures,
// retries, cancels, failed saves) left no commit.
func wikiPageLoadRaceHistory(commits []wplrCommit) []tracecheck.Step {
	w := func(s string, st map[string]any) tracecheck.Step {
		return tracecheck.Step{Action: "Wiki#0." + s, State: st}
	}
	held := map[string][]string{"A": {"A"}, "B": {"B"}} // whose texts each page has held
	wrong := false
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Wiki#0.route": "away", "Wiki#0.wrongwrite": false}}}
	for _, c := range commits {
		text := ""
		for _, p := range []string{"A", "B"} {
			if strings.HasPrefix(c.Body, "# Page "+p+"\n") {
				text = p
			}
		}
		from := ""
		for _, p := range []string{c.Page, other(c.Page)} {
			if from == "" && text != "" && slices.Contains(held[p], text) {
				from = p
			}
		}
		if from == "" {
			// No page held this text: nothing in the model writes it.
			steps = append(steps, w("SaveOf"+c.Page+"FromNowhere", nil))
			return steps
		}
		if from == c.Page {
			steps = append(steps, w("Open"+c.Page, nil), w("Answer"+c.Page, map[string]any{"Wiki#0.shown": c.Page}))
		} else {
			wrong = true
			steps = append(steps, w("Open"+from, nil), w("Open"+c.Page, nil),
				w("Answer"+from, map[string]any{"Wiki#0.shown": from, "Wiki#0.route": c.Page, "Wiki#0.late": true}))
		}
		steps = append(steps, w("Edit", map[string]any{"Wiki#0.editing": from}),
			w("Save", map[string]any{"Wiki#0.put": from + ">" + c.Page}),
			w("SaveOk", map[string]any{"Wiki#0.wrongwrite": wrong}),
			w("Answer"+c.Page, map[string]any{"Wiki#0.shown": c.Page}),
			w("Away", map[string]any{"Wiki#0.route": "away"}))
		held[c.Page] = append(held[c.Page], text)
	}
	return steps
}

func other(p string) string {
	if p == "A" {
		return "B"
	}
	return "A"
}

// walkCommits reads the wiki's git log into one list of saves per walk.
func (a *wikiPageLoadRaceAdapter) walkCommits() ([][]wplrCommit, error) {
	out, err := a.git("log", "--reverse", "--format=%H%x09%s")
	if err != nil {
		return nil, fmt.Errorf("git log: %v\n%s", err, out)
	}
	var walks [][]wplrCommit
	for _, line := range strings.Split(strings.TrimSpace(out), "\n") {
		hash, subject, _ := strings.Cut(line, "\t")
		if subject == "reset" {
			walks = append(walks, nil)
			continue
		}
		path, ok := strings.CutPrefix(subject, "edit ")
		if !ok || len(walks) == 0 {
			return nil, fmt.Errorf("unexpected commit %q", line)
		}
		body, err := a.git("show", hash+":"+path)
		if err != nil {
			return nil, fmt.Errorf("git show %s:%s: %v\n%s", hash, path, err, body)
		}
		walks[len(walks)-1] = append(walks[len(walks)-1], wplrCommit{Page: wplrName(path), Body: body})
	}
	return walks, nil
}

// checkWikiPageLoadRaceHistory replays every walk's saves on the graph.
// It returns how many saves it checked.
func checkWikiPageLoadRaceHistory(t *testing.T, a *wikiPageLoadRaceAdapter) int {
	t.Helper()
	g, err := tracecheck.Load(wplrTestdata())
	if err != nil {
		t.Fatal(err)
	}
	walks, err := a.walkCommits()
	if err != nil {
		t.Fatal(err)
	}
	if len(walks) != len(a.oks) {
		t.Errorf("%d walks, %d resets in the wiki's log", len(a.oks), len(walks))
	}
	n := 0
	for i, cs := range walks {
		n += len(cs)
		if i < len(a.oks) && len(cs) != a.oks[i] {
			t.Errorf("walk %d: the server took %d saves and the wiki's log has %d", i, a.oks[i], len(cs))
		}
		steps := wikiPageLoadRaceHistory(cs)
		if v := g.Check(steps); v != nil {
			b, _ := json.Marshal(steps)
			t.Errorf("walk %d: the wiki's history is not a path in the model: %v\ntrace: %s", i, v, b)
		}
	}
	return n
}

func wplrTestdata() string {
	return filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "wiki_page_load_race")
}

// wplrWalks is the generated walks over the checked-in graph.
func wplrWalks(t *testing.T, cover tracecheck.Cover) [][]tracecheck.Step {
	b, err := pathsJSONCover("wiki_page_load_race", cover)
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		t.Fatal(err)
	}
	out := make([][]tracecheck.Step, len(f.Paths))
	for i, p := range f.Paths {
		out[i] = p.Trace
	}
	return out
}

// replay walks one generated path: every action must pass its gate, and
// after every step the state read must be the path's.
func (a *wikiPageLoadRaceAdapter) replay(tr []tracecheck.Step) error {
	var names []string
	for _, s := range tr {
		names = append(names, strings.TrimPrefix(s.Action, "Wiki#0."))
	}
	for i, step := range tr {
		name := names[i]
		if i == 0 {
			if err := a.Init(); err != nil {
				return fmt.Errorf("Init: %w", err)
			}
		} else {
			f := wikiPageLoadRaceActions["Wiki"][name]
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
		got, _ := a.GetState()
		var diff []string
		for k, want := range step.State {
			field, ok := strings.CutPrefix(k, "Wiki#0.")
			if ok && !reflect.DeepEqual(got[field], want) {
				diff = append(diff, fmt.Sprintf("%s: want %v, got %v", field, want, got[field]))
			}
		}
		if len(diff) > 0 {
			slices.Sort(diff)
			return fmt.Errorf("step %d (%s) of %v: %s", i, name, names[1:i+1], strings.Join(diff, "; "))
		}
	}
	return nil
}

// TestWikiPageLoadRacePaths walks the generated paths (every settled
// state; every link with MODEL_COVER=transitions) against two serves,
// then replays each serve's wiki history on the graph.
func TestWikiPageLoadRacePaths(t *testing.T) {
	t.Parallel()
	paths := wplrWalks(t, envCover())
	workers := min(2, len(paths))
	for w := range workers {
		t.Run(fmt.Sprintf("serve%d", w), func(t *testing.T) {
			t.Parallel()
			a := newWikiPageLoadRaceAdapter(t)
			steps := 0
			for j := w; j < len(paths); j += workers {
				steps += len(paths[j]) - 1
				if err := a.replay(paths[j]); err != nil {
					t.Errorf("path %d: %v", j, err)
				}
			}
			n := checkWikiPageLoadRaceHistory(t, a)
			t.Logf("%d steps; %d saves trace-checked", steps, n)
			if n == 0 {
				t.Error("the walks committed no save")
			}
		})
	}
}

// An adapter whose reads are keyed to their route (what wiki.tsx does
// not do) differs from the spec only when a read lands after its route
// moved on, so this walks every link.
func TestWikiPageLoadRacePathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newWikiPageLoadRaceAdapter(t)
	a.keyed = true
	for i, p := range wplrWalks(t, tracecheck.CoverTransitions) {
		if err := a.replay(p); err != nil {
			t.Logf("caught on path %d: %v", i, err)
			return
		}
	}
	t.Fatal("every path passed with keyed reads; the walk is not checking state")
}

// The history projection must refuse a log the model cannot produce: a
// save of text neither page ever held.
func TestWikiPageLoadRaceHistoryRejects(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(wplrTestdata())
	if err != nil {
		t.Fatal(err)
	}
	good := []wplrCommit{{Page: "B", Body: wplrBody("A") + "\nedit 1\n"}, {Page: "B", Body: wplrBody("A") + "\nedit 1\n\nedit 2\n"}}
	if v := g.Check(wikiPageLoadRaceHistory(good)); v != nil {
		t.Fatalf("a wrong write and a save over it: %v", v)
	}
	bad := []wplrCommit{{Page: "B", Body: wplrBody("A") + "\nedit 1\n"}, {Page: "A", Body: wplrBody("C") + "\nedit 2\n"}}
	if v := g.Check(wikiPageLoadRaceHistory(bad)); v == nil {
		t.Fatal("a save of text no page held passed the trace check")
	} else {
		t.Logf("refused: %v", v)
	}
}

func TestWikiPageLoadRace(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newWikiPageLoadRaceAdapter(t)
	opts := map[string]any{"max-seq-runs": 300, "max-actions": 10, "max-parallel-runs": 0}
	if err := runMBT(t, "wiki_page_load_race", a, wikiPageLoadRaceActions, opts); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}
