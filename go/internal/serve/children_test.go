package serve

import (
	"errors"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// notices are the stored notice entries in a session's file.
func notices(t *testing.T, f *fixture, id string) []string {
	t.Helper()
	es, _ := history.ReadFile(filepath.Join(f.hist, id+".jsonl"))
	var out []string
	for _, e := range es {
		if e.Kind == "notice" {
			txt, _ := e.Data["text"].(string)
			out = append(out, txt)
		}
	}
	return out
}

func waitNotices(t *testing.T, f *fixture, id string, n int) []string {
	t.Helper()
	waitFor(t, "notices", func() bool { return len(notices(t, f, id)) >= n })
	// Long enough for a duplicate report (a trailing exit, a second
	// done event) to have landed if the guard were broken.
	time.Sleep(300 * time.Millisecond)
	return notices(t, f, id)
}

func childFixture(t *testing.T) *fixture {
	t.Helper()
	f := newFixture(t, envTurns+"=1")
	f.seed(t, "parent")
	return f
}

// A finished child reports once to a stopped parent, which gets a
// stored notice and is never started.
func TestChildReportsOnceToStoppedParent(t *testing.T) {
	t.Parallel()
	f := childFixture(t)
	id, queued, err := f.sup.CreateChild(CreateOptions{Prompt: "hello", SpawnedBy: "parent"}, 0, 0)
	if err != nil || queued {
		t.Fatalf("CreateChild = %q %v %v", id, queued, err)
	}
	got := waitNotices(t, f, "parent", 1)
	want := "[agent hello · " + id + " finished] echo hello"
	if len(got) != 1 || got[0] != want {
		t.Fatalf("notices = %q, want [%q]", got, want)
	}
	if f.sup.Live("parent") || f.startCount(t) != 1 {
		t.Fatalf("parent started: live=%v starts=%d", f.sup.Live("parent"), f.startCount(t))
	}
	in, ok := f.sup.infoOf(id)
	if !ok || in.SpawnedBy != "parent" {
		t.Fatalf("child file %s: %+v %v", id, in, ok)
	}
	if f.sup.Meta(id).SpawnedBy != "parent" {
		t.Fatalf("meta = %+v", f.sup.Meta(id))
	}
}

func TestChildReportWords(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct{ prompt, word, reply string }{
		{"FAIL now", "failed", "echo FAIL now"},
		{"CANCEL now", "stopped", ""},
		{"EXIT now", "stopped", ""},
		// The done event arrives before the entries: the report waits.
		{"RACE now", "finished", "echo RACE now"},
	} {
		t.Run(tc.word+"/"+tc.prompt, func(t *testing.T) {
			t.Parallel()
			f := childFixture(t)
			id, _, err := f.sup.CreateChild(CreateOptions{Prompt: tc.prompt, SpawnedBy: "parent"}, 0, 0)
			if err != nil {
				t.Fatal(err)
			}
			got := waitNotices(t, f, "parent", 1)
			want := "[agent " + tc.prompt + " · " + id + " " + tc.word + "] " + tc.reply
			if len(got) != 1 || got[0] != want {
				t.Fatalf("notices = %q, want [%q]", got, want)
			}
		})
	}
}

func TestChildReportTruncates(t *testing.T) {
	t.Parallel()
	f := childFixture(t)
	prompt := strings.Repeat("é", 2500)
	id, _, err := f.sup.CreateChild(CreateOptions{Prompt: prompt, SpawnedBy: "parent"}, 0, 0)
	if err != nil {
		t.Fatal(err)
	}
	got := waitNotices(t, f, "parent", 1)
	suffix := "…\n(tools.agent(\"" + id + "\") has the full reply)"
	if len(got) != 1 || !strings.HasSuffix(got[0], suffix) {
		t.Fatalf("notice tail = %q", got)
	}
	reply := strings.TrimSuffix(got[0][strings.Index(got[0], "] ")+2:], suffix)
	if n := len([]rune(reply)); n != reportRunes {
		t.Fatalf("reply is %d runes, want %d", n, reportRunes)
	}
}

func TestChildQueueStartsInOrder(t *testing.T) {
	t.Parallel()
	f := childFixture(t)
	a, qa, err := f.sup.CreateChild(CreateOptions{Prompt: "HANG a", SpawnedBy: "parent"}, 0, 1)
	if err != nil || qa {
		t.Fatalf("a: %v %v", qa, err)
	}
	b, qb, err := f.sup.CreateChild(CreateOptions{Prompt: "b", SpawnedBy: "parent"}, 0, 1)
	if err != nil || !qb {
		t.Fatalf("b not queued: %v %v", qb, err)
	}
	c, qc, err := f.sup.CreateChild(CreateOptions{Prompt: "c", SpawnedBy: "parent"}, 0, 1)
	if err != nil || !qc {
		t.Fatalf("c not queued: %v %v", qc, err)
	}
	time.Sleep(200 * time.Millisecond)
	if n := f.startCount(t); n != 1 {
		t.Fatalf("%d starts with max_running 1", n)
	}
	if r, q, tot := f.sup.agentCounts("parent"); r != 1 || q != 2 || tot != 3 {
		t.Fatalf("counts = %d %d %d", r, q, tot)
	}
	if err := f.sup.Send(a, "go"); err != nil {
		t.Fatal(err)
	}
	got := waitNotices(t, f, "parent", 3)
	if len(got) != 3 || !strings.Contains(got[0], a) || !strings.Contains(got[1], b) || !strings.Contains(got[2], c) {
		t.Fatalf("report order = %q", got)
	}
}

func TestChildBurstStartsOnlyCap(t *testing.T) {
	t.Parallel()
	f := childFixture(t)
	var wg sync.WaitGroup
	var mu sync.Mutex
	started := 0
	for range 3 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			_, q, err := f.sup.CreateChild(CreateOptions{Prompt: "HANG", SpawnedBy: "parent"}, 0, 1)
			if err != nil {
				t.Error(err)
			}
			if !q {
				mu.Lock()
				started++
				mu.Unlock()
			}
		}()
	}
	wg.Wait()
	time.Sleep(300 * time.Millisecond)
	if started != 1 || f.startCount(t) != 1 {
		t.Fatalf("started %d (processes %d), want 1", started, f.startCount(t))
	}
}

