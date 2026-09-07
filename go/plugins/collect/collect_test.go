package collect

import (
	"encoding/json"
	"fmt"
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
		// The MCP server's shape: "id" is the identifier, "uuid" the id,
		// status and assignee are strings.
		return `{"issues":[
		  {"id":"NME-1673","uuid":"225bd04c-1","title":"graph memory","description":"See https://github.com/asi/uni-nas-event-log/pull/46 for the first cut.\n\n## Done when","url":"https://linear.app/asi/issue/NME-1673/graph-memory","gitBranchName":"andrey/nme-1673-graph-memory","updatedAt":"2026-09-05T10:00:00.000Z","status":"In Progress","statusType":"started","assignee":"Andrey Lukin","assigneeId":"28c940f7-me"},
		  {"id":"NME-1516","uuid":"7eea1225-2","title":"old thing","url":"https://linear.app/asi/issue/NME-1516","updatedAt":"2026-09-04T10:00:00.000Z","status":"Done ","assignee":"Andrey Lukin","assigneeId":"28c940f7-me"}
		]}`, nil
	}
	rep := r.Linear("linear-server")
	if rep.Err != nil {
		t.Fatal(rep.Err)
	}
	tk, _ := r.St.Get("ticket", "NME-1673")
	if tk.Title != "graph memory" || tk.Status != "in_progress" || tk.URL == "" || !strings.HasPrefix(tk.Summary, "See https://github.com") {
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
	if id, _ := r.St.AliasOwner("linear", "28c940f7-me"); id != r.Me.ID {
		t.Fatal("linear assignee id not aliased to me")
	}
	if id, _ := r.St.AliasOwner("linear", "225bd04c-1"); id != tk.ID {
		t.Fatal("issue uuid not aliased to the ticket")
	}
}

const slackSearchReply = `{"results":"# Search Results for: <@U_ME>\n\n## Messages (1 results)\n### Result 1 of 1\nChannel: #nm-echo (ID: C1)\nFrom: Bradley Bares <bradley@example.com> (ID: U_BRAD) \nTime: 2026-09-05 04:48:10 EDT\nMessage_ts: 1757064000.000100\nReply count: 1\nPermalink: [link](https://asi.slack.com/archives/C1/p1757064000000100?thread_ts=1757064000.000100&cid=C1)\nText: \n<@U_ME|Andrey Lukin> can you look at NME-1673?\nContext after: \n- From: Olivier S <olivier@example.com> (ID: U_OLI) \n  Message_ts: 1757065000.000200\n  I think it is the sharding\n%s\n---\n"}`

