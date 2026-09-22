package serve

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestRewriteNoteAsksTheNoterWithNeighbours(t *testing.T) {
	f := newAPI(t)
	var got [3]string
	f.api.note = func(ctx context.Context, before, sentence, after string) (RewriteNote, error) {
		got = [3]string{before, sentence, after}
		return RewriteNote{Claim: "retries ignore the cause", Tells: []string{"Specifically"}, Advice: "Keep it; add the retry count."}, nil
	}
	code, body := f.do(t, "POST", "/api/rewrite/note", `{"before":"A.","sentence":"Specifically, B.","after":"C."}`)
	if code != http.StatusOK {
		t.Fatalf("code = %d %v", code, body)
	}
	if got != [3]string{"A.", "Specifically, B.", "C."} {
		t.Fatalf("noter got %q", got)
	}
	if body["claim"] != "retries ignore the cause" || body["advice"] != "Keep it; add the retry count." {
		t.Fatalf("body = %v", body)
	}
	if tells, _ := body["tells"].([]any); len(tells) != 1 || tells[0] != "Specifically" {
		t.Fatalf("tells = %v", body["tells"])
	}
}

func TestRewriteNoteRejectsEmptyAndReportsModelErrors(t *testing.T) {
	f := newAPI(t)
	f.api.note = func(context.Context, string, string, string) (RewriteNote, error) { return RewriteNote{}, errors.New("no key") }
	if code, _ := f.do(t, "POST", "/api/rewrite/note", `{"sentence":"  "}`); code != http.StatusBadRequest {
		t.Fatalf("empty: code = %d", code)
	}
	code, body := f.do(t, "POST", "/api/rewrite/note", `{"sentence":"x"}`)
	if code != http.StatusBadGateway || !strings.Contains(body["error"].(string), "no key") {
		t.Fatalf("model error: %d %v", code, body)
	}
}

func TestParseNoteTakesFencedJSONAndFallsBackToProse(t *testing.T) {
	n := parseNote("```json\n{\"claim\":\"c\",\"tells\":[\"leverage\"],\"advice\":\"cut\"}\n```")
	if n.Claim != "c" || len(n.Tells) != 1 || n.Advice != "cut" {
		t.Fatalf("fenced: %+v", n)
	}
	n = parseNote("{\"claim\":\"only\"}")
	if n.Claim != "only" || n.Tells == nil {
		t.Fatalf("tells must be [] not null: %+v", n)
	}
	n = parseNote("Just cut it.")
	if n.Advice != "Just cut it." || n.Claim != "" || n.Tells == nil {
		t.Fatalf("prose: %+v", n)
	}
}

func TestNotionPageID(t *testing.T) {
	cases := map[string]string{
		"https://www.notion.so/acme/Payments-retry-design-1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d":                   "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d",
		"https://notion.so/1a2b3c4d-5e6f-7a8b-9c0d-1e2f3a4b5c6d":                                                "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d",
		"https://acme.notion.site/Design-1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d?pvs=4":                               "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d",
		"https://www.notion.so/acme/Parent-0000000000000000000000000000abcd?p=1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d": "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d",
		"https://docs.google.com/document/d/abc":                                                                "",
		"not a url":                                                                                             "",
		"https://www.notion.so/acme/":                                                                           "",
	}
	for in, want := range cases {
		got, ok := notionPageID(in)
		if got != want || ok != (want != "") {
			t.Errorf("%s: got %q %v, want %q", in, got, ok, want)
		}
	}
}

