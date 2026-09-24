package serve

import (
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
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

func TestMeReadsTheBriefAndRefreshStartsOne(t *testing.T) {
	f := newAPI(t)
	f.api.home = f.home
	_, body := f.do(t, "GET", "/api/me", "")
	if body["hasProfile"] != false || body["page"] != nil {
		t.Fatalf("empty me = %v", body)
	}
	me := filepath.Join(f.home, ".bough", "wiki", "topics", "me")
	if err := os.MkdirAll(filepath.Join(me, "briefs"), 0o755); err != nil {
		t.Fatal(err)
	}
	for name, text := range map[string]string{
		"profile.md":           "# Me\n\nSRE.\n",
		"briefs/2026-09-18.md": "# Brief, Fri Sep 18\n\nOld day.\n",
		"briefs/2026-09-21.md": "# Brief, Mon Sep 21\n\nWaiting on a review. `gh:asi/uni-nes#7801`\n",
		"signals.json":         `{"asOf":"2026-09-21T14:00:00Z","items":[{"kind":"needs-you","source":"gh","title":"Review"}],"sources":[]}`,
	} {
		if err := os.WriteFile(filepath.Join(me, name), []byte(text), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	code, body := f.do(t, "GET", "/api/me", "")
	if code != http.StatusOK || body["hasProfile"] != true || body["path"] != "topics/me/briefs/2026-09-21.md" {
		t.Fatalf("me = %d %v", code, body)
	}
	days, _ := body["days"].([]any)
	if len(days) != 2 || days[0] != "2026-09-21" {
		t.Fatalf("days = %v", days)
	}
	page, _ := body["page"].(map[string]any)
	blocks, _ := page["blocks"].([]any)
	if len(blocks) == 0 {
		t.Fatalf("page = %v", page)
	}
	if sig, _ := body["signals"].(map[string]any); sig["asOf"] != "2026-09-21T14:00:00Z" {
		t.Fatalf("signals = %v", body["signals"])
	}
	// The brief on disk is not today's (unless this test runs on 2026-09-21): stale says so.
	if stale := body["stale"] == true; stale != (body["date"] != "2026-09-21") {
		t.Fatalf("stale = %v for date %v", body["stale"], body["date"])
	}

	ran := 0
	f.api.brief = func() error { ran++; return nil }
	if code, _ := f.do(t, "POST", "/api/me/refresh", ""); code != http.StatusOK || ran != 1 {
		t.Fatalf("refresh = %d ran=%d", code, ran)
	}
}

// The Me page shows a brief job while it runs: its spinner is a fixed
// timer, so without this a job still writing (or one that already
// failed) looked the same as none. running counts the briefs this serve
// started until each exits, the second one that loses the wiki lock
// included.
func TestMeSaysWhileABriefRuns(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	f.api.home = f.home
	// The "brief" waits in the wiki dir (its cwd) for the test's go-ahead.
	exe := filepath.Join(f.home, "fake-bough")
	if err := os.WriteFile(exe, []byte("#!/bin/sh\nwhile [ ! -e go ]; do sleep 0.02; done\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	f.sup.opt.Exe = exe
	if _, body := f.do(t, "GET", "/api/me", ""); body["running"] == true {
		t.Fatalf("running before any refresh: %v", body)
	}
	if code, body := f.do(t, "POST", "/api/me/refresh", ""); code != http.StatusOK {
		t.Fatalf("refresh = %d %v", code, body)
	}
	if _, body := f.do(t, "GET", "/api/me", ""); body["running"] != true {
		t.Fatalf("running after refresh = %v", body["running"])
	}
	if err := os.WriteFile(filepath.Join(f.home, ".bough", "wiki", "go"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(10 * time.Second)
	for {
		if _, body := f.do(t, "GET", "/api/me", ""); body["running"] != true {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("running still true after the brief exited")
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func TestMeTriageAndSteer(t *testing.T) {
	f := newAPI(t)
	f.api.home = f.home
	me := filepath.Join(f.home, ".bough", "wiki", "topics", "me")
	if err := os.MkdirAll(me, 0o755); err != nil {
		t.Fatal(err)
	}
	// Steering before a profile exists is a conflict, not a crash.
	if code, _ := f.do(t, "POST", "/api/me/steer", `{"text":"watch x"}`); code != http.StatusConflict {
		t.Fatalf("steer without profile = %d", code)
	}
	if err := os.WriteFile(filepath.Join(me, "profile.md"), []byte("# Me\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	code, body := f.do(t, "POST", "/api/me/triage", `{"action":"dismiss","key":"gh:asi/x#1","rule":"nothing from asi/x"}`)
	if code != http.StatusOK {
		t.Fatalf("triage = %d %v", code, body)
	}
	if code, _ := f.do(t, "POST", "/api/me/triage", `{"action":"pin","key":"gh:asi/y#2"}`); code != http.StatusOK {
		t.Fatalf("pin = %d", code)
	}
	if code, _ := f.do(t, "POST", "/api/me/triage", `{"action":"nope","key":"k"}`); code != http.StatusBadRequest {
		t.Fatalf("bad action = %d", code)
	}
	code, body = f.do(t, "POST", "/api/me/steer", `{"text":"ignore dependabot"}`)
	if code != http.StatusOK || body["section"] != "Not mine" {
		t.Fatalf("steer = %d %v", code, body)
	}
	_, body = f.do(t, "GET", "/api/me", "")
	tr, _ := body["triage"].(map[string]any)
	dis, _ := tr["dismissed"].(map[string]any)
	pinned, _ := tr["pinned"].([]any)
	if dis["gh:asi/x#1"] == nil || len(pinned) != 1 {
		t.Fatalf("me triage = %v", tr)
	}
	prof, _ := os.ReadFile(filepath.Join(me, "profile.md"))
	if !strings.Contains(string(prof), "## Not mine") || !strings.Contains(string(prof), "nothing from asi/x") || !strings.Contains(string(prof), "ignore dependabot") {
		t.Fatalf("profile = %s", prof)
	}
}
