//go:build !windows

package mbt

import (
	"bytes"
	"encoding/json"
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

// specs/route_resolution_first_load.fizz at the server level: one tab's
// first load, deep links, Back and Reload against a real serve.
//
// The hash, Back, the palette, arrived and acted are the page's own:
// the adapter keeps them as the spec says the page must. What a page
// renders from a server read, it reads off the server, every time the
// page would:
//
//   - list: GET /api/sessions. It must hold S as the one arrival
//     candidate (arrivalPick, the page's own rule) and none of the X
//     ids; a failed first read is the list read failing (500) while the
//     history directory cannot be read.
//   - page, wherever a read renders it: S's transcript; its Changes page
//     (the changes read, and a transcript that holds the turn ?turn=N
//     names); the X lookups, one id per outcome (an archived session
//     200, an id serve never had 404, an unreadable transcript 500); S's
//     own lookup failing the same way; the odd wiki page's read, by the
//     path wikiHash was handed; the project list.
//   - detail and shown: GET /api/projects/{slug}, whose answer must be
//     the slug asked for, and b's thread list must hold T and not Z; a
//     missing b is its definition directory moved away (404), a failed
//     read the history directory unreadable (500).
//
// Where the spec is ahead of the product (see its header), the gap is
// in the page — wikiHash, the router's trailing slash, ProjectView's
// missing slug guard, arrival under a palette — and this level cannot
// see it; the browser flow is where those go red. What this level
// checks is that every read those pages make answers as the spec
// needs: a slug-keyed detail, a 404 that is a 404, a wiki path with a
// space, % and ~ that is a page.
type rrflAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	sid     string // S: listed, one finished turn
	turnSeq int64  // the seq of S's turn, the N of #/s/S/changes?turn=N
	xFound  string // archived: not listed, its transcript loads
	xBroken string // archived, then its transcript made unreadable: a 500
	xGone   string // an id serve never had: a 404
	tid     string // T: an empty session filed under b
	zid     string // Z: an id b does not hold
	hist    string // the history directory

	hash, page, list, overlay, back, detail, shown string
	arrived, acted, pendA, pendB                   bool

	// noSlugGuard is the deliberate bug
	// TestRouteResolutionFirstLoadPathsCatchWrongAdapter injects: a
	// project read that lands is shown whatever the URL names by then
	// (ProjectView.load as the product has it).
	noSlugGuard bool
	// noPick is the one TestRouteResolutionFirstLoadCatchesWrongAdapter
	// injects: the first list read never opens S.
	noPick bool
}

// rrflWikiPage is a page whose path holds a space, a % and a ~: wikiHash
// wrote it raw, which decodeURIComponent and the citation split each
// break on. The server must hold it as an ordinary page.
const rrflWikiPage = "topics/demo/odd page 100% ~draft.md"

