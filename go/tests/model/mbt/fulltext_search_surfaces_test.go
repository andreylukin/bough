//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"slices"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/fulltext_search_surfaces.fizz at the server level: the sidebar
// filter's and the palette's full-text reads against a real serve.
//
// The fixture is the spec's, made once per serve; no walk writes
// anything, so every walk shares it:
//
//	A  prompted, then renamed "Plover notes": a title hit for "plover";
//	   nothing it said holds a query.
//	B  prompted; its reply (not its first line, which is its default
//	   title) says "gannet".
//	E  started with no prompt in a folder named plover-nest, then
//	   archived and unarchived so its child is gone: empty and dead, a
//	   path hit for "plover".
//	Z  prompted; its reply says "osprey"; archived.
//	W  the wiki page topics/birds/seabirds.md, one claim naming the gannet.
//
// The spec's abstract queries are these words (ftQuery). They are words
// no temp path, id or tool output holds: the page matches a query
// against a row's cwd, and "arch" or "title" would hit every row whose
// temp dir carries the test's name.
//
// What the page decides (debounce, "No sessions match" vs "Searching…",
// the Wiki group's stale hits, the palette's jump for a listed session)
// is the page's, which the adapter keeps with the spec's rules; the
// browser flow is where those go red. What the server decides the
// adapter reads off it every time a page would: what /api/search and
// /api/wiki/search answer for each query, which line a hit names (and
// that the transcript holds the word there), the list with and without
// the archived rows, which rows are empty and dead, and whether the wiki
// page a hit names opens. rows, note and elsewhere are derived from that
// list with the page's own rules (getSearchMatch, the empty-row rule,
// saidElsewhere in web/src/app.tsx).
type fulltextSearchSurfacesAdapter struct {
	t    *testing.T
	s    *servetest.Server
	gate gate

	letter map[string]string // session id -> A, B, E, Z
	ids    map[string]string // the other way

	// The page.
	field, archived, palette   bool
	query, search, pq, psearch string
	wiki, open, at, page       string
	said, found                map[string]serve.SearchHit // by id
	wikiRows                   []string

	// archivedUnread is the deliberate bug
	// TestFulltextSearchSurfacesCatchesWrongAdapter injects: the list is
	// read without its archived rows even with Archived included.
	archivedUnread bool
}

// ftQuery is the concrete text each abstract query types.
var ftQuery = map[string]string{
	"empty": "", "short": "§", "title": "plover", "said": "gannet", "arch": "osprey", "none": "kestrel",
}

const ftWikiPage = "topics/birds/seabirds.md"

func newFulltextSearchSurfacesAdapter(t *testing.T) *fulltextSearchSurfacesAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig, Files: map[string]string{
		".bough/wiki/" + ftWikiPage: "# Seabirds\n\nNotes on the coast.\n\n## Facts\n\n- The gannet dives from thirty metres.\n",
		".bough/wiki/index.md":      "# Wiki index\n\n## birds\n\n- [Seabirds](" + ftWikiPage + ") — the coast\n",
	}})
	a := &fulltextSearchSurfacesAdapter{t: t, s: s, letter: map[string]string{}, ids: map[string]string{}}
	ctx, cancel := context.WithTimeout(context.Background(), 4*actionTimeout)
	defer cancel()
	dir := control.Dir(s.Home)
	work := s.Dir(t, "work")
	for i, f := range []struct{ letter, prompt, reply string }{
		{"A", "good day", "fine, thanks"},
		{"B", "hello there", "a gannet dives offshore"},
		{"Z", "morning", "an osprey circles the lake"},
	} {
		row, err := s.CreateSession(ctx, work, "")
		if err != nil {
			t.Fatal(err)
		}
		name := fmt.Sprintf("f%d", i)
		control.Queue(t, dir, name, control.Turn{Mode: "ok", Text: f.reply})
		if err := s.Prompt(ctx, row.ID, f.prompt); err != nil {
			t.Fatal(err)
		}
		if _, err := s.WaitSession(ctx, row.ID, func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
			t.Fatalf("fixture %s: %v", f.letter, err)
		}
		a.letter[row.ID], a.ids[f.letter] = f.letter, row.ID
	}
	if err := s.Rename(ctx, a.ids["A"], "Plover notes"); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Archive(ctx, a.ids["Z"]); err != nil {
		t.Fatal(err)
	}
	row, err := s.CreateSession(ctx, s.Dir(t, "plover-nest"), "")
	if err != nil {
		t.Fatal(err)
	}
	a.letter[row.ID], a.ids["E"] = "E", row.ID
	// Archiving ends a session's child; unarchiving does not start one.
	if _, err := s.Archive(ctx, row.ID); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Unarchive(ctx, row.ID); err != nil {
		t.Fatal(err)
	}
	if _, err := s.WaitSession(ctx, row.ID, func(r serve.Row) bool { return !r.Live && r.Empty && !r.Archived }); err != nil {
		t.Fatalf("fixture E: %v", err)
	}
	return a
}

