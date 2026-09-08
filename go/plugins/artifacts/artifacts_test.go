package artifacts

import (
	"context"
	"io"
	"net/http"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/web"
)

func sample() map[string]any {
	return map[string]any{
		"title":    "DB comparison",
		"subtitle": "three candidates, one weekend",
		"blocks": []any{
			"Postgres **wins** on operations; see the table.",
			map[string]any{"type": "stats", "items": []any{
				map[string]any{"label": "p99 read", "value": "4 ms", "delta": "-30%", "tone": "good"},
			}},
			map[string]any{"type": "table", "columns": []any{"db", "qps"}, "rows": []any{[]any{"pg", 1200.0}, []any{"sqlite", 900.0}}},
			map[string]any{"type": "chart", "kind": "bar", "x": []any{"pg", "sqlite"}, "series": []any{map[string]any{"name": "qps", "values": []any{1200.0, 900.0}}}},
			map[string]any{"type": "section", "title": "Notes", "blocks": []any{"nested shorthand"}},
		},
	}
}

func newStore(t *testing.T) *Store {
	t.Helper()
	return &Store{root: t.TempDir(), session: "s1", web: web.New("127.0.0.1:0"), opened: map[string]bool{}, seen: map[string]int{}}
}

// A good spec is stored, served as a page and as raw JSON, and listed.
func TestPublishServesPageAndSpec(t *testing.T) {
	s := newStore(t)
	s.web.Handle("/artifacts/", s)
	defer s.web.Unhandle("/artifacts/")
	opened := ""
	s.open = func(u string) error { opened = u; return nil }
	url, err := s.Publish("DB Comparison", sample())
	if err != nil {
		t.Fatal(err)
	}
	if want := s.web.URL() + "/artifacts/s1/db-comparison"; url != want || opened != want {
		t.Fatalf("url %q opened %q", url, opened)
	}
	get := func(u string) (int, string) {
		r, err := http.Get(u)
		if err != nil {
			t.Fatal(err)
		}
		defer r.Body.Close()
		b, _ := io.ReadAll(r.Body)
		return r.StatusCode, string(b)
	}
	if code, page := get(url); code != 200 || !strings.Contains(page, `"DB comparison"`) || !strings.Contains(page, "const SPEC = {") {
		t.Fatalf("page %d: %.300s", code, page)
	}
	if _, raw := get(url + ".json"); !strings.Contains(raw, `"type": "text"`) || !strings.Contains(raw, `"md": "nested shorthand"`) {
		t.Fatalf("raw spec should have expanded shorthand: %s", raw)
	}
	if _, js := get(s.web.URL() + "/artifacts/_lib/echarts.min.js"); !strings.Contains(js, "echarts") {
		t.Fatal("lib not served")
	}
	if _, idx := get(s.web.URL() + "/artifacts/"); !strings.Contains(idx, "/artifacts/s1/db-comparison") {
		t.Fatalf("index: %s", idx)
	}
	if code, _ := get(s.web.URL() + "/artifacts/s1/nope"); code != 404 {
		t.Fatalf("missing page = %d", code)
	}
	if code, _ := get(s.web.URL() + "/artifacts/../../etc/passwd"); code != 404 {
		t.Fatalf("traversal = %d", code)
	}
	// Republishing keeps the URL and does not reopen the browser.
	opened = ""
	if again, _ := s.Publish("db-comparison", sample()); again != url || opened != "" {
		t.Fatalf("republish: %q opened %q", again, opened)
	}
	if l := s.List(); !strings.Contains(l, "DB comparison  "+url) {
		t.Fatalf("list: %s", l)
	}
	if _, err := s.Publish("../x", sample()); err == nil {
		t.Fatal("bad name accepted")
	}
	if fi, err := filepath.Glob(filepath.Join(s.root, "s1", "*.json")); err != nil || len(fi) != 1 {
		t.Fatalf("files: %v %v", fi, err)
	}
}

