package artifacts

import (
	"context"
	"io"
	"net/http"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/web"
)

const sample = `root = Card([head, tbl, ask])
head = CardHeader("DB comparison", "three candidates")
tbl = Table([Col("engine", ["SQLite", "DuckDB"]), Col("p99 ms", [1.8, 14.2], "number")])
ask = Buttons([Button("Keep SQLite"), Button("Try DuckDB")])
`

func newStore(t *testing.T) *Store {
	t.Helper()
	return &Store{root: t.TempDir(), session: "s1", web: web.New("127.0.0.1:0"), opened: map[string]bool{}, seen: map[string]int{}, errSeen: map[string]string{}}
}

func get(t *testing.T, u string) (int, string) {
	t.Helper()
	r, err := http.Get(u)
	if err != nil {
		t.Fatal(err)
	}
	defer r.Body.Close()
	b, _ := io.ReadAll(r.Body)
	return r.StatusCode, string(b)
}

// A program is stored, served as a page hosting the bundle and as
// its source, and listed.
func TestPublishServesPageAndSource(t *testing.T) {
	s := newStore(t)
	s.web.Handle("/artifacts/", s)
	defer s.web.Unhandle("/artifacts/")
	opened := ""
	s.open = func(u string) error { opened = u; return nil }
	url, err := s.Publish("DB Comparison", sample)
	if err != nil {
		t.Fatal(err)
	}
	if want := s.web.URL() + "/artifacts/s1/db-comparison"; url != want || opened != want {
		t.Fatalf("url %q opened %q", url, opened)
	}
	code, page := get(t, url)
	if code != 200 || !strings.Contains(page, `const PAGE = {`) || !strings.Contains(page, `openui-bundle.min.js`) || !strings.Contains(page, `CardHeader(\"DB comparison\"`) {
		t.Fatalf("page %d: %.300s", code, page)
	}
	if _, src := get(t, url+".ui"); src != sample {
		t.Fatalf("source: %q", src)
	}
	if _, js := get(t, s.web.URL()+"/artifacts/_lib/openui-bundle.min.js"); !strings.Contains(js, "__OpenUI") {
		t.Fatal("bundle not served")
	}
	if _, css := get(t, s.web.URL()+"/artifacts/_lib/openui-styles.css"); !strings.Contains(css, "openui") {
		t.Fatal("styles not served")
	}
	if _, idx := get(t, s.web.URL()+"/artifacts/"); !strings.Contains(idx, "/artifacts/s1/db-comparison") {
		t.Fatalf("index: %s", idx)
	}
	if code, _ := get(t, s.web.URL()+"/artifacts/s1/nope"); code != 404 {
		t.Fatalf("missing page = %d", code)
	}
	if code, _ := get(t, s.web.URL()+"/artifacts/../../etc/passwd"); code != 404 {
		t.Fatalf("traversal = %d", code)
	}
	// Republishing keeps the URL, bumps the version, and does not
	// reopen the browser.
	opened = ""
	if again, _ := s.Publish("db-comparison", sample); again != url || opened != "" {
		t.Fatalf("republish: %q opened %q", again, opened)
	}
	if m := readMeta(metaPath(s.codePath("db-comparison"))); m.Version != 2 {
		t.Fatalf("version = %d", m.Version)
	}
	if l := s.List(); !strings.Contains(l, "db-comparison (v2)  "+url) {
		t.Fatalf("list: %s", l)
	}
	if _, err := s.Publish("../x", sample); err == nil {
		t.Fatal("bad name accepted")
	}
	if g, _ := s.Guide(); !strings.Contains(g, "## Component Signatures") || strings.Contains(g, "ENTIRE response") {
		t.Fatalf("guide: %.200s", g)
	}
}

// The publish check catches what the browser would refuse anyway,
// with the fix in the message.
func TestCheckRefusesTheObvious(t *testing.T) {
	for _, c := range []struct{ code, want string }{
		{"", "no statements"},
		{"just prose about databases", "no statements"},
		{"head = CardHeader(\"x\")\n", "no root"},
		{"root = Card([a])\na = TextContent(\"1\")\na = TextContent(\"2\")\n", `defines "a" twice`},
		{"```\nroot = Card([])\n```", "markdown fence"},
	} {
		err := check(c.code)
		if err == nil || !strings.Contains(err.Error(), c.want) {
			t.Errorf("%q: %v (want %q)", c.code, err, c.want)
		}
	}
	if err := check(sample); err != nil {
		t.Fatal(err)
	}
}

