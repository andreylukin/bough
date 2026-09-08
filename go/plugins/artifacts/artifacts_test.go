package artifacts

import (
	"io"
	"net/http"
	"path/filepath"
	"strings"
	"testing"

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
	return &Store{root: t.TempDir(), session: "s1", web: web.New("127.0.0.1:0"), opened: map[string]bool{}}
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
		`blocks[0]: unknown type "tabel" (types: callout, chart, code, grid, heading, kv, list, section, stats, table, text, timeline)`,
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