// A bad spec comes back as every problem at once, addressed by block.
func TestNormalizeReportsEveryProblem(t *testing.T) {
	_, err := Normalize(map[string]any{
		"blocks": []any{
			map[string]any{"type": "tabel"},
			map[string]any{"type": "table", "columns": []any{"a", "b"}, "rows": []any{[]any{1.0}}, "colour": "red"},
			map[string]any{"type": "chart", "kind": "donut", "series": []any{map[string]any{"values": []any{1.0, 2.0}}}},
			map[string]any{"type": "callout", "text": "x", "tone": "loud"},
			map[string]any{"type": "grid", "columns": 7.0, "blocks": []any{map[string]any{"type": "stats", "items": []any{map[string]any{"label": "a"}}}}},
			42.0,
		},
	})
	if err == nil {
		t.Fatal("accepted")
	}
	for _, want := range []string{
		"title: required",
		`blocks[0]: unknown type "tabel" (types: callout, chart, checklist, code, decision, diagram, diff, filter, form, grid, heading, image, kv, list, section, stats, table, tabs, text, timeline)`,
		"blocks[1] (table): unknown field colour",
		"blocks[1] (table): rows[0] has 1 cells, 2 columns",
		"blocks[2] (chart): kind must be one of bar|line|area|pie|scatter",
		"blocks[2] (chart): x is required",
		"blocks[3] (callout): tone must be one of info|good|warn|bad",
		"blocks[4] (grid): columns must be 2, 3 or 4",
		"blocks[4] (grid).blocks[0] (stats): items[0]: missing value",
		"blocks[5]: a block is an object with a type, or a string",
	} {
		if !strings.Contains(err.Error(), want) {
			t.Errorf("missing %q in:\n%s", want, err)
		}
	}
	if _, err := Normalize("nope"); err == nil || !strings.Contains(err.Error(), "must be an object") {
		t.Fatalf("string spec: %v", err)
	}
}

// The viewer inlines the spec without letting it close the script tag.
func TestRenderEscapesScriptClose(t *testing.T) {
	page := render([]byte(`{"title":"</script><b>x"}`))
	if strings.Contains(page, `"</script>`) || !strings.Contains(page, `<\/script>`) {
		t.Fatalf("page: %.200s", page[strings.Index(page, "const SPEC"):])
	}
}

// Lenient shapes are accepted and canonicalised: object rows, a y:
// shorthand, columns derived from the rows.
func TestLenientShapes(t *testing.T) {
	page, err := Normalize(map[string]any{"title": "t", "blocks": []any{
		map[string]any{"type": "table", "rows": []any{map[string]any{"db": "pg", "qps": 1.0}, map[string]any{"db": "lite", "qps": 2.0}}},
		map[string]any{"type": "chart", "kind": "bar", "x": []any{"a", "b"}, "y": []any{1.0, 2.0}},
	}})
	if err != nil {
		t.Fatal(err)
	}
	blocks := page["blocks"].([]any)
	tbl := blocks[0].(map[string]any)
	if cols := tbl["columns"].([]any); len(cols) != 2 || cols[0] != "db" || tbl["rows"].([]any)[1].([]any)[1] != 2.0 {
		t.Fatalf("table: %v", tbl)
	}
	if series := blocks[1].(map[string]any)["series"].([]any); len(series) != 1 || series[0].(map[string]any)["values"].([]any)[1] != 2.0 {
		t.Fatalf("chart: %v", blocks[1])
	}
}

// Bound blocks are checked against the data set's columns.
func TestDataBinding(t *testing.T) {
	spec := func(blocks ...any) map[string]any {
		return map[string]any{"title": "t", "data": map[string]any{"sales": []any{
			map[string]any{"region": "eu", "q": "Q1", "amount": 10.0},
			map[string]any{"region": "us", "q": "Q1", "amount": 12.0},
		}}, "blocks": blocks}
	}
	if _, err := Normalize(spec(
		map[string]any{"type": "filter", "from": "sales", "by": []any{"region"}},
		map[string]any{"type": "table", "from": "sales"},
		map[string]any{"type": "chart", "kind": "bar", "from": "sales", "x": "q", "y": "amount"},
	)); err != nil {
		t.Fatal(err)
	}
	_, err := Normalize(spec(
		map[string]any{"type": "table", "from": "sale"},
		map[string]any{"type": "chart", "kind": "bar", "from": "sales", "x": "quarter", "series": []any{"amount"}},
		map[string]any{"type": "decision", "id": "go", "question": "ship?", "options": []any{"yes"}},
		map[string]any{"type": "decision", "id": "go", "question": "again?", "options": []any{"a", "b"}},
		map[string]any{"type": "form", "id": "f", "fields": []any{map[string]any{"name": "env", "type": "select"}}},
	))
	if err == nil {
		t.Fatal("accepted")
	}
	for _, want := range []string{
		`blocks[0] (table): from: no data set "sale" (data has: sales)`,
		`blocks[1] (chart): x: "quarter" is not a column of sales (columns: amount, q, region)`,
		"blocks[2] (decision): options needs at least 2 entries",
		`blocks[3] (decision): id "go" is used twice`,
		"blocks[4] (form): fields[0]: a select needs options",
	} {
		if !strings.Contains(err.Error(), want) {
			t.Errorf("missing %q in:\n%s", want, err)
		}
	}
}

