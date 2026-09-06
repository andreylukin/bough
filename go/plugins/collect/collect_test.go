package collect

import (
	"encoding/json"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/graph"
)

func newRun(t *testing.T) *Run {
	t.Helper()
	st, err := graph.Open(filepath.Join(t.TempDir(), "graph.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { st.Close() })
	r, err := NewRun(st, "andrey@example.com")
	if err != nil {
		t.Fatal(err)
	}
	r.Now = time.Date(2026, 9, 6, 12, 0, 0, 0, time.UTC).Unix()
	return r
}

func TestGithubRecordsLinkTruthAndAwaits(t *testing.T) {
	r := newRun(t)
	view := map[string]any{
		"number": 46, "title": "NME-1673 fix sharding", "url": "https://github.com/asi/uni-nas-event-log/pull/46",
		"state": "OPEN", "isDraft": false, "updatedAt": "2026-09-05T10:00:00Z", "headRefName": "andrey/nme-1673-fix",
		"author":            map[string]any{"login": "andreylukin"},
		"reviewRequests":    []any{map[string]any{"login": "bradley"}},
		"reviews":           []any{map[string]any{"state": "COMMENTED", "author": map[string]any{"login": "devin-ai"}}},
		"reviewDecision":    "REVIEW_REQUIRED",
		"statusCheckRollup": []any{map[string]any{"conclusion": "SUCCESS"}},
	}
	r.gh = func(args ...string) ([]byte, error) {
		switch {
		case args[0] == "api" && args[1] == "user":
			return []byte("andreylukin\n"), nil
		case args[0] == "search" && strings.HasPrefix(args[2], "--author"):
			return []byte(`[{"url":"https://github.com/asi/uni-nas-event-log/pull/46"}]`), nil
		case args[0] == "search":
			return []byte(`[]`), nil
		case args[0] == "pr":
			b, _ := json.Marshal(view)
			return b, nil
		case args[0] == "api" && args[1] == "graphql":
			return []byte("2\n"), nil
		}
		t.Fatalf("unexpected gh %v", args)
		return nil, nil
	}
	rep := r.Github()
	if rep.Err != nil {
		t.Fatal(rep.Err)
	}
	pr, err := r.St.Get("pr", "uni-nas-event-log#46")
	if err != nil {
		t.Fatal(err)
	}
	if pr.URL != "https://github.com/asi/uni-nas-event-log/pull/46" || pr.Status != "open" || !strings.Contains(pr.Summary, "2 unresolved threads") || !strings.Contains(pr.Summary, "ci green") {
		t.Fatalf("link truth: %+v", pr)
	}
	// The author login is me (aliased), so the PR is mine and, with
	// open bot threads, awaits me; Bradley's requested review awaits him.
	w, _ := r.St.WorldOf(r.Me)
	out := w.Render()
	if strings.Contains(out, "Mine, open:") || !strings.Contains(out, "awaits bradley") || !strings.Contains(out, "Waiting on me:") {
		t.Fatalf("world:\n%s", out)
	}
	tk, err := r.St.Get("ticket", "NME-1673")
	if err != nil {
		t.Fatal("the branch's ticket was not linked")
	}
	if n, _ := r.St.Neighbors(tk, 1, "implements", 0); len(n) != 1 {
		t.Fatalf("implements: %+v", n)
	}
	if id, err := r.St.AliasOwner("github", "andreylukin"); err != nil || id != r.Me.ID {
		t.Fatal("my login is not aliased to me")
	}

	// Merged: the PR leaves the world and nobody is awaited.
	view["state"] = "MERGED"
	view["updatedAt"] = "2026-09-06T09:00:00Z"
	if rep := r.Github(); rep.Err != nil {
		t.Fatal(rep.Err)
	}
	w, _ = r.St.WorldOf(r.Me)
	if !w.Empty() {
		t.Fatalf("merged PR still in the world:\n%s", w.Render())
	}
	tl, _ := r.St.Timeline(pr)
	states := 0
	for _, e := range tl {
		if e.Rel == "has_state" {
			states++
		}
	}
	if states != 2 {
		t.Fatalf("state history: %d", states)
	}
}

func TestLinearFillsTitlesAndStates(t *testing.T) {
	r := newRun(t)
	r.call = func(server, tool, args string) (string, error) {
		if tool != "list_issues" || !strings.Contains(args, `"assignee":"me"`) {
			t.Fatalf("unexpected %s %s", tool, args)
		}
		return "Here are the issues:\n```json\n" + `{"issues":[
		  {"id":"uuid-1","identifier":"NME-1673","title":"graph memory","url":"https://linear.app/asi/issue/NME-1673","state":{"name":"In Progress"},"updatedAt":"2026-09-05T10:00:00Z",
		   "assignee":{"id":"lin-me","name":"Andrey","email":"andrey@example.com"},
		   "description":"See https://github.com/asi/uni-nas-event-log/pull/46 for the first cut."},
		  {"id":"uuid-2","identifier":"NME-1516","title":"old thing","url":"https://linear.app/asi/issue/NME-1516","state":"Done ","updatedAt":"2026-09-04T10:00:00Z"}
		]}` + "\n```", nil
	}
	rep := r.Linear("linear-server")
	if rep.Err != nil {
		t.Fatal(rep.Err)
	}
	tk, _ := r.St.Get("ticket", "NME-1673")
	if tk.Title != "graph memory" || tk.Status != "in_progress" || tk.URL == "" {
		t.Fatalf("ticket: %+v", tk)
	}
	done, _ := r.St.Get("ticket", "NME-1516")
	if done.Status != "done" {
		t.Fatalf("status normalizes 'Done ': %q", done.Status)
	}
	w, _ := r.St.WorldOf(r.Me)
	out := w.Render()
	if !strings.Contains(out, "ticket:NME-1673") || strings.Contains(out, "NME-1516") {
		t.Fatalf("world:\n%s", out)
	}
	if pr, err := r.St.Get("pr", "uni-nas-event-log#46"); err != nil {
		t.Fatal("PR in the description not linked")
	} else if n, _ := r.St.Neighbors(pr, 1, "implements", 0); len(n) != 1 {
		t.Fatalf("implements: %+v", n)
	}
	if id, _ := r.St.AliasOwner("linear", "lin-me"); id != r.Me.ID {
		t.Fatal("linear assignee id not aliased to me")
	}
}

func TestSlackThreadsAwaitMeUntilIReply(t *testing.T) {
	r := newRun(t)
	thread := []any{
		map[string]any{"ts": "1757064000.000100", "user": "U_BRAD", "text": "<@U_ME> can you look at NME-1673?"},
	}
	r.call = func(server, tool, args string) (string, error) {
		switch tool {
		case "slack_read_user_profile":
			return `{"id":"U_ME","email":"andrey@example.com","real_name":"Andrey"}`, nil
		case "slack_search_public_and_private":
			if strings.Contains(args, "<@U_ME>") {
				return `{"messages":{"matches":[{"ts":"1757064000.000100","user":"U_BRAD","text":"<@U_ME> can you look at NME-1673?","channel":{"id":"C1","name":"nm-echo"},"permalink":"https://asi.slack.com/archives/C1/p1757064000000100"}]}}`, nil
			}
			return `{"messages":{"matches":[]}}`, nil
		case "slack_read_thread":
			b, _ := json.Marshal(map[string]any{"messages": thread})
			return string(b), nil
		}
		t.Fatalf("unexpected %s", tool)
		return "", nil
	}
	if rep := r.Slack("slack", nil); rep.Err != nil {
		t.Fatal(rep.Err)
	}
	th, err := r.St.Get("slack_thread", "C1:1757064000.000100")
	if err != nil {
		t.Fatal(err)
	}
	if th.URL != "https://asi.slack.com/archives/C1/p1757064000000100" || th.Status != "open" || !strings.HasPrefix(th.Title, "#nm-echo:") {
		t.Fatalf("thread: %+v", th)
	}
	w, _ := r.St.WorldOf(r.Me)
	if !strings.Contains(w.Render(), "slack_thread:C1:1757064000.000100") {
		t.Fatalf("world:\n%s", w.Render())
	}
	if n, _ := r.St.Neighbors(th, 1, "discusses", 0); len(n) != 1 || n[0].Dst.Key != "NME-1673" {
		t.Fatalf("discusses: %+v", n)
	}

	// I replied: answered, no longer awaiting me.
	thread = append(thread, map[string]any{"ts": "1757070000.000200", "user": "U_ME", "text": "on it"})
	if rep := r.Slack("slack", nil); rep.Err != nil {
		t.Fatal(rep.Err)
	}
	th, _ = r.St.Get("slack_thread", "C1:1757064000.000100")
	if th.Status != "answered" {
		t.Fatalf("status: %q", th.Status)
	}
	w, _ = r.St.WorldOf(r.Me)
	if !w.Empty() {
		t.Fatalf("answered thread still in the world:\n%s", w.Render())
	}
}

func TestNotionPagesLink(t *testing.T) {
	r := newRun(t)
	r.call = func(server, tool, args string) (string, error) {
		return `{"pages":[{"id":"abc123","title":"NME-1673 design notes","url":"https://notion.so/NME-1673-design-abc123","last_edited_time":"2026-09-05T10:00:00Z"}]}`, nil
	}
	rep := r.Notion("notion")
	if rep.Err != nil || rep.Entities != 1 || rep.Edges != 1 {
		t.Fatalf("%+v", rep)
	}
	p, _ := r.St.Get("notion_page", "abc123")
	if p.URL == "" || p.Title == "" {
		t.Fatalf("page: %+v", p)
	}
}

func TestHelpers(t *testing.T) {
	if got := jsonIn("Result:\n```json\n[1]\n```"); got != "[1]" {
		t.Errorf("jsonIn fence: %q", got)
	}
	if got := status("In Progress"); got != "in_progress" {
		t.Errorf("status: %q", got)
	}
	if when("1757064000.000100") != 1757064000 || when("2026-09-05T10:00:00Z") == 0 {
		t.Error("when")
	}
	if _, err := parseConfig(map[string]any{"bogus": 1}); err == nil {
		t.Error("unknown key accepted")
	}
	c, err := parseConfig(map[string]any{"me": "a@b.c", "linear": "", "every": "5m"})
	if err != nil || c.Linear != "" || c.Every != 5*time.Minute || c.Me != "a@b.c" {
		t.Errorf("%+v %v", c, err)
	}
}