func (a *fulltextSearchSurfacesAdapter) Init() error {
	a.gate.reset()
	a.field, a.archived, a.palette = false, false, false
	a.query, a.search, a.pq, a.psearch, a.wiki = "empty", "idle", "empty", "idle", "idle"
	a.open, a.at, a.page = "", "", ""
	a.said, a.found, a.wikiRows = nil, nil, nil
	return nil
}

func (a *fulltextSearchSurfacesAdapter) Cleanup() error { return nil }

func (a *fulltextSearchSurfacesAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Search", Index: 0}: a}, nil
}

// name is a session as the spec calls it; an id the fixture did not
// make stays itself, so the mismatch names it.
func (a *fulltextSearchSurfacesAdapter) name(id string) string {
	if l, ok := a.letter[id]; ok {
		return l
	}
	return id
}

// names lists hits' sessions in the spec's order (A, B, E, Z).
func (a *fulltextSearchSurfacesAdapter) names(ids []string) []any {
	var out []string
	for _, id := range ids {
		out = append(out, a.name(id))
	}
	slices.Sort(out)
	res := []any{}
	for _, n := range out {
		res = append(res, n)
	}
	return res
}

func hitIDs(m map[string]serve.SearchHit) []string {
	var ids []string
	for id := range m {
		ids = append(ids, id)
	}
	return ids
}

// view is what the sidebar shows, read off the list the way the page
// reads it.
type ftView struct {
	rows      []string // ids listed, filtered by the query
	local     map[string]bool
	note      string
	elsewhere int
}

// ftLocalHit is getSearchMatch: the title, branch, repo, path or id
// holds the query.
func ftLocalHit(r serve.Row, q string) bool {
	if q == "" {
		return false
	}
	for _, f := range []string{r.Title, r.Branch, r.Repo, r.Cwd, r.ID} {
		if strings.Contains(strings.ToLower(f), q) {
			return true
		}
	}
	return false
}

func (a *fulltextSearchSurfacesAdapter) view() (ftView, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	listed, err := a.s.ListSessions(ctx, a.archived && !a.archivedUnread)
	if err != nil {
		return ftView{}, err
	}
	q := strings.ToLower(strings.TrimSpace(ftQuery[a.query]))
	v := ftView{local: map[string]bool{}}
	in := map[string]bool{}
	for _, r := range listed {
		in[r.ID] = true
		// The empty-row rule: an empty, dead session lists only for a
		// query (or while open).
		if q == "" && r.Empty && !r.Live && !r.Archived && r.ID != a.ids[a.open] {
			continue
		}
		hit := ftLocalHit(r, q)
		if _, said := a.said[r.ID]; q == "" || hit || said {
			v.rows = append(v.rows, r.ID)
		}
		v.local[r.ID] = hit
	}
	for id := range a.said {
		if !in[id] {
			v.elsewhere++
		}
	}
	if q != "" {
		switch {
		case a.search == "error":
			v.note = "failed"
		case len(v.rows) == 0 && a.search == "loading":
			v.note = "searching"
		case len(v.rows) == 0:
			v.note = "nomatch"
		}
	}
	return v, nil
}

