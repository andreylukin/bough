package serve

import (
	"errors"
	"net/http"
	"path/filepath"
	"slices"
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
		{"FAIL now", "failed", "Background agent failed: boom\necho FAIL now"},
		{"CANCEL now", "stopped", ""},
		{"EXIT now", "stopped", ""},
		// The done event arrives before the entries: the report waits.
		{"RACE now", "finished", "echo RACE now"},
		// A turn that closes with calls adopted is not the end: the
		// report is the wake turn's, once.
		{"ADOPT now", "finished", "echo ADOPT now"},
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

// The child starts on the model its parent asked for, and a failure
// notice carries the child's first error line, cleaned.
func TestChildModelAndFailReason(t *testing.T) {
	t.Parallel()
	f := childFixture(t)
	id, _, err := f.sup.CreateChild(CreateOptions{Prompt: "FAIL now", SpawnedBy: "parent", Model: "llm-openrouter/z-ai/glm-5"}, 0, 0)
	if err != nil {
		t.Fatal(err)
	}
	got := waitNotices(t, f, "parent", 1)
	want := "[agent FAIL now · " + id + " failed] Background agent failed: boom\necho FAIL now"
	if len(got) != 1 || got[0] != want {
		t.Fatalf("notices = %q, want [%q]", got, want)
	}
	es, _ := history.ReadFile(filepath.Join(f.hist, id+".jsonl"))
	if args, _ := es[0].Data["args"].(string); !strings.Contains(args, "--set llm.plugin=llm-openrouter --set llm.model=z-ai/glm-5") {
		t.Fatalf("child args = %q", args)
	}
	if c := f.sup.Children("parent"); len(c) != 1 || c[0].Error != "boom" {
		t.Fatalf("children = %+v", c)
	}
}

func TestFailReason(t *testing.T) {
	for in, want := range map[string]string{
		"GoError: llm-anthropic: 401 Unauthorized\nbody":    "llm-anthropic: 401 Unauthorized",
		"read /private/tmp/claude-501/x/y.go: no such file": "read y.go: no such file",
		"  \n": "",
	} {
		if got := failReason(in); got != want {
			t.Errorf("failReason(%q) = %q, want %q", in, got, want)
		}
	}
}

// A finished thread reports to the project's main thread. That is the
// whole reason a web-created project session is parented: before it
// was, childEventLocked returned early and the notice went nowhere.
func TestProjectThreadReportsToMain(t *testing.T) {
	t.Parallel()
	f := newAPI(t, envTurns+"=1")
	slug := mkProject(t, f, "Reporting")
	main, err := f.sup.Main(slug)
	if err != nil {
		t.Fatal(err)
	}
	// Stopped, so the notice takes the stored-entry path: main is idle
	// far more often than it is live, and that is the path that has to
	// survive a serve restart.
	if err := f.sup.Kill(main); err != nil {
		t.Fatal(err)
	}
	code, body := f.do(t, "POST", "/api/sessions", `{"mode":"project","project":"`+slug+`","prompt":"ship it"}`)
	if code != http.StatusCreated {
		t.Fatalf("create thread = %d %v", code, body)
	}
	thread, _ := rowOf(t, body)["id"].(string)
	if f.sup.Meta(thread).SpawnedBy != main {
		t.Fatalf("thread spawnedBy = %q, want main %q", f.sup.Meta(thread).SpawnedBy, main)
	}
	got := waitNotices(t, f.fixture, main, 1)
	want := "[agent ship it · " + thread + " finished] echo ship it"
	if len(got) != 1 || got[0] != want {
		t.Fatalf("main's notices = %q, want [%q]", got, want)
	}
	if f.sup.Live(main) {
		t.Error("the report restarted main")
	}
}

// A thread a person started from the project page is parented to main
// only so its report lands there: it may start background agents of its
// own, while those, and the threads main's model starts, stay at depth 1.
// Refusing the person's thread left every web thread unable to delegate.
func TestProjectThreadIsATopLevelAgent(t *testing.T) {
	t.Parallel()
	f := newAPI(t, envTurns+"=1")
	slug := mkProject(t, f, "Delegating")
	main, err := f.sup.Main(slug)
	if err != nil {
		t.Fatal(err)
	}
	code, body := f.do(t, "POST", "/api/sessions", `{"mode":"project","project":"`+slug+`","prompt":"HANG"}`)
	if code != http.StatusCreated {
		t.Fatalf("create thread = %d %v", code, body)
	}
	thread, _ := rowOf(t, body)["id"].(string)
	if m := f.sup.Meta(thread); m.SpawnedBy != main || !m.Thread {
		t.Fatalf("thread meta = %+v, want spawnedBy %q and thread", m, main)
	}
	if env := f.sup.projectEnv(thread); !slices.Contains(env, "BOUGH_PROJECT_THREAD=1") {
		t.Errorf("a restarted thread starts with %v, want BOUGH_PROJECT_THREAD", env)
	}
	waitFor(t, "the thread's history", func() bool { _, ok := f.sup.infoOf(thread); return ok })
	kid, _, err := f.sup.CreateChild(CreateOptions{Prompt: "HANG", SpawnedBy: thread}, 0, 0)
	if err != nil {
		t.Fatalf("the person's thread could not start an agent: %v", err)
	}
	if m := f.sup.Meta(kid); m.SpawnedBy != thread || m.Thread {
		t.Fatalf("agent meta = %+v, want an agent of %q", m, thread)
	}
	waitFor(t, "the agent's history", func() bool { _, ok := f.sup.infoOf(kid); return ok })
	if _, _, err := f.sup.CreateChild(CreateOptions{Prompt: "x", SpawnedBy: kid}, 0, 0); !errors.Is(err, ErrDepth) {
		t.Fatalf("the thread's agent started one = %v, want ErrDepth", err)
	}
	byMain, _, err := f.sup.CreateChild(CreateOptions{Prompt: "HANG", SpawnedBy: main}, 0, 0)
	if err != nil {
		t.Fatal(err)
	}
	waitFor(t, "main's agent's history", func() bool { _, ok := f.sup.infoOf(byMain); return ok })
	if _, _, err := f.sup.CreateChild(CreateOptions{Prompt: "x", SpawnedBy: byMain}, 0, 0); !errors.Is(err, ErrDepth) {
		t.Fatalf("a thread main's model started started one = %v, want ErrDepth", err)
	}
}

// A project's main thread lives as long as the project, so its lifetime
// tally would eventually refuse every thread. Only the ones still going
// count against its budget.
func TestMainThreadBudgetCountsOnlyLiveThreads(t *testing.T) {
	t.Parallel()
	f := newAPI(t, envTurns+"=1")
	slug := mkProject(t, f, "Busy")
	main, err := f.sup.Main(slug)
	if err != nil {
		t.Fatal(err)
	}
	// Two threads that finish, then two that hang: a plain parent would
	// be at four of a budget of two.
	for range 2 {
		id, _, err := f.sup.CreateChild(CreateOptions{Prompt: "done now", SpawnedBy: main}, 2, 0)
		if err != nil {
			t.Fatal(err)
		}
		waitFor(t, "the thread to finish", func() bool {
			st, _ := StatusOf(mustEntries(t, f.fixture, id), f.sup.Live(id))
			return st == StatusDone
		})
	}
	for range 2 {
		id, _, err := f.sup.CreateChild(CreateOptions{Prompt: "HANG", SpawnedBy: main}, 2, 0)
		if err != nil {
			t.Fatalf("a finished thread did not give its slot back: %v", err)
		}
		waitFor(t, "the thread to start its turn", func() bool {
			st, _ := StatusOf(mustEntries(t, f.fixture, id), f.sup.Live(id))
			return st == StatusRunning
		})
	}
	// Two are still going, so the budget is spent.
	if _, _, err := f.sup.CreateChild(CreateOptions{Prompt: "HANG", SpawnedBy: main}, 2, 0); !errors.Is(err, ErrAgentLimit) {
		t.Fatalf("fifth = %v, want ErrAgentLimit", err)
	}
	// A parent that is not a main still counts every child it ever
	// started: an ordinary session's budget is a lifetime one.
	f.seed(t, "plain")
	for range 2 {
		if _, _, err := f.sup.CreateChild(CreateOptions{Prompt: "done now", SpawnedBy: "plain"}, 2, 0); err != nil {
			t.Fatal(err)
		}
	}
	if _, _, err := f.sup.CreateChild(CreateOptions{Prompt: "x", SpawnedBy: "plain"}, 2, 0); !errors.Is(err, ErrAgentLimit) {
		t.Fatalf("plain parent's third = %v, want ErrAgentLimit", err)
	}
}

func mustEntries(t *testing.T, f *fixture, id string) []history.Entry {
	t.Helper()
	es, _ := history.ReadFile(filepath.Join(f.hist, id+".jsonl"))
	return es
}

// A thread nobody has messaged yet holds no running slot. It opens no
// turn, so it emits none of the done/cancelled/exit events that give a
// slot back: sixteen empty threads from the project page's "New thread"
// used to wedge every spawn until serve restarted.
func TestAnUnpromptedChildHoldsNoRunningSlot(t *testing.T) {
	t.Parallel()
	f := childFixture(t)
	idle, queued, err := f.sup.CreateChild(CreateOptions{SpawnedBy: "parent"}, 0, 1)
	if err != nil || queued {
		t.Fatalf("CreateChild = %q %v %v", idle, queued, err)
	}
	waitFor(t, "the idle thread to give its slot back", func() bool {
		f.sup.mu.Lock()
		defer f.sup.mu.Unlock()
		return !f.sup.running[idle]
	})
	if f.sup.Live(idle) != true {
		t.Fatal("the idle thread is not running; the slot proves nothing")
	}
	// maxRunning is 1: the next thread still starts rather than queueing
	// behind a session that will never finish.
	next, queued, err := f.sup.CreateChild(CreateOptions{Prompt: "hello", SpawnedBy: "parent"}, 0, 1)
	if err != nil {
		t.Fatalf("CreateChild: %v", err)
	}
	if queued {
		t.Fatalf("thread %s queued behind an idle one", next)
	}
}

// A finished background agent messaged again is running again: it holds
// a running slot until that turn closes, so the parent's count shows it
// and the Work dialog's Stop interrupts it. The real headless child
// never prints "input", which the slot used to wait for: the re-messaged
// agent ran unseen and Stop answered "idle" without stopping it.
func TestAMessagedChildHoldsARunningSlot(t *testing.T) {
	t.Parallel()
	f := newFixture(t, envTurns+"=1", envNoInput+"=1")
	f.seed(t, "parent")
	id, queued, err := f.sup.CreateChild(CreateOptions{Prompt: "hello", SpawnedBy: "parent"}, 0, 1)
	if err != nil || queued {
		t.Fatalf("CreateChild = %q %v %v", id, queued, err)
	}
	waitNotices(t, f, "parent", 1)
	if r, _, _ := f.sup.agentCounts("parent"); r != 0 {
		t.Fatalf("running = %d after the first turn closed, want 0", r)
	}
	if err := f.sup.Send(id, "HANG again"); err != nil {
		t.Fatal(err)
	}
	waitFor(t, "the second turn to open", func() bool {
		st, _ := StatusOf(mustEntries(t, f, id), f.sup.Live(id))
		return st == StatusRunning
	})
	if r, _, _ := f.sup.agentCounts("parent"); r != 1 {
		t.Fatalf("running = %d with the messaged agent mid-turn, want 1", r)
	}
	if was, err := f.sup.stopChild(id); err != nil || was != "running" {
		t.Fatalf("stopChild = %q %v, want running", was, err)
	}
}

// An agent whose turn closed with its calls adopted as jobs is still
// working: serve keeps its running slot, so its row says running (the
// Work dialog offers Stop) rather than done while the parent's count
// says one is running.
func TestAWaitingChildRowSaysRunning(t *testing.T) {
	t.Parallel()
	f := newAPI(t, envTurns+"=1")
	f.seed(t, "parent")
	_, body := f.do(t, "POST", "/api/sessions", `{"prompt":"WAITJOB","spawnedBy":"parent","maxRunning":1}`)
	id := rowOf(t, body)["id"].(string)
	waitFor(t, "the interim close", func() bool {
		es := mustEntries(t, f.fixture, id)
		return len(es) > 0 && es[len(es)-1].Kind == "done"
	})
	time.Sleep(200 * time.Millisecond) // the done event reaches serve
	if r, _, _ := f.sup.agentCounts("parent"); r != 1 {
		t.Fatalf("running = %d while the agent waits on its job, want 1", r)
	}
	_, body = f.do(t, "GET", "/api/sessions/parent/children", "")
	kids, _ := body["children"].([]any)
	if len(kids) != 1 || kids[0].(map[string]any)["status"] != "running" {
		t.Fatalf("children = %v, want the waiting agent running", kids)
	}
}

// Stop on an agent waiting on its job ends it: the process exits and
// the job with it, so its transcript and its report say stopped. They
// said finished, with the interim reply, as if nothing was cut short.
func TestStopAWaitingChildSaysStopped(t *testing.T) {
	t.Parallel()
	f := childFixture(t)
	id, _, err := f.sup.CreateChild(CreateOptions{Prompt: "WAITJOB", SpawnedBy: "parent"}, 0, 1)
	if err != nil {
		t.Fatal(err)
	}
	waitFor(t, "the interim close", func() bool {
		es := mustEntries(t, f, id)
		return len(es) > 0 && es[len(es)-1].Kind == "done"
	})
	time.Sleep(200 * time.Millisecond) // the done event reaches serve
	if was, err := f.sup.stopChild(id); err != nil || was != "running" {
		t.Fatalf("stopChild = %q %v, want running", was, err)
	}
	got := waitNotices(t, f, "parent", 1)
	if len(got) != 1 || !strings.Contains(got[0], " stopped]") {
		t.Fatalf("notices = %q, want one stopped", got)
	}
	if st, _ := StatusOf(mustEntries(t, f, id), f.sup.Live(id)); st != StatusStopped {
		t.Fatalf("status = %s, want stopped", st)
	}
}

// A waiting agent messaged again answers, and the parent is told, but
// the job its earlier turn adopted still runs: the agent keeps its
// running slot, so the queue does not start another past the cap.
func TestAChildCloseBesideAJobKeepsItsSlot(t *testing.T) {
	t.Parallel()
	f := childFixture(t)
	id, _, err := f.sup.CreateChild(CreateOptions{Prompt: "WAITJOB", SpawnedBy: "parent"}, 0, 1)
	if err != nil {
		t.Fatal(err)
	}
	waitFor(t, "the interim close", func() bool {
		es := mustEntries(t, f, id)
		return len(es) > 0 && es[len(es)-1].Kind == "done"
	})
	later, queued, err := f.sup.CreateChild(CreateOptions{Prompt: "later", SpawnedBy: "parent"}, 0, 1)
	if err != nil || !queued {
		t.Fatalf("second CreateChild = %q %v %v, want queued", later, queued, err)
	}
	if err := f.sup.Send(id, "BESIDE"); err != nil {
		t.Fatal(err)
	}
	if got := waitNotices(t, f, "parent", 1); len(got) != 1 || !strings.Contains(got[0], "finished] echo BESIDE") {
		t.Fatalf("notices = %q, want the BESIDE reply once", got)
	}
	if r, q, _ := f.sup.agentCounts("parent"); r != 1 || q != 1 {
		t.Fatalf("running, queued = %d, %d beside the job, want 1, 1", r, q)
	}
}