// A patch replaces same-named statements, adds new ones, removes with
// null, and keeps everything else verbatim.
func TestMergeIsStatementByStatement(t *testing.T) {
	patch := "tbl = Table([Col(\"engine\", [\"SQLite\"])])\nnote = Callout(\"info\", \"Chosen\", \"SQLite stays\")\nroot = Card([head, tbl, note])\nask = null\n"
	got := merge(sample, patch)
	want := "root = Card([head, tbl, note])\nhead = CardHeader(\"DB comparison\", \"three candidates\")\ntbl = Table([Col(\"engine\", [\"SQLite\"])])\nnote = Callout(\"info\", \"Chosen\", \"SQLite stays\")\n"
	if got != want {
		t.Fatalf("merge:\n%s\nwant:\n%s", got, want)
	}
	// Multi-line statements travel whole.
	multi := "root = Card([a])\na = Table([\n  Col(\"x\", [1])\n])\n"
	if out := merge(multi, "b = TextContent(\"hi\")\nroot = Card([a, b])\n"); !strings.Contains(out, "a = Table([\n  Col(\"x\", [1])\n])") || !strings.HasPrefix(out, "root = Card([a, b])") {
		t.Fatalf("multi: %q", out)
	}
	s := newStore(t)
	if _, err := s.Publish("p", sample); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Patch("p", "root = null\n"); err == nil || !strings.Contains(err.Error(), "no root") {
		t.Fatalf("removing root: %v", err)
	}
	if _, err := s.Patch("p", "   \n"); err == nil || !strings.Contains(err.Error(), "no statements") {
		t.Fatalf("empty patch: %v", err)
	}
	if _, err := s.Patch("p", patch); err != nil {
		t.Fatal(err)
	}
	b, _ := os.ReadFile(s.codePath("p"))
	if string(b) != want {
		t.Fatalf("stored: %s", b)
	}
	if m := readMeta(metaPath(s.codePath("p"))); m.Version != 2 {
		t.Fatalf("version = %d", m.Version)
	}
	if _, err := s.Patch("nope", "x = 1"); err == nil || !strings.Contains(err.Error(), "not published") {
		t.Fatalf("unknown: %v", err)
	}
}