func (a *fulltextSearchSurfacesAdapter) GetState() (map[string]any, error) {
	v, err := a.view()
	if err != nil {
		return nil, err
	}
	wikiRows, wikiSays := []any{}, ""
	for _, w := range a.wikiRows {
		wikiRows = append(wikiRows, w)
	}
	if a.palette && a.wiki == "error" {
		wikiSays = "failed"
	}
	return map[string]any{
		"field": a.field, "query": a.query, "search": a.search, "said": a.names(hitIDs(a.said)),
		"archived": a.archived, "open": a.open, "at": a.at, "page": a.page,
		"palette": a.palette, "pq": a.pq, "psearch": a.psearch, "found": a.names(hitIDs(a.found)),
		"wiki": a.wiki, "wikiRows": wikiRows,
		"rows": a.names(v.rows), "note": v.note, "elsewhere": v.elsewhere, "wikiSays": wikiSays,
	}, nil
}

// get is a GET the page makes, with token: the server's token, or one
// it does not know when the read is to fail for real. It returns the
// status for a non-2xx answer.
func (a *fulltextSearchSurfacesAdapter) get(path, token string, out any) (int, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, a.s.URL+path, nil)
	if err != nil {
		return 0, err
	}
	req.Header.Set("Authorization", "Bearer "+token)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, err
	}
	defer resp.Body.Close()
	b, err := io.ReadAll(resp.Body)
	if err != nil {
		return 0, err
	}
	if resp.StatusCode/100 != 2 {
		return resp.StatusCode, nil
	}
	if err := json.Unmarshal(b, out); err != nil {
		return 0, fmt.Errorf("GET %s: %w: %s", path, err, b)
	}
	return resp.StatusCode, nil
}

// textSearch is useFullText's read. ok is false when the server refused
// it; the hits kept are those with a line, as the sidebar's `said` does.
func (a *fulltextSearchSurfacesAdapter) textSearch(query, token string) (map[string]serve.SearchHit, bool, error) {
	var d struct {
		Hits []serve.SearchHit `json:"hits"`
	}
	st, err := a.get("/api/search?q="+url.QueryEscape(ftQuery[query]), token, &d)
	if err != nil || st/100 != 2 {
		return nil, false, err
	}
	out := map[string]serve.SearchHit{}
	for _, h := range d.Hits {
		if len(h.Lines) > 0 {
			out[h.ID] = h
		}
	}
	return out, true, nil
}

// lineHolds checks the jump's target: the hit's first line is an entry
// of the session's transcript that holds the query.
func (a *fulltextSearchSurfacesAdapter) lineHolds(h serve.SearchHit, query string) error {
	ctx, cancel := actionCtx()
	defer cancel()
	_, entries, err := a.s.GetSession(ctx, h.ID)
	if err != nil {
		return err
	}
	seq := h.Lines[0].Seq
	for _, e := range entries {
		if e.Seq == seq {
			if !strings.Contains(strings.ToLower(e.Text), ftQuery[query]) {
				return fmt.Errorf("the hit's line %s#%d says %q, not %q", a.name(h.ID), seq, e.Text, ftQuery[query])
			}
			return nil
		}
	}
	return fmt.Errorf("the hit's line %s#%d is no entry of its transcript", a.name(h.ID), seq)
}

func (a *fulltextSearchSurfacesAdapter) quiet() bool {
	return a.open == "" && a.page == "" && !a.palette
}

// --- the sidebar filter ---

func (a *fulltextSearchSurfacesAdapter) OpenFilter() error {
	if a.gate.pass(a.quiet() && !a.field) {
		a.field = true
	}
	return nil
}

