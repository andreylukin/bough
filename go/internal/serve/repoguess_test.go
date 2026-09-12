package serve

import (
	"net/http"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

func codeEntry(seq int64, text string) history.Entry {
	return history.Entry{Seq: seq, At: time.Now(), Kind: "code", Data: map[string]any{"text": text}}
}

// Every session this user runs starts in their home directory, so
// Row.Repo is empty on all of them. The repo is only knowable from the
// paths the session touched — without that, grouping is impossible and
// the Projects page is 150 dropdowns.
func TestRepoGroupsFromTouchedPaths(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	f.api.home = f.home
	now := time.Now()
	// Two branches of one repo: worktrees are "<repo>.<branch>", and
	// both must land in the same group rather than fragmenting.
	f.seed(t, "01a00000-0000-7000-8000-0000000000a1",
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": f.home}},
		codeEntry(2, `tools.bash("cd repos/worktree/uni-svc.andrey-feature-one && git status")`),
	)
	f.seed(t, "01a00000-0000-7000-8000-0000000000a2",
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": f.home}},
		codeEntry(2, `tools.bash("cd repos/worktree/uni-svc.andrey-feature-two && make test")`),
	)
	f.seed(t, "01a00000-0000-7000-8000-0000000000b1",
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": f.home}},
		codeEntry(2, `tools.bash("cd repos/other-thing && ls")`),
	)
	// A session that touched no repo must not invent one.
	f.seed(t, "01a00000-0000-7000-8000-0000000000c1",
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": f.home}},
		codeEntry(2, `tools.bash("echo hello")`),
	)

	code, body := f.do(t, "GET", "/api/projects/by-repo", "")
	if code != http.StatusOK {
		t.Fatalf("GET /api/projects/by-repo = %d", code)
	}
	groups, _ := body["groups"].([]any)
	got := map[string]int{}
	for _, g := range groups {
		m, _ := g.(map[string]any)
		n, _ := m["count"].(float64)
		got[m["repo"].(string)] = int(n)
	}
	if got["uni-svc"] != 2 {
		t.Errorf("uni-svc = %d, want 2 — two worktrees of one repo must group together (%v)", got["uni-svc"], got)
	}
	if got["other-thing"] != 1 {
		t.Errorf("other-thing = %d, want 1 (%v)", got["other-thing"], got)
	}
	if len(got) != 2 {
		t.Errorf("groups = %v, want only the two repos actually touched", got)
	}
}

// One call files a whole group: doing it from the client would be one
// request per session, and a real group here holds twenty.
func TestProjectFromRepoMovesTheGroup(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	f.api.home = f.home
	now := time.Now()
	for _, id := range []string{
		"01a00000-0000-7000-8000-0000000000d1",
		"01a00000-0000-7000-8000-0000000000d2",
	} {
		f.seed(t, id,
			history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": f.home}},
			codeEntry(2, `tools.bash("cd repos/uni-grouped && ls")`),
		)
	}
	code, body := f.do(t, "POST", "/api/projects/from-repo", `{"repo":"uni-grouped"}`)
	if code != http.StatusOK {
		t.Fatalf("POST /api/projects/from-repo = %d (%v)", code, body)
	}
	if moved, _ := body["moved"].(float64); moved != 2 {
		t.Errorf("moved = %v, want 2", body["moved"])
	}
	// Filed sessions leave the unassigned pile, or the button would keep
	// offering the same group forever.
	_, after := f.do(t, "GET", "/api/projects/by-repo", "")
	groups, _ := after["groups"].([]any)
	for _, g := range groups {
		if m, _ := g.(map[string]any); m["repo"] == "uni-grouped" {
			t.Errorf("still offered after filing: %v", m)
		}
	}
}

// An unknown repo is a 404, not a project with nothing in it.
func TestProjectFromRepoUnknown(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	f.api.home = f.home
	code, _ := f.do(t, "POST", "/api/projects/from-repo", `{"repo":"nope"}`)
	if code != http.StatusNotFound {
		t.Errorf("unknown repo = %d, want 404", code)
	}
}