func TestChildLimitAndDepth(t *testing.T) {
	t.Parallel()
	f := childFixture(t)
	for range 2 {
		if _, _, err := f.sup.CreateChild(CreateOptions{Prompt: "HANG", SpawnedBy: "parent"}, 2, 0); err != nil {
			t.Fatal(err)
		}
	}
	if _, _, err := f.sup.CreateChild(CreateOptions{Prompt: "x", SpawnedBy: "parent"}, 2, 0); !errors.Is(err, ErrAgentLimit) {
		t.Fatalf("third = %v, want ErrAgentLimit", err)
	}
	f.seed(t, "kid", history.Entry{Seq: 1, Kind: "meta", Data: map[string]any{"cwd": f.home, "spawned_by": "parent"}})
	if _, _, err := f.sup.CreateChild(CreateOptions{Prompt: "x", SpawnedBy: "kid"}, 0, 0); !errors.Is(err, ErrDepth) {
		t.Fatalf("grandchild = %v, want ErrDepth", err)
	}
	if _, _, err := f.sup.CreateChild(CreateOptions{Prompt: "x", SpawnedBy: "nope"}, 0, 0); !errors.Is(err, ErrUnknownSession) {
		t.Fatalf("unknown parent = %v", err)
	}
}

func TestChildAPI(t *testing.T) {
	t.Parallel()
	f := newAPI(t, envTurns+"=1")
	f.seed(t, "parent")
	f.seed(t, "other")

	code, body := f.do(t, "POST", "/api/sessions", `{"prompt":"hello","spawnedBy":"parent","maxRunning":1}`)
	if code != 201 || body["queued"] != false {
		t.Fatalf("create = %d %v", code, body)
	}
	id := rowOf(t, body)["id"].(string)
	waitNotices(t, f.fixture, "parent", 1)

	code, body = f.do(t, "GET", "/api/sessions/"+id+"/agent?parent=parent", "")
	if code != 200 || body["reply"] != "echo hello" || body["status"] != "done" || body["title"] != "hello" {
		t.Fatalf("agent = %d %v", code, body)
	}
	if code, _ := f.do(t, "GET", "/api/sessions/"+id+"/agent?parent=other", ""); code != 403 {
		t.Fatalf("foreign agent = %d", code)
	}
	if code, _ := f.do(t, "GET", "/api/sessions/nope/agent?parent=parent", ""); code != 404 {
		t.Fatalf("unknown agent = %d", code)
	}

	// A hanging child holds the only slot; the next one queues.
	_, body = f.do(t, "POST", "/api/sessions", `{"prompt":"HANG","spawnedBy":"parent","maxRunning":1}`)
	hang := rowOf(t, body)["id"].(string)
	code, body = f.do(t, "POST", "/api/sessions", `{"prompt":"later","spawnedBy":"parent","maxRunning":1}`)
	if code != 201 || body["queued"] != true || rowOf(t, body)["status"] != "queued" {
		t.Fatalf("queued create = %d %v", code, body)
	}
	queued := rowOf(t, body)["id"].(string)

	_, body = f.do(t, "GET", "/api/sessions/parent/children", "")
	kids, _ := body["children"].([]any)
	if len(kids) != 3 {
		t.Fatalf("children = %v", body)
	}
	_, body = f.do(t, "GET", "/api/sessions", "")
	var sawQueued bool
	for _, r := range body["sessions"].([]any) {
		row := r.(map[string]any)
		if row["id"] == queued && row["status"] == "queued" && row["spawnedBy"] == "parent" {
			sawQueued = true
		}
		if row["id"] == "parent" {
			ag, _ := row["agents"].(map[string]any)
			if ag["total"] != float64(3) || ag["queued"] != float64(1) {
				t.Fatalf("parent agents = %v", row["agents"])
			}
		}
	}
	if !sawQueued {
		t.Fatalf("queued child not listed: %v", body)
	}

	if code, body := f.do(t, "POST", "/api/sessions/"+queued+"/stop", `{"parent":"parent"}`); code != 200 || body["was"] != "queued" {
		t.Fatalf("stop queued = %d %v", code, body)
	}
	if code, body := f.do(t, "POST", "/api/sessions/"+id+"/stop", `{"parent":"parent"}`); code != 200 || body["was"] != "idle" {
		t.Fatalf("stop idle = %d %v", code, body)
	}

	// Archive with stopChildren interrupts the hanging child.
	if code, body := f.do(t, "POST", "/api/sessions/parent/archive", `{"stopChildren":true}`); code != 200 {
		t.Fatalf("archive = %d %v", code, body)
	}
	waitFor(t, "hanging child stopped", func() bool { return !f.sup.Live(hang) })

	if code, _ := f.do(t, "POST", "/api/sessions", `{"prompt":"x","spawnedBy":"parent","slug":"nope"}`); code != 400 {
		t.Fatalf("unknown project = %d", code)
	}
	f.seed(t, "kid", history.Entry{Seq: 1, Kind: "meta", Data: map[string]any{"cwd": f.home, "spawned_by": "parent"}})
	if code, _ := f.do(t, "POST", "/api/sessions", `{"prompt":"x","spawnedBy":"kid"}`); code != 409 {
		t.Fatalf("depth = %d", code)
	}
	if code, _ := f.do(t, "POST", "/api/sessions", `{"prompt":"x","spawnedBy":"other","maxPerSession":1}`); code != 201 {
		t.Fatalf("other first = %d", code)
	}
	if code, body := f.do(t, "POST", "/api/sessions", `{"prompt":"x","spawnedBy":"other","maxPerSession":1}`); code != 429 {
		t.Fatalf("limit = %d %v", code, body)
	}
	if code, _ := f.do(t, "POST", "/api/sessions/other/notify", `{"text":"hi"}`); code != 200 {
		t.Fatalf("notify = %d", code)
	}
}