func (a *fulltextSearchSurfacesAdapter) setQuery(q string) {
	a.query, a.said = q, nil
	if q == "empty" || q == "short" {
		a.search = "idle"
	} else {
		a.search = "loading"
	}
	if q == "empty" {
		a.archived = false
	}
}

func (a *fulltextSearchSurfacesAdapter) typeQuery(q string) error {
	if a.gate.pass(a.quiet() && a.field && a.query != q) {
		a.setQuery(q)
	}
	return nil
}

func (a *fulltextSearchSurfacesAdapter) TypeShort() error { return a.typeQuery("short") }
func (a *fulltextSearchSurfacesAdapter) TypeTitle() error { return a.typeQuery("title") }
func (a *fulltextSearchSurfacesAdapter) TypeSaid() error  { return a.typeQuery("said") }
func (a *fulltextSearchSurfacesAdapter) TypeArch() error  { return a.typeQuery("arch") }
func (a *fulltextSearchSurfacesAdapter) TypeNone() error  { return a.typeQuery("none") }

func (a *fulltextSearchSurfacesAdapter) Clear() error {
	if a.gate.pass(a.quiet() && a.field && a.query != "empty") {
		a.setQuery("empty")
	}
	return nil
}

func (a *fulltextSearchSurfacesAdapter) Escape() error {
	if a.gate.pass(a.quiet() && a.field) {
		a.setQuery("empty")
		a.field = false
	}
	return nil
}

func (a *fulltextSearchSurfacesAdapter) settleSearch(token string) error {
	if !a.gate.pass(a.search == "loading") {
		return nil
	}
	hits, ok, err := a.textSearch(a.query, token)
	if err != nil {
		return err
	}
	a.said = hits
	if ok {
		a.search = "done"
	} else {
		a.search = "error"
	}
	return nil
}

func (a *fulltextSearchSurfacesAdapter) SearchOk() error { return a.settleSearch(a.s.Token) }

// SearchFails asks with a token the server does not know, so the read
// really fails.
func (a *fulltextSearchSurfacesAdapter) SearchFails() error { return a.settleSearch("not-the-token") }

func (a *fulltextSearchSurfacesAdapter) Retry() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if a.gate.pass(a.quiet() && v.note == "failed") {
		a.search = "loading"
	}
	return nil
}

func (a *fulltextSearchSurfacesAdapter) ToggleArchived() error {
	if a.gate.pass(a.quiet() && a.query != "empty") {
		a.archived = !a.archived
	}
	return nil
}

// selectRow is the sidebar's onSelect for the first row (in the spec's
// order) that local says is or is not a local hit: a row listed only
// for what it said opens at that line.
func (a *fulltextSearchSurfacesAdapter) selectRow(local bool) error {
	v, err := a.view()
	if err != nil {
		return err
	}
	var pick []string
	for _, id := range v.rows {
		if v.local[id] == local {
			pick = append(pick, id)
		}
	}
	slices.SortFunc(pick, func(x, y string) int { return strings.Compare(a.name(x), a.name(y)) })
	if !a.gate.pass(a.quiet() && a.query != "empty" && len(pick) > 0) {
		return nil
	}
	id := pick[0]
	a.open, a.at = a.name(id), "top"
	if h, ok := a.said[id]; ok && !local {
		if err := a.lineHolds(h, a.query); err != nil {
			return err
		}
		a.at = "line"
	}
	return nil
}

func (a *fulltextSearchSurfacesAdapter) SelectTitleRow() error { return a.selectRow(true) }
func (a *fulltextSearchSurfacesAdapter) SelectSaidRow() error  { return a.selectRow(false) }

func (a *fulltextSearchSurfacesAdapter) openPalette(pq string) {
	a.palette, a.pq, a.found, a.wikiRows = true, pq, nil, nil
	if pq == "empty" {
		a.psearch, a.wiki = "idle", "idle"
	} else {
		a.psearch, a.wiki = "loading", "loading"
	}
}

