package prwatch

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"
)

func TestIsBot(t *testing.T) {
	for login, want := range map[string]bool{
		"chatgpt-codex-connector[bot]": true, "devin-ai-integration[bot]": true, "Copilot": true,
		"dependabot[bot]": true, "bradley": false, "andreylukin": false,
	} {
		if got := isBot(login, defaultBots); got != want {
			t.Errorf("isBot(%q) = %v", login, got)
		}
	}
	if isBot("dependabot[bot]", nil) != true || isBot("bradley", nil) != false {
		t.Fatal("nil bots list still recognises the [bot] suffix")
	}
}

func fixture() PR {
	at := time.Now().Add(-time.Hour)
	return PR{
		Owner: "andreylukin", Name: "bough", Number: 12, Title: "fix thing", Branch: "fix-thing", HeadSHA: "abc123abc123", Updated: at,
		Threads: []Thread{
			{ID: "T1", Path: "a.go", Line: 3, Comments: []Comment{{ID: "101", Author: "chatgpt-codex-connector[bot]", Body: "nil check missing", At: at}}},
			{ID: "T2", Path: "b.go", Line: 9, Comments: []Comment{{ID: "102", Author: "bradley", Body: "rename this", At: at}, {ID: "103", Author: "andreylukin", Body: "done", At: at}}},
			{ID: "T3", Path: "c.go", Line: 1, Resolved: true, Comments: []Comment{{ID: "104", Author: "bradley", Body: "old", At: at}}},
			{ID: "T4", Path: "d.go", Line: 2, Comments: []Comment{{ID: "105", Author: "bradley", Body: "why?", At: at}}},
		},
		Comments: []Comment{
			{ID: "C1", Author: "dependabot[bot]", Body: "superseded", At: at},
			{ID: "C2", Author: "bradley", Body: "can you also bump the version?", At: at},
			{ID: "C3", Author: "andreylukin", Body: "sure", At: at},
		},
		Checks: []Check{{Name: "ci-go", State: "FAILURE", Link: "https://github.com/x/y/actions/runs/77"}, {Name: "lint", State: "SUCCESS"}},
	}
}

func TestJudge(t *testing.T) {
	pr := fixture()
	w := judge(pr, "andreylukin", defaultBots, &prState{}, time.Now())
	ids := func(ts []Thread) []string {
		var out []string
		for _, x := range ts {
			out = append(out, x.ID)
		}
		return out
	}
	if got := ids(w.Threads); strings.Join(got, ",") != "T1,T4" {
		t.Fatalf("threads: want the bot's and the unanswered person's, got %v", got)
	}
	if len(w.Comments) != 1 || w.Comments[0].ID != "C2" {
		t.Fatalf("comments: want the person's question only, got %+v", w.Comments)
	}
	if len(w.CI) != 1 || w.CI[0].Name != "ci-go" {
		t.Fatalf("ci: want the failed check, got %+v", w.CI)
	}
	// Once handled, nothing repeats: seen ids and the CI head.
	w2 := judge(pr, "andreylukin", defaultBots, &prState{Seen: []string{"101", "105", "C2"}, CIHead: pr.HeadSHA}, time.Now())
	if !w2.Empty() {
		t.Fatalf("handled PR should be empty, got %+v", w2)
	}
	// A new head reopens CI.
	pr.HeadSHA = "def456"
	if w3 := judge(pr, "andreylukin", defaultBots, &prState{Seen: []string{"101", "105", "C2"}, CIHead: "abc123abc123"}, time.Now()); len(w3.CI) != 1 {
		t.Fatal("a new head must reopen CI")
	}
	// Two jobs started on a comment without marking it: stop asking.
	if w5 := judge(pr, "andreylukin", defaultBots, &prState{Attempts: map[string]int{"101": 2, "105": 1, "C2": 2}}, time.Now()); len(w5.Threads) != 1 || w5.Threads[0].ID != "T4" || len(w5.Comments) != 0 {
		t.Fatalf("attempted-out comments must not come back: %+v", w5)
	}
	// Stuck: only pending checks on a head older than 30 minutes.
	pr.Checks = []Check{{Name: "ci-go", State: "QUEUED"}}
	if w4 := judge(pr, "andreylukin", defaultBots, &prState{}, time.Now()); !w4.CIStuck {
		t.Fatal("queued-only checks on an hour-old head are stuck")
	}
}

func TestTaskAndDescribe(t *testing.T) {
	pr := fixture()
	w := &Watcher{cfg: Config{Bots: defaultBots}, me: "andreylukin"}
	work := judge(pr, "andreylukin", defaultBots, &prState{}, time.Now())
	task := w.task(pr, work, "/tmp/wt/pr-12")
	for _, want := range []string{
		"cd /tmp/wt/pr-12 &&", "git push origin HEAD:fix-thing",
		"[thread T1] a.go:3 (REVIEW BOT)", "[thread T4] d.go:2 (person)", "NEVER resolve",
		"pulls/12/comments/<COMMENT_ID>/replies", "resolveReviewThread", "issues/12/comments",
		"[comment C2] by bradley", "ci-go: FAILURE https://github.com/x/y/actions/runs/77", "never pass a time limit to tools.bash",
	} {
		if !strings.Contains(task, want) {
			t.Errorf("task missing %q", want)
		}
	}
	if strings.Contains(task, "[thread T2]") || strings.Contains(task, "[thread T3]") {
		t.Error("answered or resolved threads must not be in the task")
	}
	if d := describe(work); d != "2 review threads, 1 comment, 1 failed check" {
		t.Fatalf("describe: %q", d)
	}
}