func newRRFLAdapter(t *testing.T) *rrflAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &rrflAdapter{
		t: t, s: s, dir: control.Dir(s.Home),
		xGone: "01999999-0000-7000-8000-000000000000",
		zid:   "01999999-0000-7000-8000-0000000000aa",
		hist:  filepath.Join(s.Home, ".bough", "history"),
	}
	ctx, cancel := actionCtx()
	defer cancel()
	work := s.Dir(t, "work")
	for _, id := range []*string{&a.xFound, &a.xBroken, &a.tid, &a.sid} {
		row, err := s.CreateSession(ctx, work, "")
		if err != nil {
			t.Fatal(err)
		}
		*id = row.ID
	}
	for _, id := range []string{a.xFound, a.xBroken} {
		if _, err := s.Archive(ctx, id); err != nil {
			t.Fatal(err)
		}
	}
	for _, name := range []string{"a", "b"} {
		if st, err := a.send(http.MethodPost, "/api/projects", map[string]string{"name": name}, nil); err != nil || st != http.StatusOK {
			t.Fatalf("fixture: create project %s: %d %v", name, st, err)
		}
	}
	if st, err := a.send(http.MethodPost, "/api/sessions/"+a.tid+"/project", map[string]string{"project": "b"}, nil); err != nil || st != http.StatusOK {
		t.Fatalf("fixture: file T under b: %d %v", st, err)
	}
	// S's one turn: what the arrival pick needs (a non-empty session) and
	// what ?turn=N names.
	control.Queue(t, a.dir, "rrfl", control.Turn{Mode: "ok", Text: "finished"})
	if err := s.Prompt(ctx, a.sid, "turn rrfl"); err != nil {
		t.Fatal(err)
	}
	if _, err := waitRow(s, a.sid, "S's turn to finish", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		t.Fatal(err)
	}
	_, lines, err := s.GetSession(ctx, a.sid)
	if err != nil {
		t.Fatal(err)
	}
	for _, l := range lines {
		if l.Kind == "input" {
			a.turnSeq = l.Seq
		}
	}
	if a.turnSeq == 0 {
		t.Fatal("fixture: S's transcript holds no input")
	}
	// xBroken is known to the server (the list caches it by size and
	// mtime, which chmod moves neither of) while reading it fails.
	if _, _, err := s.GetSession(ctx, a.xBroken); err != nil {
		t.Fatal(err)
	}
	broken := filepath.Join(a.hist, a.xBroken+".jsonl")
	if err := os.Chmod(broken, 0); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.Chmod(broken, 0o644) })
	page := filepath.Join(s.Home, ".bough", "wiki", filepath.FromSlash(rrflWikiPage))
	if err := os.MkdirAll(filepath.Dir(page), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(page, []byte("# Odd page\n\nA path with a space, a % and a ~.\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	return a
}

// Init is a fresh tab on #/ whose first list read is still out.
func (a *rrflAdapter) Init() error {
	a.hash, a.page, a.list, a.overlay, a.back, a.detail, a.shown = "home", "overview", "pending", "none", "none", "", ""
	a.arrived, a.acted, a.pendA, a.pendB = false, false, false, false
	a.gate.reset()
	return nil
}

func (a *rrflAdapter) Cleanup() error { return nil }

func (a *rrflAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Route", Index: 0}: a}, nil
}

func (a *rrflAdapter) GetState() (map[string]any, error) {
	return map[string]any{
		"hash": a.hash, "page": a.page, "list": a.list, "arrived": a.arrived, "acted": a.acted,
		"overlay": a.overlay, "back": a.back, "detail": a.detail,
		"pend_a": a.pendA, "pend_b": a.pendB, "shown": a.shown,
	}, nil
}

// send is one request the page makes; it returns the status and decodes
// a 2xx body into out.
func (a *rrflAdapter) send(method, path string, body, out any) (int, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	var rd io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return 0, err
		}
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, a.s.URL+path, rd)
	if err != nil {
		return 0, err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return 0, err
	}
	if out != nil && resp.StatusCode/100 == 2 {
		if err := json.Unmarshal(raw, out); err != nil {
			return 0, fmt.Errorf("%s %s: decode: %w", method, path, err)
		}
	}
	return resp.StatusCode, nil
}

// unreadable runs f while path cannot be read: a read that reaches the
// disk fails, which is how a lookup, the list or a project read gets an
// error rather than an answer from this serve.
func unreadable(path string, f func() error) error {
	fi, err := os.Stat(path)
	if err != nil {
		return err
	}
	if err := os.Chmod(path, 0); err != nil {
		return err
	}
	ferr := f()
	if err := os.Chmod(path, fi.Mode().Perm()); err != nil {
		return err
	}
	return ferr
}

// lookup is the page reading one session: found on a 200, "missing" on
// a 404, "loadfail" on any other answer.
func (a *rrflAdapter) lookup(id, found string) (string, error) {
	st, err := a.send(http.MethodGet, "/api/sessions/"+url.PathEscape(id), nil, nil)
	switch {
	case err != nil:
		return "", err
	case st == http.StatusOK:
		return found, nil
	case st == http.StatusNotFound:
		return "missing", nil
	}
	return "loadfail", nil
}