// OpenElsewhere is "N more found in the conversation · ⌘K".
func (a *fulltextSearchSurfacesAdapter) OpenElsewhere() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if a.gate.pass(a.quiet() && a.query != "empty" && v.elsewhere > 0) {
		a.openPalette(a.query)
	}
	return nil
}

func (a *fulltextSearchSurfacesAdapter) Back() error {
	if a.gate.pass(a.open != "" || a.page != "") {
		a.open, a.at, a.page = "", "", ""
	}
	return nil
}

// --- the palette ---

func (a *fulltextSearchSurfacesAdapter) OpenPalette() error {
	if a.gate.pass(a.quiet() && a.query == "empty") {
		a.openPalette("empty")
	}
	return nil
}

func (a *fulltextSearchSurfacesAdapter) typePq(q string) error {
	if a.gate.pass(a.palette && a.pq != q) {
		a.openPalette(q)
	}
	return nil
}

func (a *fulltextSearchSurfacesAdapter) PTypeSaid() error { return a.typePq("said") }
func (a *fulltextSearchSurfacesAdapter) PTypeNone() error { return a.typePq("none") }
func (a *fulltextSearchSurfacesAdapter) PClear() error    { return a.typePq("empty") }

func (a *fulltextSearchSurfacesAdapter) settlePSearch(token string) error {
	if !a.gate.pass(a.palette && a.psearch == "loading") {
		return nil
	}
	hits, ok, err := a.textSearch(a.pq, token)
	if err != nil {
		return err
	}
	a.found = hits
	if ok {
		a.psearch = "done"
	} else {
		a.psearch = "error"
	}
	return nil
}

func (a *fulltextSearchSurfacesAdapter) PSearchOk() error    { return a.settlePSearch(a.s.Token) }
func (a *fulltextSearchSurfacesAdapter) PSearchFails() error { return a.settlePSearch("not-the-token") }

func (a *fulltextSearchSurfacesAdapter) PRetry() error {
	if a.gate.pass(a.palette && a.psearch == "error") {
		a.psearch = "loading"
	}
	return nil
}

// settleWiki is useWikiHits's read: the pages whose claims mention the
// query, W by its path.
func (a *fulltextSearchSurfacesAdapter) settleWiki(token string) error {
	if !a.gate.pass(a.palette && a.wiki == "loading") {
		return nil
	}
	var d struct {
		Hits []struct {
			Path string `json:"path"`
		} `json:"hits"`
	}
	st, err := a.get("/api/wiki/search?q="+url.QueryEscape(ftQuery[a.pq]), token, &d)
	if err != nil {
		return err
	}
	a.wikiRows = nil
	if st/100 != 2 {
		a.wiki = "error"
		return nil
	}
	a.wiki = "done"
	for _, h := range d.Hits {
		if h.Path == ftWikiPage {
			a.wikiRows = append(a.wikiRows, "W")
		} else {
			a.wikiRows = append(a.wikiRows, h.Path)
		}
	}
	return nil
}

func (a *fulltextSearchSurfacesAdapter) WikiAnswer() error { return a.settleWiki(a.s.Token) }
func (a *fulltextSearchSurfacesAdapter) WikiFail() error   { return a.settleWiki("not-the-token") }

func (a *fulltextSearchSurfacesAdapter) closePalette() {
	a.palette, a.pq, a.psearch, a.wiki = false, "empty", "idle", "idle"
	a.found, a.wikiRows = nil, nil
}

// PickSession is a Mentioned in row: the first found session, at the
// line that said it (the spec's; the product's jump for a listed
// session is the page's business).
func (a *fulltextSearchSurfacesAdapter) PickSession() error {
	ids := hitIDs(a.found)
	slices.SortFunc(ids, func(x, y string) int { return strings.Compare(a.name(x), a.name(y)) })
	if !a.gate.pass(a.palette && len(ids) > 0) {
		return nil
	}
	if err := a.lineHolds(a.found[ids[0]], a.pq); err != nil {
		return err
	}
	a.open, a.at = a.name(ids[0]), "line"
	a.closePalette()
	return nil
}