func TestStateLockAndRows(t *testing.T) {
	sf := &stateFile{path: filepath.Join(t.TempDir(), "repo.json")}
	w := &Watcher{state: sf, session: "sess-a-1234", ctx: context.Background()}
	spawned := ""
	w.spawn = func(_ context.Context, task string, _ map[string]any, sink func(string, string)) (any, error) {
		spawned = task
		sink("code", "tools.bash('go test ./...')")
		return map[string]any{"handled": []any{"101"}, "noted": []any{"C2"}, "resolved": []any{"T1"}, "pushed": true, "summary": "fixed the nil check", "blocked": "T4 asks about a design the author must decide"}, nil
	}
	// Another session holds the lock: handle must do nothing.
	_ = sf.update(func(st *state) error {
		st.PRs["andreylukin/bough#12"] = &prState{Lock: &lock{Session: "other", Since: time.Now(), What: "1 review thread"}, Title: "fix thing"}
		return nil
	})
	rows := w.Rows()
	if len(rows) != 1 || !strings.Contains(rows[0], "session other") || !strings.Contains(rows[0], "1 review thread") {
		t.Fatalf("rows should show the other session's job: %v", rows)
	}
	pr := fixture()
	work := judge(pr, "andreylukin", defaultBots, &prState{}, time.Now())
	w.handle(pr, work)
	if spawned != "" {
		t.Fatal("must not run while another session holds the lock")
	}
	// A stale lock is taken over; the job records what was handled.
	_ = sf.update(func(st *state) error {
		st.PRs["andreylukin/bough#12"].Lock.Since = time.Now().Add(-2 * lockStale)
		return nil
	})
	w.worktreeFn = func(context.Context, PR) (string, func(), error) { return "/tmp/wt", func() {}, nil }
	w.handle(pr, work)
	if spawned == "" {
		t.Fatal("stale lock must be taken over")
	}
	st, _ := sf.load()
	ps := st.PRs["andreylukin/bough#12"]
	b, _ := json.Marshal(ps)
	if ps.Lock != nil || ps.CIHead != pr.HeadSHA || !strings.Contains(string(b), `"101"`) || !strings.Contains(string(b), `"C2"`) || slices.Contains(ps.Seen, "105") {
		t.Fatalf("after the job: lock released, CI head recorded, handled and noted ids seen (not the unanswered one): %s", b)
	}
	// The unanswered comment counts one attempt; the seen ones none.
	if ps.Attempts["105"] != 1 || ps.Attempts["101"] != 0 || ps.Blocked != "T4 asks about a design the author must decide" {
		t.Fatalf("attempts/blocked: %+v", ps)
	}
	rows = w.Rows()
	if len(rows) != 1 || !strings.Contains(rows[0], "blocked") || !strings.Contains(rows[0], "author must decide") {
		t.Fatalf("rows should show the blocker: %v", rows)
	}
	bg := w.Background()
	for _, want := range []string{"andreylukin/bough#12 · finished", "code · tools.bash('go test ./...')", "done (pushed) · fixed the nil check"} {
		if !strings.Contains(bg, want) {
			t.Fatalf("/background missing %q:\n%s", want, bg)
		}
	}
}

func TestPruneWorktrees(t *testing.T) {
	home := t.TempDir()
	sf := &stateFile{path: filepath.Join(home, "state.json")}
	w := &Watcher{state: sf, home: home, session: "sess-live-1234", ctx: context.Background()}
	clone := filepath.Join(home, "repos", "o_r")
	if err := runGitInit(clone); err != nil {
		t.Skip("git unavailable:", err)
	}
	mk := func(name string) string {
		p := filepath.Join(home, "wt", "o_r", name)
		if _, err := runCmd(context.Background(), clone, "git", "worktree", "add", "--detach", p); err != nil {
			t.Fatal(err)
		}
		return p
	}
	live := mk("pr-7-sess-liv")
	dead := mk("pr-7-sess-dea")
	_ = sf.update(func(st *state) error {
		st.PRs["o/r#7"] = &prState{Lock: &lock{Session: "sess-live-1234", Since: time.Now()}}
		return nil
	})
	w.pruneWorktrees()
	if _, err := os.Stat(live); err != nil {
		t.Error("the live job's worktree must stay")
	}
	if _, err := os.Stat(dead); err == nil {
		t.Error("the dead job's worktree must go")
	}
	// Shutdown gives the PR back.
	w.releaseMine()
	st, _ := sf.load()
	if st.PRs["o/r#7"].Lock != nil {
		t.Error("releaseMine must drop this session's lock")
	}
}

func runGitInit(dir string) error {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	for _, args := range [][]string{{"init", "-q"}, {"-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "init"}} {
		if out, err := runCmd(context.Background(), dir, "git", args...); err != nil {
			return fmt.Errorf("%v: %s", err, out)
		}
	}
	return nil
}