// Actions and notes posted by the page are stored, read back by the
// tool, and become notices; the renderer's errors become a notice
// once per report; the events stream reports a republish.
func TestPageTalksBack(t *testing.T) {
	s := newStore(t)
	s.web.Handle("/artifacts/", s)
	defer s.web.Unhandle("/artifacts/")
	var notices []string
	s.notify = func(t string) { notices = append(notices, t) }
	url, err := s.Publish("plan", sample)
	if err != nil {
		t.Fatal(err)
	}
	if got, _ := s.Answers("plan"); got != "no answers yet on plan" {
		t.Fatalf("empty: %q", got)
	}
	postJSON := func(path, body string) int {
		r, err := http.Post(url+path, "application/json", strings.NewReader(body))
		if err != nil {
			t.Fatal(err)
		}
		r.Body.Close()
		return r.StatusCode
	}
	if code := postJSON("/answers", `{"kind":"state","state":{"window":"tomorrow"}}`); code != 200 {
		t.Fatalf("state = %d", code)
	}
	if code := postJSON("/answers", `{"kind":"action","value":{"type":"continue_conversation","message":"Keep SQLite","formState":{"window":"tomorrow"}},"state":{"window":"tomorrow"}}`); code != 200 {
		t.Fatalf("action = %d", code)
	}
	if code := postJSON("/answers", `{"kind":"note","value":"also bump the version"}`); code != 200 {
		t.Fatalf("note = %d", code)
	}
	if code := postJSON("/answers", `{"kind":"note","value":"  "}`); code != 400 {
		t.Fatalf("empty note = %d", code)
	}
	if code := postJSON("/errors", `{"version":1,"errors":[{"source":"parser","code":"unknown-component","statementId":"tbl","message":"Unknown component \"Tabel\"","hint":"Available components: Table, Col"}]}`); code != 200 {
		t.Fatalf("errors = %d", code)
	}
	s.sweep()
	s.sweep() // the same report again is not news
	if len(notices) != 3 {
		t.Fatalf("notices: %q", notices)
	}
	if !strings.Contains(notices[0], `[artifact plan] the user pressed "Keep SQLite" with {"window":"tomorrow"}`) ||
		!strings.Contains(notices[1], "the user wrote: also bump") ||
		!strings.Contains(notices[2], "[artifact plan] the page has 1 error(s) (version 1)") || !strings.Contains(notices[2], `tbl: Unknown component "Tabel" — Available components: Table, Col`) {
		t.Fatalf("notices: %q", notices)
	}
	// A clean report after a fix says nothing; a new complaint does.
	postJSON("/errors", `{"version":2,"errors":[]}`)
	s.sweep()
	postJSON("/errors", `{"version":2,"errors":[{"message":"still wrong"}]}`)
	s.sweep()
	if len(notices) != 4 || !strings.Contains(notices[3], "still wrong") {
		t.Fatalf("notices: %q", notices)
	}
	if got, _ := s.Answers("plan"); !strings.Contains(got, `"window": "tomorrow"`) || !strings.Contains(got, `"message": "Keep SQLite"`) || !strings.Contains(got, "bump the version") {
		t.Fatalf("tool: %s", got)
	}
	_, es := get(t, url+"/errors")
	if !strings.Contains(es, "still wrong") {
		t.Fatalf("errors GET: %s", es)
	}
	// Events: a patch shows up as an update line.
	req, _ := http.NewRequest("GET", url+"/events", nil)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	resp, err := http.DefaultClient.Do(req.WithContext(ctx))
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	time.Sleep(1100 * time.Millisecond)
	if _, err := s.Patch("plan", "head = CardHeader(\"v2\")\n"); err != nil {
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

// The viewer inlines the program without letting it close the script tag.
func TestRenderEscapesScriptClose(t *testing.T) {
	page := render("x", "root = Card([t])\nt = TextContent(\"</script><b>x\")\n", meta{Version: 1})
	if strings.Contains(page, "</script><b>") {
		t.Fatalf("page: %.200s", page[strings.Index(page, "const PAGE"):])
	}
}

// A name that climbs out of the session directory is refused by every
// tool, and no URL shape reaches a file outside the store: unknown
// pages, sources, sub-routes and encoded traversal are all 404.
func TestNamesAndRoutesStayInsideTheStore(t *testing.T) {
	s := newStore(t)
	s.web.Handle("/artifacts/", s)
	defer s.web.Unhandle("/artifacts/")
	if err := os.WriteFile(s.root+"/secret.ui", []byte(sample), 0o644); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"../secret", "..", "a/../../secret", `..\secret`, ".hidden", ""} {
		if _, err := s.Publish(name, sample); err == nil {
			t.Errorf("Publish(%q) accepted", name)
		}
		if _, err := s.Patch(name, "x = 1"); err == nil {
			t.Errorf("Patch(%q) accepted", name)
		}
		if _, err := s.Answers(name); err == nil {
			t.Errorf("Answers(%q) accepted", name)
		}
	}
	if _, err := s.Publish("p", sample); err != nil {
		t.Fatal(err)
	}
	base := s.web.URL() + "/artifacts/"
	for _, p := range []string{
		"s1/nope", "s1/nope.ui", "s1/nope/answers", "s2/p", "s1/p/bogus",
		"s1/..%2fsecret", "..%2fsecret.ui", "s1/%2e%2e/secret.ui",
	} {
		if code, body := get(t, base+p); code != 404 {
			t.Errorf("GET %s = %d %.80q", p, code, body)
		}
	}
	if code, _ := get(t, base+"s1/p.ui"); code != 200 {
		t.Errorf("the page itself = %d", code)
	}
}