// PickWikiHit opens the page the hit names: goWiki({at: "page", path}).
func (a *fulltextSearchSurfacesAdapter) PickWikiHit() error {
	if !a.gate.pass(a.palette && slices.Contains(a.wikiRows, "W")) {
		return nil
	}
	var pg struct {
		Path string `json:"path"`
	}
	st, err := a.get("/api/wiki/page?path="+url.QueryEscape(ftWikiPage), a.s.Token, &pg)
	if err != nil {
		return err
	}
	if st/100 != 2 {
		return fmt.Errorf("the wiki hit's page answered %d", st)
	}
	a.page = "W"
	a.closePalette()
	return nil
}

func (a *fulltextSearchSurfacesAdapter) PEscape() error {
	if a.gate.pass(a.palette) {
		a.closePalette()
	}
	return nil
}

var fulltextSearchSurfacesActions = map[string]map[string]fmbt.ActionFunc{"Search": {
	"OpenFilter":     action((*fulltextSearchSurfacesAdapter).OpenFilter),
	"TypeShort":      action((*fulltextSearchSurfacesAdapter).TypeShort),
	"TypeTitle":      action((*fulltextSearchSurfacesAdapter).TypeTitle),
	"TypeSaid":       action((*fulltextSearchSurfacesAdapter).TypeSaid),
	"TypeArch":       action((*fulltextSearchSurfacesAdapter).TypeArch),
	"TypeNone":       action((*fulltextSearchSurfacesAdapter).TypeNone),
	"Clear":          action((*fulltextSearchSurfacesAdapter).Clear),
	"Escape":         action((*fulltextSearchSurfacesAdapter).Escape),
	"SearchOk":       action((*fulltextSearchSurfacesAdapter).SearchOk),
	"SearchFails":    action((*fulltextSearchSurfacesAdapter).SearchFails),
	"Retry":          action((*fulltextSearchSurfacesAdapter).Retry),
	"ToggleArchived": action((*fulltextSearchSurfacesAdapter).ToggleArchived),
	"SelectTitleRow": action((*fulltextSearchSurfacesAdapter).SelectTitleRow),
	"SelectSaidRow":  action((*fulltextSearchSurfacesAdapter).SelectSaidRow),
	"OpenElsewhere":  action((*fulltextSearchSurfacesAdapter).OpenElsewhere),
	"Back":           action((*fulltextSearchSurfacesAdapter).Back),
	"OpenPalette":    action((*fulltextSearchSurfacesAdapter).OpenPalette),
	"PTypeSaid":      action((*fulltextSearchSurfacesAdapter).PTypeSaid),
	"PTypeNone":      action((*fulltextSearchSurfacesAdapter).PTypeNone),
	"PClear":         action((*fulltextSearchSurfacesAdapter).PClear),
	"PSearchOk":      action((*fulltextSearchSurfacesAdapter).PSearchOk),
	"PSearchFails":   action((*fulltextSearchSurfacesAdapter).PSearchFails),
	"PRetry":         action((*fulltextSearchSurfacesAdapter).PRetry),
	"WikiAnswer":     action((*fulltextSearchSurfacesAdapter).WikiAnswer),
	"WikiFail":       action((*fulltextSearchSurfacesAdapter).WikiFail),
	"PickSession":    action((*fulltextSearchSurfacesAdapter).PickSession),
	"PickWikiHit":    action((*fulltextSearchSurfacesAdapter).PickWikiHit),
	"PEscape":        action((*fulltextSearchSurfacesAdapter).PEscape),
}}

// Every step is a read or two and no walk writes, so many walks are
// cheap; most die early on a disabled action among 28. Seedless.
func fulltextSearchSurfacesOptions() map[string]any {
	return map[string]any{"max-seq-runs": 300, "max-actions": 8, "max-parallel-runs": 0}
}