// A patch changes the stored page in place, is validated whole, and
// bumps the version the open page reloads on.
func TestPatch(t *testing.T) {
	s := newStore(t)
	if _, err := s.Publish("p", sample()); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Patch("p", []any{
		map[string]any{"op": "set", "path": "blocks[2].rows", "value": []any{[]any{"pg", 1.0}}},
		map[string]any{"op": "append", "path": "blocks", "value": map[string]any{"type": "callout", "text": "new"}},
		map[string]any{"op": "remove", "path": "blocks[0]"},
		map[string]any{"op": "set", "path": "subtitle", "value": "v2"},
	}); err != nil {
		t.Fatal(err)
	}
	page, _ := s.load("p")
	blocks := page["blocks"].([]any)
	if page["version"] != 2.0 || page["subtitle"] != "v2" || len(blocks) != 5 || blocks[len(blocks)-1].(map[string]any)["text"] != "new" || blocks[0].(map[string]any)["type"] != "stats" {
		t.Fatalf("page: %v", page)
	}
	// A bad patch is refused and leaves the page as it was.
	_, err := s.Patch("p", []any{map[string]any{"op": "set", "path": "blocks[1].rows", "value": []any{[]any{"only-one-cell"}}}})
	if err == nil || !strings.Contains(err.Error(), "rows[0] has 1 cells, 2 columns") {
		t.Fatalf("bad patch: %v", err)
	}
	if again, _ := s.load("p"); again["version"] != 2.0 {
		t.Fatal("bad patch changed the page")
	}
	if _, err := s.Patch("p", []any{map[string]any{"op": "set", "path": "blocks[9]", "value": 1.0}}); err == nil || !strings.Contains(err.Error(), "out of range") {
		t.Fatalf("range: %v", err)
	}
	if _, err := s.Patch("nope", []any{}); err == nil || !strings.Contains(err.Error(), "not published") {
		t.Fatalf("unknown: %v", err)
	}
}

// Answers posted by the page are stored, read back by the tool, and
// become notices for the agent; the events stream reports a republish.
func TestAnswersRoundTrip(t *testing.T) {
	s := newStore(t)
	s.web.Handle("/artifacts/", s)
	defer s.web.Unhandle("/artifacts/")
	var notices []string
	s.notify = func(t string) { notices = append(notices, t) }
	url, err := s.Publish("plan", map[string]any{"title": "Plan", "blocks": []any{
		map[string]any{"type": "decision", "id": "go", "question": "ship?", "options": []any{"yes", "no"}},
	}})
	if err != nil {
		t.Fatal(err)
	}
	if got, _ := s.Answers("plan"); got != "no answers yet on plan" {
		t.Fatalf("empty: %q", got)
	}
	postJSON := func(body string) int {
		r, err := http.Post(url+"/answers", "application/json", strings.NewReader(body))
		if err != nil {
			t.Fatal(err)
		}
		r.Body.Close()
		return r.StatusCode
	}
	if code := postJSON(`{"kind":"decision","id":"go","value":{"choice":"yes","reason":"tests green"}}`); code != 200 {
		t.Fatalf("post = %d", code)
	}
	if code := postJSON(`{"kind":"note","value":"also bump the version"}`); code != 200 {
		t.Fatalf("note = %d", code)
	}
	if code := postJSON(`{"kind":"note","value":"  "}`); code != 400 {
		t.Fatalf("empty note = %d", code)
	}
	r, _ := http.Get(url + "/answers")
	b, _ := io.ReadAll(r.Body)
	r.Body.Close()
	if !strings.Contains(string(b), `"choice":"yes"`) || !strings.Contains(string(b), "bump the version") {
		t.Fatalf("answers: %s", b)
	}
	// The watcher notices the two entries: publishing registered the
	// page, so nothing posted since counts as history.
	stop := make(chan struct{})
	done := make(chan struct{})
	go func() { s.watchAnswers(stop); close(done) }()
	deadline := time.Now().Add(5 * time.Second)
	for len(notices) < 2 && time.Now().Before(deadline) {
		time.Sleep(50 * time.Millisecond)
	}
	close(stop)
	<-done
	if len(notices) != 2 || !strings.Contains(notices[0], `[artifact plan] decision go = {"choice":"yes","reason":"tests green"}`) || !strings.Contains(notices[1], "the user wrote: also bump") {
		t.Fatalf("notices: %v", notices)
	}
	if got, _ := s.Answers("plan"); !strings.Contains(got, `"choice": "yes"`) {
		t.Fatalf("tool: %s", got)
	}
	// Events: a republish shows up as an update line.
	req, _ := http.NewRequest("GET", url+"/events", nil)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	resp, err := http.DefaultClient.Do(req.WithContext(ctx))
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	time.Sleep(1100 * time.Millisecond) // past the first tick, mtime resolution
	if _, err := s.Publish("plan", map[string]any{"title": "Plan v2", "blocks": []any{"x"}}); err != nil {
		t.Fatal(err)
	}
	buf := make([]byte, 256)
	var got string
	for !strings.Contains(got, "event: update") {
		n, err := resp.Body.Read(buf)
		got += string(buf[:n])
		if err != nil {
			break
		}
	}
	if !strings.Contains(got, "event: update") {
		t.Fatalf("events: %q", got)
	}
}