func TestSlackThreadsAwaitMeUntilIReply(t *testing.T) {
	r := newRun(t)
	myReply := ""
	r.call = func(server, tool, args string) (string, error) {
		switch tool {
		case "slack_read_user_profile":
			return `{"result":"User ID: U_ME\nUsername: andrey\nReal Name: Andrey Lukin\nEmail: andrey@example.com\n"}`, nil
		case "slack_search_public_and_private":
			if strings.Contains(args, "<@U_ME> after:") {
				return fmt.Sprintf(slackSearchReply, myReply), nil
			}
			return `{"results":"# Search Results\n\n## Messages (0 results)\n"}`, nil
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
	if !strings.HasPrefix(th.URL, "https://asi.slack.com/archives/C1/p1757064000000100") || th.Status != "open" || !strings.HasPrefix(th.Title, "#nm-echo:") {
		t.Fatalf("thread: %+v", th)
	}
	w, _ := r.St.WorldOf(r.Me)
	if !strings.Contains(w.Render(), "slack_thread:C1:1757064000.000100") {
		t.Fatalf("world:\n%s", w.Render())
	}
	if n, _ := r.St.Neighbors(th, 1, "discusses", 0); len(n) != 1 || n[0].Dst.Key != "NME-1673" {
		t.Fatalf("discusses: %+v", n)
	}
	// People come with emails, so Bradley is one node by email.
	if b, err := r.St.Get("person", "bradley@example.com"); err != nil || b.Title != "Bradley Bares" {
		t.Fatalf("bradley: %+v %v", b, err)
	}
	if id, _ := r.St.AliasOwner("slack", "U_ME"); id != r.Me.ID {
		t.Fatal("my slack id not aliased")
	}

	// I replied last: answered, no longer awaiting me.
	myReply = `- From: Andrey Lukin <andrey@example.com> (ID: U_ME) \n  Message_ts: 1757070000.000300\n  on it`
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
		return `{"results":[{"type":"page","url":"https://app.notion.com/p/3c8bd6d804a8816fbbd8e777b56602b5?pvs=204","title":"NME-1673 design notes","icon":"🧿"}]}`, nil
	}
	rep := r.Notion("notion")
	if rep.Err != nil || rep.Entities != 1 || rep.Edges != 1 {
		t.Fatalf("%+v", rep)
	}
	p, _ := r.St.Get("notion_page", "3c8bd6d804a8816fbbd8e777b56602b5")
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

// Each fact lands at the source's own time, so a first pass over an
// old PR draws its real history; a changed awaits claim opens a new
// window; a later pass backdates an edge the first one stamped late.
func TestGithubSourceTimes(t *testing.T) {
	r := newRun(t)
	view := map[string]any{
		"number": 7, "title": "route demand", "url": "https://github.com/asi/orb/pull/7",
		"state": "OPEN", "isDraft": false, "createdAt": "2026-08-20T09:00:00Z", "updatedAt": "2026-09-05T10:00:00Z", "headRefName": "andrey/route",
		"author":  map[string]any{"login": "andreylukin"},
		"reviews": []any{map[string]any{"state": "COMMENTED", "submittedAt": "2026-08-28T12:00:00Z", "author": map[string]any{"login": "hubert"}}},
		"reviewRequests": []any{}, "reviewDecision": "REVIEW_REQUIRED", "statusCheckRollup": []any{},
	}
	threads := "4\n"
	r.gh = func(args ...string) ([]byte, error) {
		switch {
		case args[0] == "api" && args[1] == "user":
			return []byte("andreylukin\n"), nil
		case args[0] == "search" && strings.HasPrefix(args[2], "--author"):
			return []byte(`[{"url":"https://github.com/asi/orb/pull/7"}]`), nil
		case args[0] == "search":
			return []byte(`[]`), nil
		case args[0] == "pr":
			b, _ := json.Marshal(view)
			return b, nil
		case args[0] == "api" && args[1] == "graphql":
			return []byte(threads), nil
		}
		t.Fatalf("unexpected gh %v", args)
		return nil, nil
	}
	if rep := r.Github(); rep.Err != nil {
		t.Fatal(rep.Err)
	}
	pr, _ := r.St.Get("pr", "orb#7")
	at := func(rel, dstKey string) (from int64, to *int64, claim string) {
		tl, _ := r.St.Timeline(pr)
		for _, e := range tl {
			if e.Rel == rel && (dstKey == "" || e.Dst.Key == dstKey || e.Src.Key == dstKey) && e.ValidTo == nil {
				return e.ValidFrom, e.ValidTo, e.Claim
			}
		}
		return 0, nil, ""
	}
	born := int64(1787216400) // 2026-08-20T09:00:00Z
	if f, _, _ := at("has_state", "open"); f != born {
		t.Errorf("open since createdAt: %d", f)
	}
	if f, _, _ := at("authored", ""); f != born {
		t.Errorf("authored at createdAt: %d", f)
	}
	if f, _, _ := at("reviews", ""); f != 1787918400 {
		t.Errorf("review at submittedAt: %d", f)
	}
	if _, _, c := at("awaits", ""); c != "4 unresolved review threads" {
		t.Errorf("awaits claim: %q", c)
	}
	// Two threads resolved: the old window closes, a new claim opens.
	threads = "2\n"
	view["updatedAt"] = "2026-09-06T10:00:00Z"
	if rep := r.Github(); rep.Err != nil {
		t.Fatal(rep.Err)
	}
	tl, _ := r.St.Timeline(pr)
	var closed, open int
	for _, e := range tl {
		if e.Rel == "awaits" {
			if e.ValidTo != nil {
				closed++
			} else if e.Claim == "2 unresolved review threads" {
				open++
			}
		}
	}
	if closed != 1 || open != 1 {
		t.Fatalf("awaits windows: closed %d open %d", closed, open)
	}
}