// showChanges is S's Changes page filtered to turn N: the changes read,
// and a transcript that still holds the turn the link names.
func (a *rrflAdapter) showChanges() (string, error) {
	st, err := a.send(http.MethodGet, "/api/sessions/"+a.sid+"/changes", nil, nil)
	if err != nil {
		return "", err
	}
	if st != http.StatusOK {
		return fmt.Sprintf("changes_failed_%d", st), nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, lines, err := a.s.GetSession(ctx, a.sid)
	if err != nil {
		return "", err
	}
	for _, l := range lines {
		if l.Kind == "input" && l.Seq == a.turnSeq {
			return "changes", nil
		}
	}
	return fmt.Sprintf("changes_no_turn_%d", a.turnSeq), nil
}

// resolve is what a transcript read for the hash puts up (the spec's
// resolved, and landing's once the list holds S).
func (a *rrflAdapter) resolve(h string) (string, error) {
	switch h {
	case "s":
		return a.lookup(a.sid, "session")
	case "changes_q":
		if page, err := a.lookup(a.sid, "changes"); err != nil || page != "changes" {
			return page, err
		}
		return a.showChanges()
	}
	return a.lookup(a.xFound, "other_session")
}

func (a *rrflAdapter) showWiki() (string, error) {
	st, err := a.send(http.MethodGet, "/api/wiki/page?path="+url.QueryEscape(rrflWikiPage), nil, nil)
	if err != nil {
		return "", err
	}
	if st != http.StatusOK {
		return fmt.Sprintf("wiki_failed_%d", st), nil
	}
	return "wiki_page", nil
}

func (a *rrflAdapter) showProjects() (string, error) {
	st, err := a.send(http.MethodGet, "/api/projects", nil, nil)
	if err != nil {
		return "", err
	}
	if st != http.StatusOK {
		return fmt.Sprintf("projects_failed_%d", st), nil
	}
	return "projects", nil
}

// land is the spec's landing, with a read wherever the page renders one.
func (a *rrflAdapter) land(h string) (string, error) {
	switch h {
	case "home":
		return "overview", nil
	case "s", "changes_q":
		if a.list != "ok" {
			return "loading", nil
		}
		return a.resolve(h)
	case "other":
		return "loading", nil
	case "wiki_odd":
		return a.showWiki()
	case "projects":
		return a.showProjects()
	case "pa", "pb_t", "pb_z":
		return "project", nil
	}
	return "lost", nil
}

func rrflSlug(h string) string {
	switch h {
	case "pa":
		return "a"
	case "pb_t", "pb_z":
		return "b"
	}
	return ""
}

// goTo is the spec's go: an arrival at h by a read. A project hash
// starts that project's read; leaving the project pages drops them.
func (a *rrflAdapter) goTo(h string) error {
	page, err := a.land(h)
	if err != nil {
		return err
	}
	a.hash, a.page, a.detail, a.shown = h, page, "", ""
	switch rrflSlug(h) {
	case "":
		a.pendA, a.pendB = false, false
	case "a":
		a.pendA = true
	default:
		a.pendB = true
	}
	return nil
}

func (a *rrflAdapter) touch() { a.acted, a.arrived = true, true }

// --- system: the first list read ---

// readList is GET /api/sessions as the first load makes it: S must be
// the one arrival candidate, and no X listed.
func (a *rrflAdapter) readList() (string, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	rows, err := a.s.ListSessions(ctx, false)
	if err != nil {
		return "", err
	}
	for _, r := range rows {
		if r.ID == a.xFound || r.ID == a.xBroken || r.ID == a.xGone {
			return "", fmt.Errorf("the list holds %s, which it must not", r.ID)
		}
	}
	return arrivalPick(rows, time.Now())
}

// resolveFromList: a link to S resolves once the list answered.
func (a *rrflAdapter) resolveFromList() error {
	if (a.page == "loading" || a.page == "loadfail") && (a.hash == "s" || a.hash == "changes_q") {
		page, err := a.resolve(a.hash)
		a.page = page
		return err
	}
	return nil
}

func (a *rrflAdapter) ListFast() error {
	if !a.gate.pass(a.list == "pending") {
		return nil
	}
	pick, err := a.readList()
	if err != nil {
		return err
	}
	if pick != a.sid {
		return fmt.Errorf("arrival picked %q, not S (%s)", pick, a.sid)
	}
	a.list = "ok"
	if !a.noPick && !a.arrived && a.hash == "home" && a.overlay == "none" && a.page == "overview" {
		// replaceState: back is untouched.
		page, err := a.resolve("s")
		if err != nil {
			return err
		}
		a.hash, a.page = "s", page
	}
	a.arrived = true
	return a.resolveFromList()
}

func (a *rrflAdapter) ListLate() error {
	if !a.gate.pass(a.list == "pending" || a.list == "failed") {
		return nil
	}
	if _, err := a.readList(); err != nil {
		return err
	}
	a.list, a.arrived = "ok", true
	return a.resolveFromList()
}

// ListFail is the first read answered with an error: serve cannot read
// its history directory.
func (a *rrflAdapter) ListFail() error {
	if !a.gate.pass(a.list == "pending") {
		return nil
	}
	var st int
	if err := unreadable(a.hist, func() (err error) {
		st, err = a.send(http.MethodGet, "/api/sessions", nil, nil)
		return err
	}); err != nil {
		return err
	}
	if st/100 == 2 {
		return fmt.Errorf("the list answered %d with its history directory unreadable", st)
	}
	a.list = "failed"
	return nil
}

// --- system: the session lookup ---

func (a *rrflAdapter) Found() error {
	if !a.gate.pass(a.page == "loading") {
		return nil
	}
	page, err := a.resolve(a.hash)
	a.page = page
	return err
}

func (a *rrflAdapter) NotFound() error {
	if !a.gate.pass(a.page == "loading" && a.hash == "other") {
		return nil
	}
	page, err := a.lookup(a.xGone, "other_session")
	a.page = page
	return err
}

// LookupError: X's is the unreadable transcript; S's is S's own
// transcript made unreadable for the one read.
func (a *rrflAdapter) LookupError() error {
	if !a.gate.pass(a.page == "loading") {
		return nil
	}
	if a.hash == "other" {
		page, err := a.lookup(a.xBroken, "other_session")
		a.page = page
		return err
	}
	var page string
	err := unreadable(filepath.Join(a.hist, a.sid+".jsonl"), func() (err error) {
		page, err = a.lookup(a.sid, resolvedPage(a.hash))
		return err
	})
	a.page = page
	return err
}

func resolvedPage(h string) string {
	switch h {
	case "s":
		return "session"
	case "changes_q":
		return "changes"
	}
	return "other_session"
}

// --- system: GET /api/projects/{slug} ---

// readProject is one project read: the detail the page would show, by
// the slug the answer carries.
func (a *rrflAdapter) readProject(slug string) (string, serve.ProjectDetail, error) {
	var d serve.ProjectDetail
	st, err := a.send(http.MethodGet, "/api/projects/"+slug, nil, &d)
	switch {
	case err != nil:
		return "", d, err
	case st == http.StatusOK:
		return d.Slug, d, nil
	case st == http.StatusNotFound:
		return "missing", d, nil
	case st >= 500:
		return "error", d, nil
	}
	return fmt.Sprintf("status_%d", st), d, nil
}

// landsHere says whether a read of slug is shown: only under the URL's
// own project, unless the adapter carries the product's missing guard.
func (a *rrflAdapter) landsHere(slug string) bool {
	if a.noSlugGuard {
		return a.page == "project"
	}
	return rrflSlug(a.hash) == slug
}

func (a *rrflAdapter) DetailA() error {
	if !a.gate.pass(a.pendA) {
		return nil
	}
	a.pendA = false
	detail, _, err := a.readProject("a")
	if err != nil {
		return err
	}
	if a.landsHere("a") {
		a.detail = detail
	}
	return nil
}

// showB puts b's read up, and the deep-linked thread: T when b holds
// it, the note when it does not hold Z.
func (a *rrflAdapter) showB(detail string, d serve.ProjectDetail) {
	if !a.landsHere("b") {
		return
	}
	a.detail = detail
	if detail != "b" {
		return
	}
	held := func(id string) bool {
		return slices.ContainsFunc(d.Threads, func(r serve.Row) bool { return r.ID == id })
	}
	switch {
	case a.hash == "pb_t" && held(a.tid):
		a.shown = "T"
	case a.hash == "pb_z" && !held(a.zid):
		a.shown = "note"
	}
}

func (a *rrflAdapter) DetailB() error {
	if !a.gate.pass(a.pendB) {
		return nil
	}
	a.pendB = false
	detail, d, err := a.readProject("b")
	if err != nil {
		return err
	}
	a.showB(detail, d)
	return nil
}

// DetailBMissing: b's definition directory is gone for the read.
func (a *rrflAdapter) DetailBMissing() error {
	if !a.gate.pass(a.pendB) {
		return nil
	}
	a.pendB = false
	dir := filepath.Join(a.s.Home, ".bough", "projects", "b")
	away := filepath.Join(a.s.Home, ".bough", "projects-away-b")
	if err := os.Rename(dir, away); err != nil {
		return err
	}
	detail, d, err := a.readProject("b")
	if rerr := os.Rename(away, dir); rerr != nil {
		return rerr
	}
	if err != nil {
		return err
	}
	a.showB(detail, d)
	return nil
}

// DetailBError: the read fails in serve (it cannot list sessions).
func (a *rrflAdapter) DetailBError() error {
	if !a.gate.pass(a.pendB) {
		return nil
	}
	a.pendB = false
	var detail string
	var d serve.ProjectDetail
	if err := unreadable(a.hist, func() (err error) {
		detail, d, err = a.readProject("b")
		return err
	}); err != nil {
		return err
	}
	a.showB(detail, d)
	return nil
}

// --- the person: typed links ---

func (a *rrflAdapter) typed(h string, enabled bool) error {
	if !a.gate.pass(a.overlay == "none" && enabled) {
		return nil
	}
	back := a.hash
	if err := a.goTo(h); err != nil {
		return err
	}
	a.back = back
	a.touch()
	return nil
}

func (a *rrflAdapter) OpenSessionLink() error {
	return a.typed("s", a.back == "none" && a.hash != "s")
}

func (a *rrflAdapter) OpenOtherLink() error {
	return a.typed("other", a.back == "none" && a.hash != "other")
}

func (a *rrflAdapter) OpenChangesLink() error {
	return a.typed("changes_q", a.back == "none" && a.hash != "changes_q")
}

// OpenSlashLink: #/projects/ is the projects page.
func (a *rrflAdapter) OpenSlashLink() error {
	return a.typed("projects", a.back == "none" && a.hash != "projects")
}

func (a *rrflAdapter) OpenUnknownLink() error {
	return a.typed("unknown", a.back == "none" && a.hash != "unknown")
}

func (a *rrflAdapter) OpenProjectLink() error {
	return a.typed("pa", a.back == "none" && a.hash == "home")
}

func (a *rrflAdapter) FollowToB() error {
	return a.typed("pb_t", a.hash == "pa" && a.back == "home")
}

func (a *rrflAdapter) FollowToZ() error {
	return a.typed("pb_z", a.hash == "pa" && a.back == "home")
}

// --- the person: pushState navigation ---

func (a *rrflAdapter) OpenOddWikiPage() error {
	if !a.gate.pass(a.overlay == "none" && a.back == "none" && a.page == "overview") {
		return nil
	}
	page, err := a.showWiki()
	if err != nil {
		return err
	}
	a.back, a.hash, a.page = a.hash, "wiki_odd", page
	a.touch()
	return nil
}

func (a *rrflAdapter) failed() bool { return a.page == "missing" || a.page == "loadfail" }

func (a *rrflAdapter) NavProjects() error {
	return a.typed("projects", (a.back == "none" && a.page == "overview") || a.failed())
}

func (a *rrflAdapter) GoHome() error {
	return a.typed("home", (a.back == "home" && (a.hash == "projects" || a.hash == "wiki_odd")) || a.failed())
}

func (a *rrflAdapter) Retry() error {
	if a.gate.pass(a.overlay == "none" && a.page == "loadfail") {
		a.page = "loading"
	}
	return nil
}

// --- the palette ---

func (a *rrflAdapter) OpenPalette() error {
	if a.gate.pass(a.overlay == "none" && a.page == "overview") {
		a.overlay = "pal"
		a.touch()
	}
	return nil
}

func (a *rrflAdapter) ClosePalette() error {
	if a.gate.pass(a.overlay == "pal") {
		a.overlay = "none"
	}
	return nil
}

// --- the browser ---

func (a *rrflAdapter) Back() error {
	if !a.gate.pass(a.overlay == "none" && a.back != "none") {
		return nil
	}
	h := a.back
	a.back = "none"
	if err := a.goTo(h); err != nil {
		return err
	}
	a.touch()
	return nil
}

func (a *rrflAdapter) Reload() error {
	if !a.gate.pass(a.overlay == "none") {
		return nil
	}
	a.list, a.arrived, a.acted = "pending", false, false
	return a.goTo(a.hash)
}

var routeResolutionFirstLoadActions = map[string]map[string]fmbt.ActionFunc{"Route": {
	"ListFast":        action((*rrflAdapter).ListFast),
	"ListLate":        action((*rrflAdapter).ListLate),
	"ListFail":        action((*rrflAdapter).ListFail),
	"Found":           action((*rrflAdapter).Found),
	"NotFound":        action((*rrflAdapter).NotFound),
	"LookupError":     action((*rrflAdapter).LookupError),
	"DetailA":         action((*rrflAdapter).DetailA),
	"DetailB":         action((*rrflAdapter).DetailB),
	"DetailBMissing":  action((*rrflAdapter).DetailBMissing),
	"DetailBError":    action((*rrflAdapter).DetailBError),
	"OpenSessionLink": action((*rrflAdapter).OpenSessionLink),
	"OpenOtherLink":   action((*rrflAdapter).OpenOtherLink),
	"OpenChangesLink": action((*rrflAdapter).OpenChangesLink),
	"OpenSlashLink":   action((*rrflAdapter).OpenSlashLink),
	"OpenUnknownLink": action((*rrflAdapter).OpenUnknownLink),
	"OpenProjectLink": action((*rrflAdapter).OpenProjectLink),
	"FollowToB":       action((*rrflAdapter).FollowToB),
	"FollowToZ":       action((*rrflAdapter).FollowToZ),
	"OpenOddWikiPage": action((*rrflAdapter).OpenOddWikiPage),
	"NavProjects":     action((*rrflAdapter).NavProjects),
	"GoHome":          action((*rrflAdapter).GoHome),
	"Retry":           action((*rrflAdapter).Retry),
	"OpenPalette":     action((*rrflAdapter).OpenPalette),
	"ClosePalette":    action((*rrflAdapter).ClosePalette),
	"Back":            action((*rrflAdapter).Back),
	"Reload":          action((*rrflAdapter).Reload),
}}

// Every step is a read or two and no turn, and most random walks end on
// a disabled action among 26, so many short walks. Seedless.
func routeResolutionFirstLoadOptions() map[string]any {
	return map[string]any{"max-seq-runs": 300, "max-actions": 8, "max-parallel-runs": 0}
}

// routeResolutionFirstLoadHistory: navigation writes nothing to a
// transcript, so what S's history can say is what the first list read
// does with S. A transcript holding a finished turn is a session the
// arrival pick opens (#/ → #/s/S on a quiet overview); one without is
// not a candidate, and the pick leaving #/ is no path in the model.
func routeResolutionFirstLoadHistory(entries []history.Entry) []tracecheck.Step {
	asked, finished := false, false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			asked, finished = true, false
		case "done":
			finished = asked
		}
	}
	after := map[string]any{"Route#0.list": "ok", "Route#0.hash": "home", "Route#0.page": "overview"}
	if finished {
		after = map[string]any{"Route#0.list": "ok", "Route#0.hash": "s", "Route#0.page": "session"}
	}
	return []tracecheck.Step{
		{Action: "Init", State: map[string]any{"Route#0.hash": "home", "Route#0.list": "pending"}},
		{Action: "Route#0.ListFast", State: after},
	}
}