// fulltextSearchSurfacesHistory reads one fixture transcript as the
// searches it answers. A transcript that holds a query's word is found
// by that query, so it projects to typing the word and the search
// answering with this session, then opening it at the line: B for
// "gannet", Z (once Archived is included) for "osprey". A transcript
// holding "plover" or "kestrel" is one the model forbids: no transcript
// answers the title's or the phrase nothing holds.
func fulltextSearchSurfacesHistory(entries []history.Entry) []tracecheck.Step {
	q := func(kv ...any) map[string]any {
		m := map[string]any{}
		for i := 0; i < len(kv); i += 2 {
			m["Search#0."+kv[i].(string)] = kv[i+1]
		}
		return m
	}
	holds := func(word string) bool {
		for _, e := range entries {
			if strings.Contains(strings.ToLower(history.EntryText(e)), word) {
				return true
			}
		}
		return false
	}
	steps := []tracecheck.Step{
		{Action: "Init", State: q("query", "empty", "said", []any{})},
		{Action: "Search#0.OpenFilter", State: q("field", true)},
	}
	for _, f := range []struct{ query, typ, who string }{
		{"said", "TypeSaid", "B"}, {"arch", "TypeArch", "Z"}, {"title", "TypeTitle", "A"}, {"none", "TypeNone", "A"},
	} {
		if !holds(ftQuery[f.query]) {
			continue
		}
		steps = append(steps,
			tracecheck.Step{Action: "Search#0." + f.typ, State: q("query", f.query, "search", "loading")},
			tracecheck.Step{Action: "Search#0.SearchOk", State: q("search", "done", "said", []any{f.who})})
		if f.query == "arch" {
			steps = append(steps, tracecheck.Step{Action: "Search#0.ToggleArchived", State: q("archived", true, "rows", []any{"Z"})})
		}
		steps = append(steps,
			tracecheck.Step{Action: "Search#0.SelectSaidRow", State: q("open", f.who, "at", "line")},
			tracecheck.Step{Action: "Search#0.Back", State: q("open", "")},
			tracecheck.Step{Action: "Search#0.Clear", State: q("query", "empty", "archived", false)})
	}
	return steps
}

func init() { historyProjections["fulltext_search_surfaces"] = fulltextSearchSurfacesHistory }

// checkFulltextSearchSurfacesHistories trace-checks the four fixture
// sessions' transcripts.
func checkFulltextSearchSurfacesHistories(t *testing.T, a *fulltextSearchSurfacesAdapter) {
	t.Helper()
	g, err := tracecheck.Load(fizzCheck(t, "fulltext_search_surfaces"))
	if err != nil {
		t.Fatal(err)
	}
	for _, l := range []string{"A", "B", "E", "Z"} {
		checkHistory(t, g, sessionHistory(t, a.s.Home, a.ids[l]), fulltextSearchSurfacesHistory)
	}
}

// walkFulltextSearchSurfacesPaths walks every derived path through the
// adapter and compares the role's state with the spec's after each step.
// Every path runs, so one run reports every divergence.
func walkFulltextSearchSurfacesPaths(a *fulltextSearchSurfacesAdapter, cover tracecheck.Cover) error {
	b, err := pathsJSONCover("fulltext_search_surfaces", cover)
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
	if len(doc.Paths) == 0 {
		return errors.New("no paths")
	}
	var errs []error
	for i, p := range doc.Paths {
		if err := a.walk(p.Trace); err != nil {
			var names []string
			for _, s := range p.Trace {
				names = append(names, strings.TrimPrefix(s.Action, "Search#0."))
			}
			errs = append(errs, fmt.Errorf("path %d %v: %w", i, names, err))
		}
	}
	return errors.Join(errs...)
}

