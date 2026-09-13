package serve

import (
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// seedWiki writes a one-page wiki under the fixture's home.
func seedWiki(t *testing.T, f *apiFixture) {
	t.Helper()
	dir := filepath.Join(f.home, ".bough", "wiki", "topics", "bough")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	page := "# Ports\n\nWho binds first wins.\n\n## Facts\n\n- The artifact port is 7683.\n"
	if err := os.WriteFile(filepath.Join(dir, "ports.md"), []byte(page), 0o644); err != nil {
		t.Fatal(err)
	}
	index := "# Wiki index\n\n## bough\n\n- [Ports](topics/bough/ports.md) — who binds first\n"
	if err := os.WriteFile(filepath.Join(f.home, ".bough", "wiki", "index.md"), []byte(index), 0o644); err != nil {
		t.Fatal(err)
	}
}

func TestWikiIndexAndPage(t *testing.T) {
	f := newAPI(t)
	f.api.home = f.home
	code, body := f.do(t, "GET", "/api/wiki", "")
	if code != http.StatusOK || body["exists"] != false {
		t.Fatalf("no wiki: %d %v", code, body)
	}
	seedWiki(t, f)
	_, body = f.do(t, "GET", "/api/wiki", "")
	topics, _ := body["topics"].([]any)
	if body["exists"] != true || len(topics) != 1 {
		t.Fatalf("index = %v", body)
	}
	code, body = f.do(t, "GET", "/api/wiki/page?path=topics/bough/ports.md", "")
	if code != http.StatusOK || body["title"] != "Ports" {
		t.Fatalf("page: %d %v", code, body)
	}
	counts, _ := body["counts"].(map[string]any)
	if counts["uncited"] != float64(1) {
		t.Fatalf("counts = %v", counts)
	}
	_, body = f.do(t, "GET", "/api/wiki/review", "")
	if flags, _ := body["flags"].([]any); len(flags) != 1 {
		t.Fatalf("review = %v", body)
	}
}

func TestWikiPagePathGuard(t *testing.T) {
	f := newAPI(t)
	f.api.home = f.home
	seedWiki(t, f)
	for _, p := range []string{"index.md", "../../history/x.md", "/etc/hosts"} {
		if code, _ := f.do(t, "GET", "/api/wiki/page?path="+p, ""); code != http.StatusBadRequest {
			t.Errorf("GET page %q = %d, want 400", p, code)
		}
	}
	code, _ := f.do(t, "PUT", "/api/wiki/page", `{"path":"../index.md","body":"x"}`)
	if code != http.StatusBadRequest {
		t.Fatalf("PUT outside topics = %d", code)
	}
	if code, _ := f.do(t, "GET", "/api/wiki/page?path=topics/bough/none.md", ""); code != http.StatusNotFound {
		t.Fatalf("missing page = %d", code)
	}
}

func TestWikiClaimEditAndStale(t *testing.T) {
	f := newAPI(t)
	f.api.home = f.home
	seedWiki(t, f)
	stale := `{"path":"topics/bough/ports.md","line":7,"end":7,"raw":"- something else","action":"drop"}`
	if code, _ := f.do(t, "POST", "/api/wiki/claim", stale); code != http.StatusConflict {
		t.Fatalf("stale edit = %d, want 409", code)
	}
	if code, _ := f.do(t, "POST", "/api/wiki/claim", `{"path":"topics/bough/ports.md","line":7,"end":7,"raw":"- The artifact port is 7683.","action":"rm"}`); code != http.StatusBadRequest {
		t.Fatalf("unknown action = %d", code)
	}
	ok := `{"path":"topics/bough/ports.md","line":7,"end":7,"raw":"- The artifact port is 7683.","action":"inference"}`
	if code, body := f.do(t, "POST", "/api/wiki/claim", ok); code != http.StatusOK {
		t.Fatalf("edit = %d %v", code, body)
	}
	b, _ := os.ReadFile(filepath.Join(f.home, ".bough", "wiki", "topics", "bough", "ports.md"))
	if !strings.Contains(string(b), "- *Inference:* The artifact port is 7683.") {
		t.Fatalf("page after edit = %q", b)
	}
}

func TestWikiIngestStartsARun(t *testing.T) {
	f := newAPI(t)
	f.api.home = f.home
	var got []string
	f.api.ingest = func(only string) error { got = append(got, only); return nil }
	if code, _ := f.do(t, "POST", "/api/wiki/ingest", ""); code != http.StatusOK {
		t.Fatalf("ingest = %d", code)
	}
	if code, _ := f.do(t, "POST", "/api/wiki/ingest", `{"session":"s9"}`); code != http.StatusOK {
		t.Fatalf("ingest one = %d", code)
	}
	if len(got) != 2 || got[0] != "" || got[1] != "s9" {
		t.Fatalf("ingest calls = %q", got)
	}
}

func TestWikiSearchAndCheck(t *testing.T) {
	f := newAPI(t)
	f.api.home = f.home
	seedWiki(t, f)
	_, body := f.do(t, "GET", "/api/wiki/search?q=7683", "")
	if hits, _ := body["hits"].([]any); len(hits) != 1 {
		t.Fatalf("search = %v", body)
	}
	code, body := f.do(t, "POST", "/api/wiki/check", "")
	if probs, _ := body["problems"].([]any); code != http.StatusOK || probs == nil {
		t.Fatalf("check = %d %v", code, body)
	}
}