func init() { historyProjections["route_resolution_first_load"] = routeResolutionFirstLoadHistory }

func TestRouteResolutionFirstLoad(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newRRFLAdapter(t)
	if err := runMBT(t, "route_resolution_first_load", a, routeResolutionFirstLoadActions, routeResolutionFirstLoadOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkRRFLHistory(t, a)
}

// TestRouteResolutionFirstLoadPaths walks every generated path through
// the adapter: the random walks mostly die on a disabled action, so
// this is what takes every state (every link under
// MODEL_COVER=transitions) against the server.
func TestRouteResolutionFirstLoadPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newRRFLAdapter(t)
	if err := walkRRFLPaths(a, envCover()); err != nil {
		t.Fatal(err)
	}
	checkRRFLHistory(t, a)
}

func checkRRFLHistory(t *testing.T, a *rrflAdapter) {
	t.Helper()
	g, err := tracecheck.Load(fizzCheck(t, "route_resolution_first_load"))
	if err != nil {
		t.Fatal(err)
	}
	checkHistory(t, g, sessionHistory(t, a.s.Home, a.sid), routeResolutionFirstLoadHistory)
}

// The projection must be able to fail: a transcript with no finished
// turn (T's, empty) is not a session the pick opens.
func TestRouteResolutionFirstLoadHistoryProjection(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	g, err := tracecheck.Load(fizzCheck(t, "route_resolution_first_load"))
	if err != nil {
		t.Fatal(err)
	}
	done := []history.Entry{{Kind: "input"}, {Kind: "assistant"}, {Kind: "done"}}
	if v := g.Check(routeResolutionFirstLoadHistory(done)); v != nil {
		t.Errorf("a finished turn: %v", v)
	}
	for name, entries := range map[string][]history.Entry{
		"empty":   nil,
		"running": {{Kind: "input"}},
	} {
		if v := g.Check(routeResolutionFirstLoadHistory(entries)); v == nil {
			t.Errorf("%s transcript replayed as a session the pick opens", name)
		}
	}
}

