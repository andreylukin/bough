package attention

import (
	"path/filepath"
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
	if len(b.Motion) != 1 || b.Motion[0].Key != "bough#66" || b.Motion[0].Session != "8f3a1234-abcd" || b.Motion[0].Detail != "1 review thread · session 8f3a" {
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
		10 * time.Minute: 0, time.Hour: 1, 24 * time.Hour: 5, 7 * 24 * time.Hour: 8, 30 * 24 * time.Hour: 8,
	} {
		if got := Age(now.Add(-d), now); got != want {
			t.Errorf("Age(%v) = %d, want %d", d, got, want)
		}
	}
}