// A fake Notion: one page with a title, a heading, two paragraphs with
// annotations, a bullet with a nested bullet, a code block, and one
// child_page that must not be descended into. Paged in two.
func fakeNotion(t *testing.T) *httptest.Server {
	t.Helper()
	const page = "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d"
	mux := http.NewServeMux()
	mux.HandleFunc("GET /v1/pages/"+page, func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "Bearer secret" || r.Header.Get("Notion-Version") == "" {
			http.Error(w, `{"message":"unauthorized"}`, 401)
			return
		}
		w.Write([]byte(`{"properties":{"Name":{"type":"title","title":[{"plain_text":"Retry design"}]},"Owner":{"type":"people"}}}`))
	})
	mux.HandleFunc("GET /v1/blocks/"+page+"/children", func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Query().Get("start_cursor") == "" {
			w.Write([]byte(`{"has_more":true,"next_cursor":"c2","results":[
			{"id":"h","type":"heading_1","heading_1":{"rich_text":[{"plain_text":"Problem"}]}},
			{"id":"p1","type":"paragraph","paragraph":{"rich_text":[{"plain_text":"Retries run on a "},{"plain_text":"fixed interval","annotations":{"bold":true}},{"plain_text":". See "},{"plain_text":"the RFC","href":"https://x/rfc"},{"plain_text":"."}]}}]}`))
			return
		}
		w.Write([]byte(`{"has_more":false,"results":[
		{"id":"b1","type":"bulleted_list_item","has_children":true,"bulleted_list_item":{"rich_text":[{"plain_text":"outer"}]}},
		{"id":"c","type":"code","code":{"language":"go","rich_text":[{"plain_text":"x := 1"}]}},
		{"id":"cp","type":"child_page","has_children":true,"child_page":{"title":"Sub"}},
		{"id":"p2","type":"paragraph","paragraph":{"rich_text":[]}}]}`))
	})
	mux.HandleFunc("GET /v1/blocks/b1/children", func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(`{"has_more":false,"results":[{"id":"b2","type":"bulleted_list_item","bulleted_list_item":{"rich_text":[{"plain_text":"inner"}]}}]}`))
	})
	mux.HandleFunc("GET /v1/blocks/cp/children", func(w http.ResponseWriter, r *http.Request) {
		t.Error("descended into a child page")
	})
	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)
	return srv
}

func TestRewriteFetchReadsANotionPageAsMarkdown(t *testing.T) {
	f := newAPI(t)
	f.api.notionBase = fakeNotion(t).URL
	f.api.getenv = func(k string) string {
		if k == "NOTION_TOKEN" {
			return "secret"
		}
		return ""
	}
	code, body := f.do(t, "POST", "/api/rewrite/fetch", `{"url":"https://www.notion.so/acme/Retry-1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d"}`)
	if code != http.StatusOK {
		t.Fatalf("code = %d %v", code, body)
	}
	if body["title"] != "Retry design" {
		t.Fatalf("title = %v", body["title"])
	}
	want := "# Problem\n\nRetries run on a **fixed interval**. See [the RFC](https://x/rfc).\n\n- outer\n  - inner\n```go\nx := 1\n```\n"
	if body["text"] != want {
		t.Fatalf("text =\n%q\nwant\n%q", body["text"], want)
	}
}

func TestRewriteFetchSaysWhatIsMissing(t *testing.T) {
	f := newAPI(t)
	f.api.getenv = func(string) string { return "" }
	code, body := f.do(t, "POST", "/api/rewrite/fetch", `{"url":"https://docs.google.com/document/d/abc"}`)
	if code != http.StatusBadRequest || !strings.Contains(body["error"].(string), "paste the text") {
		t.Fatalf("non-notion: %d %v", code, body)
	}
	code, body = f.do(t, "POST", "/api/rewrite/fetch", `{"url":"https://notion.so/1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d"}`)
	if code != http.StatusFailedDependency || !strings.Contains(body["error"].(string), "NOTION_TOKEN") {
		t.Fatalf("no token: %d %v", code, body)
	}
	f.api.notionBase = fakeNotion(t).URL
	f.api.getenv = func(string) string { return "wrong" }
	code, body = f.do(t, "POST", "/api/rewrite/fetch", `{"url":"https://notion.so/1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d"}`)
	if code != http.StatusBadGateway || !strings.Contains(body["error"].(string), "unauthorized") {
		t.Fatalf("bad token: %d %v", code, body)
	}
}

// The real path: the home config's llm rows mount on their own, and
// llm-echo answers the prompt back, which parseNote turns into advice.
func TestRewriteModelMountsTheHomeConfig(t *testing.T) {
	f := newAPI(t)
	if err := os.WriteFile(filepath.Join(f.home, ".bough", "bough.yml"), []byte("- id: llm\n  plugin: llm-echo\n- id: tools\n  plugin: tools-basic\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	f.api.home = f.home
	n, err := f.api.modelNote(context.Background(), "", "Furthermore, it is robust.", "")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(n.Advice, "Furthermore, it is robust.") {
		t.Fatalf("advice = %q", n.Advice)
	}
}