// walkRRFLPaths drives a down every walk and compares its state with the
// walk's after Init and after every step.
func walkRRFLPaths(a *rrflAdapter, cover tracecheck.Cover) error {
	raw, err := pathsJSONCover("route_resolution_first_load", cover)
	if err != nil {
		return err
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		return err
	}
	for pi, p := range doc.Paths {
		for si, step := range p.Trace {
			name := strings.TrimPrefix(step.Action, "Route#0.")
			if si == 0 {
				err = a.Init()
			} else if f, ok := routeResolutionFirstLoadActions["Route"][name]; !ok {
				return fmt.Errorf("path %d step %d: no adapter action for %q", pi, si, step.Action)
			} else {
				_, err = f(a, nil)
			}
			if err != nil {
				return fmt.Errorf("path %d step %d (%s): %w", pi, si, name, err)
			}
			if a.gate.off {
				return fmt.Errorf("path %d step %d: %s is enabled in the model but not in the adapter's view", pi, si, name)
			}
			want := map[string]any{}
			for k, v := range step.State {
				if f, ok := strings.CutPrefix(k, "Route#0."); ok {
					want[f] = v
				}
			}
			got, _ := a.GetState()
			if !reflect.DeepEqual(got, want) {
				return fmt.Errorf("path %d step %d (%s): state\n got %v\nwant %v", pi, si, name, got, want)
			}
		}
	}
	if len(doc.Paths) == 0 {
		return fmt.Errorf("no paths")
	}
	return nil
}

// A project read shown under whatever the URL names by the time it
// lands — ProjectView.load without a slug guard — must fail the walk.
// It shows on one link (DetailA after FollowToB/Z), so every link.
func TestRouteResolutionFirstLoadPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newRRFLAdapter(t)
	a.noSlugGuard = true
	err := walkRRFLPaths(a, tracecheck.CoverTransitions)
	if err == nil {
		t.Fatal("a project read shown under another slug walked every path; the walk is not checking state")
	}
	t.Logf("caught: %v", err)
}

// The random runs prove nothing unless an adapter that breaks the model
// fails them: a first list read that never opens S is the first step
// of a walk often enough to be caught.
func TestRouteResolutionFirstLoadCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newRRFLAdapter(t)
	a.noPick = true
	err := runMBT(t, "route_resolution_first_load", a, routeResolutionFirstLoadActions, routeResolutionFirstLoadOptions())
	if err == nil {
		t.Fatal("a run whose first list read never opens S passed; the runner is not checking state")
	}
	// Any runner error passes this test, a broken runner included (the
	// socket under a long TMPDIR once did), so the log says which.
	t.Logf("caught: %v", err)
}