func (a *fulltextSearchSurfacesAdapter) walk(trace []tracecheck.Step) error {
	for j, s := range trace {
		name := strings.TrimPrefix(s.Action, "Search#0.")
		var err error
		if s.Action == "Init" {
			err = a.Init()
		} else if f, ok := fulltextSearchSurfacesActions["Search"][name]; !ok {
			return fmt.Errorf("step %d: no action %q", j, s.Action)
		} else {
			_, err = f(a, nil)
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", j, name, err)
		}
		if a.gate.off {
			return fmt.Errorf("step %d (%s): the adapter's require says disabled", j, name)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w", j, name, err)
		}
		b, _ := json.Marshal(got)
		var norm map[string]any
		json.Unmarshal(b, &norm)
		for k, v := range s.State {
			f, ok := strings.CutPrefix(k, "Search#0.")
			if !ok {
				continue
			}
			want, _ := json.Marshal(v)
			have, _ := json.Marshal(norm[f])
			if string(want) != string(have) {
				return fmt.Errorf("step %d (%s): %s is %s, the spec says %s (state %s)", j, name, f, have, want, b)
			}
		}
	}
	return nil
}

// TestFulltextSearchSurfaces lets fizzbee-mbt walk the spec at random
// against a real serve (MODEL_COVER=transitions only; see runMBT).
func TestFulltextSearchSurfaces(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newFulltextSearchSurfacesAdapter(t)
	if err := runMBT(t, "fulltext_search_surfaces", a, fulltextSearchSurfacesActions, fulltextSearchSurfacesOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkFulltextSearchSurfacesHistories(t, a)
}

// TestFulltextSearchSurfacesPaths walks every derived path against one
// serve: every settled state, or every link under MODEL_COVER=transitions.
func TestFulltextSearchSurfacesPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newFulltextSearchSurfacesAdapter(t)
	if err := walkFulltextSearchSurfacesPaths(a, envCover()); err != nil {
		t.Fatalf("spec path: %v", err)
	}
	checkFulltextSearchSurfacesHistories(t, a)
}

// The projection is only a check if a transcript the model forbids is
// refused: A's transcript saying its own title's word.
func TestFulltextSearchSurfacesHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(fizzCheck(t, "fulltext_search_surfaces"))
	if err != nil {
		t.Fatal(err)
	}
	meta := history.Entry{Kind: "meta", Data: map[string]any{"cwd": "/w"}}
	in := func(s string) history.Entry { return history.Entry{Kind: "input", Data: map[string]any{"text": s}} }
	out := func(s string) history.Entry { return history.Entry{Kind: "assistant", Data: map[string]any{"text": s}} }
	done := history.Entry{Kind: "done"}
	for name, es := range map[string][]history.Entry{
		"A": {meta, in("good day"), out("fine"), done},
		"B": {meta, in("hello"), out("a gannet dives"), done},
		"E": {meta},
		"Z": {meta, in("morning"), out("an osprey circles"), done},
	} {
		if v := g.Check(fulltextSearchSurfacesHistory(es)); v != nil {
			t.Errorf("%s: %v", name, v)
		}
	}
	if v := g.Check(fulltextSearchSurfacesHistory([]history.Entry{meta, in("hello"), out("the plover nests"), done})); v == nil {
		t.Error("a transcript saying the title's word passed the trace check")
	} else {
		t.Logf("refused as expected: %v", v)
	}
}

// A run that reads the list without its archived rows under Archived's
// Include must fail, or a green TestFulltextSearchSurfacesPaths proves
// nothing. Every link: the wrong read shows on ToggleArchived after
// "osprey" answered, which a walk reaching every state need not take.
func TestFulltextSearchSurfacesCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newFulltextSearchSurfacesAdapter(t)
	a.archivedUnread = true
	err := walkFulltextSearchSurfacesPaths(a, tracecheck.CoverTransitions)
	if err == nil {
		t.Fatal("a run that leaves the archived rows out of an Include passed; the paths are not checking state")
	}
	t.Logf("caught as expected: %v", err)
}
