package attention

import (
	"io"
	"net"
	"net/http"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/graph"
	"github.com/andreylukin/bough/plugins/prwatch"
)

func TestBoardPlacesByTurn(t *testing.T) {
	st, err := graph.Open(filepath.Join(t.TempDir(), "graph.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()
	now := time.Date(2026, 9, 6, 12, 0, 0, 0, time.UTC)
	ep, _ := st.Episode("collector", "t")
	me, _ := st.Upsert("person", "andrey@example.com", "Andrey", "")
	bradley, _ := st.Upsert("person", "bradley@example.com", "Bradley", "")
	mk := func(key, title, summary string, age time.Duration) graph.Entity {
		e, _ := st.Upsert("pr", key, title, "")
		e, _ = st.SetLink(e, graph.Link{URL: "https://github.com/x/" + key, Summary: summary, UpdatedAt: now.Add(-age).Unix()})
		if err := st.SetState(e, "open", ep, "collector", now.Add(-age).Unix()); err != nil {
			t.Fatal(err)
		}
		return e
	}
	// Three dependabot bumps and one real review await me.
	for i, k := range []string{"bough#62", "bough#63", "bough#64"} {
		e := mk(k, "chore(deps): bump thing "+k, "ci failing, review required, branch dependabot/x", time.Duration(i+2)*24*time.Hour)
		st.Assert(e, "awaits", me, ep, "collector", graph.AssertOpts{})
	}
	review := mk("orb#142", "alert backtesting", "review required, branch feat/x", 3*time.Hour)
	st.Assert(review, "awaits", me, ep, "collector", graph.AssertOpts{})
	// Mine: one awaits Bradley, one is being worked by pr-watch, one has no next actor.
	mine := mk("nas#46", "fix sharding", "ci green, branch fix/x", 2*24*time.Hour)
	st.Assert(me, "authored", mine, ep, "collector", graph.AssertOpts{})
	st.Assert(mine, "awaits", bradley, ep, "collector", graph.AssertOpts{})
	worked := mk("bough#66", "fix thing", "ci failing, branch fix-thing", time.Hour)
	st.Assert(me, "authored", worked, ep, "collector", graph.AssertOpts{})
	lone := mk("mathlib4#42788", "facet incidence", "ci green, branch andrey/x", 5*24*time.Hour)
	st.Assert(me, "authored", lone, ep, "collector", graph.AssertOpts{})

	s := &Service{graph: &graph.Service{Store: st, Me: "andrey@example.com"}, Now: func() time.Time { return now }}
	s.locks = func() []working {
		return []prwatch.Working{{Key: "andreylukin/bough#66", Session: "8f3a1234-abcd", Since: now.Add(-14 * time.Minute), What: "1 review thread"}}
	}
	b := s.Board()
	if b.Err != "" {
		t.Fatal(b.Err)
	}
	// NEEDS ME: oldest debt first — one deps stack of three, then the real review.
	if len(b.Me) != 2 || b.Me[0].Key != "bough · deps" || b.Me[0].Count != 3 || b.Me[0].Status != "ci failing ×3" || b.Me[1].Key != "orb#142" {
		t.Fatalf("needs me: %+v", b.Me)
	}
	if b.Me[1].Detail != "review required" {
		t.Fatalf("ask: %q", b.Me[1].Detail)
	}
	if len(b.Motion) != 1 || b.Motion[0].Key != "bough#66" || b.Motion[0].Session != "8f3a1234-abcd" || b.Motion[0].Detail != "session 8f3a · 1 review thread" {
		t.Fatalf("in motion: %+v", b.Motion)
	}
	if len(b.Others) != 2 || b.Others[0].Key != "mathlib4#42788" || b.Others[0].Detail != "next: ?" || b.Others[1].Key != "nas#46" || b.Others[1].Detail != "awaits Bradley" {
		t.Fatalf("others: %+v", b.Others)
	}
	if b.Collected.IsZero() {
		t.Fatal("collected time missing")
	}
}

func TestAge(t *testing.T) {
	now := time.Now()
	for d, want := range map[time.Duration]int{
		10 * time.Minute: 0, time.Hour: 1, 24 * time.Hour: 4, 7 * 24 * time.Hour: 6, 30 * 24 * time.Hour: 8, 90 * 24 * time.Hour: 8,
	} {
		if got := Age(now.Add(-d), now); got != want {
			t.Errorf("Age(%v) = %d, want %d", d, got, want)
		}
	}
}

func TestShortKey(t *testing.T) {
	for in, want := range map[string]string{
		"uni-network-evaluation-scheduler#7362": "unes#7362", "bough#64": "bough#64", "NME-1664": "NME-1664", "uni-gitops-state-aws-prod#13322": "ugsap#13322",
	} {
		if got := ShortKey(in); got != want {
			t.Errorf("ShortKey(%q) = %q, want %q", in, got, want)
		}
	}
}

func TestDetail(t *testing.T) {
	st, err := graph.Open(filepath.Join(t.TempDir(), "graph.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()
	now := time.Date(2026, 9, 6, 12, 0, 0, 0, time.UTC)
	at := now.Add(-5 * 24 * time.Hour).Unix()
	ep, _ := st.Episode("collector", "t")
	me, _ := st.Upsert("person", "andrey@example.com", "Andrey", "")
	devin, _ := st.Upsert("person", "github:devin-ai-integration[bot]", "", "")
	pr, _ := st.Upsert("pr", "unes#7490", "Route demand through Pulsar", "")
	if err := st.SetState(pr, "open", ep, "collector", at); err != nil {
		t.Fatal(err)
	}
	tk, _ := st.Upsert("ticket", "NME-1664", "Add nas-event-log to prod", "")
	_ = st.SetState(tk, "code_review", ep, "collector", at)
	sess, _ := st.Upsert("session", "old:abc", "Deploy the event log", "")
	opts := graph.AssertOpts{ValidFrom: at}
	st.Assert(me, "authored", pr, ep, "collector", opts)
	st.Assert(devin, "reviews", pr, ep, "collector", graph.AssertOpts{ValidFrom: at, Claim: "commented"})
	st.Assert(pr, "awaits", me, ep, "collector", graph.AssertOpts{ValidFrom: at, Claim: "4 unresolved review threads"})
	st.Assert(pr, "implements", tk, ep, "collector", opts)
	st.Assert(sess, "touches", tk, ep, "session", opts)

	s := &Service{graph: &graph.Service{Store: st, Me: "andrey@example.com"}, Now: func() time.Time { return now }}
	s.recent = func(key string) (prwatch.Recent, bool) {
		return prwatch.Recent{Key: "o/unes#7490", Summary: "replied to 2 threads, pushed", At: now.Add(-time.Hour)}, key == "unes#7490"
	}
	got := map[string]string{}
	for _, l := range s.Detail("pr", "unes#7490") {
		got[l.Label] = l.Text
	}
	for label, want := range map[string]string{
		"asks":     "4 unresolved review threads (you)",
		"state":    "open since Sep 1",
		"who":      "you opened Sep 1 · devin-ai-integration commented Sep 1",
		"for":      "NME-1664 Add nas-event-log to prod [code_review]",
		"pr-watch": "replied to 2 threads, pushed · Sep 6",
	} {
		if got[label] != want {
			t.Errorf("%s = %q, want %q (all: %v)", label, got[label], want, got)
		}
	}
	got = map[string]string{}
	for _, l := range s.Detail("ticket", "NME-1664") {
		got[l.Label] = l.Text
	}
	if got["sessions"] != "Sep 1 “Deploy the event log”" || got["for"] != "unes#7490 Route demand through Pulsar [open]" {
		t.Errorf("ticket detail: %v", got)
	}
}

func TestWebEndpoints(t *testing.T) {
	st, err := graph.Open(filepath.Join(t.TempDir(), "graph.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()
	ep, _ := st.Episode("collector", "t")
	me, _ := st.Upsert("person", "andrey@example.com", "Andrey", "")
	pr, _ := st.Upsert("pr", "orb#142", "alert backtesting", "")
	pr, _ = st.SetLink(pr, graph.Link{URL: "https://github.com/x/orb/pull/142", Summary: "review required", UpdatedAt: time.Now().Unix()})
	_ = st.SetState(pr, "open", ep, "collector", time.Now().Unix())
	st.Assert(pr, "awaits", me, ep, "collector", graph.AssertOpts{Claim: "1 thread"})

	s := &Service{graph: &graph.Service{Store: st, Me: "andrey@example.com"}, Now: time.Now, web: "127.0.0.1:0"}
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	addr := ln.Addr().String()
	ln.Close()
	webAddr = "" // the process singleton, reset for the test
	s.web = addr
	s.serveWeb(addr)
	get := func(path string) string {
		r, err := http.Get("http://" + addr + path)
		if err != nil {
			t.Fatal(err)
		}
		defer r.Body.Close()
		b, _ := io.ReadAll(r.Body)
		return string(b)
	}
	if page := get("/"); !strings.Contains(page, "<title>current work</title>") || !strings.Contains(page, "/api/flow") {
		t.Fatalf("page: %.200s", page)
	}
	board := get("/api/board")
	if !strings.Contains(board, `"orb#142"`) || !strings.Contains(board, `"now"`) || !strings.Contains(board, `"review required"`) {
		t.Fatalf("board: %s", board)
	}
	detail := get("/api/detail?kind=pr&key=orb%23142")
	if !strings.Contains(detail, `"Label":"asks"`) || !strings.Contains(detail, "1 thread (you)") {
		t.Fatalf("detail: %s", detail)
	}
	if got := s.URL(); got != "http://"+addr {
		t.Fatalf("URL = %q", got)
	}
}

func TestFlowStages(t *testing.T) {
	st, err := graph.Open(filepath.Join(t.TempDir(), "graph.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()
	now := time.Date(2026, 9, 6, 12, 0, 0, 0, time.UTC)
	day := func(n int) int64 { return now.Add(-time.Duration(n) * 24 * time.Hour).Unix() }
	ep, _ := st.Episode("collector", "t")
	me, _ := st.Upsert("person", "andrey@example.com", "Andrey", "")
	bradley, _ := st.Upsert("person", "bradley@example.com", "Bradley", "")
	tk, _ := st.Upsert("ticket", "LIN-482", "rollout plan", "")
	pr, _ := st.Upsert("pr", "bough#61", "rollout doc", "")
	pr, _ = st.SetLink(pr, graph.Link{URL: "https://github.com/x/bough/pull/61", Summary: "review required, branch doc", UpdatedAt: day(2)})
	// Opened 5 days ago (building), review requested of Bradley 3 days ago,
	// Bradley answered and it came back to me 2 days ago.
	if err := st.SetState(pr, "open", ep, "collector", day(5)); err != nil {
		t.Fatal(err)
	}
	st.Assert(me, "authored", pr, ep, "collector", graph.AssertOpts{ValidFrom: day(5)})
	st.Assert(pr, "implements", tk, ep, "collector", graph.AssertOpts{ValidFrom: day(5)})
	e1, _ := st.Assert(pr, "awaits", bradley, ep, "collector", graph.AssertOpts{ValidFrom: day(3)})
	if err := st.Invalidate(e1.ID, "answered", "collector", day(2)); err != nil {
		t.Fatal(err)
	}
	st.Assert(pr, "awaits", me, ep, "collector", graph.AssertOpts{ValidFrom: day(2), Claim: "Bradley asked why"})

	s := &Service{graph: &graph.Service{Store: st, Me: "andrey@example.com"}, Now: func() time.Time { return now }}
	f := s.Flow(7)
	if len(f.Groups) != 1 || f.Groups[0].Key != "me" || len(f.Groups[0].Rows) != 1 {
		t.Fatalf("groups: %+v", f.Groups)
	}
	r := f.Groups[0].Rows[0]
	if r.Subject.Key != "LIN-482" || r.Subject.Title != "rollout plan" {
		t.Fatalf("subject: %+v", r.Subject)
	}
	var stages []string
	for _, sg := range r.Segments {
		stages = append(stages, sg.Stage)
	}
	// Nothing before it existed, then building → in review (Bradley) →
	// in review (me): the two review windows merge into one segment.
	if strings.Join(stages, ",") != "building,in review" {
		t.Fatalf("stages: %v (%+v)", stages, r.Segments)
	}
	if got := r.Segments[0].From.Unix(); got != day(5) {
		t.Errorf("building starts at open: %v", r.Segments[0].From)
	}
	if got := r.Segments[1].From.Unix(); got != day(3) {
		t.Errorf("review starts at the first awaits: %v", r.Segments[1].From)
	}
	var marks []string
	for _, m := range r.Marks {
		marks = append(marks, m.Kind+":"+m.Text)
	}
	if strings.Join(marks, " | ") != "other:you opened | other:→ Bradley | me:→ you" {
		t.Fatalf("marks: %v", marks)
	}
	if r.Marks[2].Claim != "Bradley asked why" {
		t.Errorf("claim on the mark: %+v", r.Marks[2])
	}
}
