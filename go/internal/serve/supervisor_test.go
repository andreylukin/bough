package serve

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// The supervisor's only seam onto a real bough is Options.Exe plus
// Options.Env, so the tests point Exe at this test binary and let
// TestMain re-enter as a fake headless child. Nothing is built, no
// model is reached, and the child speaks the verified stdout contract:
// one JSON object per line, {"kind","text",...extra}.
const (
	envFake   = "BOUGH_FAKE_CHILD"
	envHist   = "BOUGH_FAKE_HIST"
	envNewID  = "BOUGH_FAKE_NEWID"
	envStarts = "BOUGH_FAKE_STARTS" // one byte appended per process start
	envAsk    = "BOUGH_FAKE_ASK"
	envNoise  = "BOUGH_FAKE_NOISE"
	envNoHist = "BOUGH_FAKE_NOHIST"
	envMetaID = "BOUGH_FAKE_METAID" // volunteer the id on the meta line
)

func TestMain(m *testing.M) {
	if os.Getenv(envFake) != "" {
		fakeChild()
		return
	}
	os.Exit(m.Run())
}

// fakeChild stands in for `bough --headless --json [-r id]`.
func fakeChild() {
	id := os.Getenv(envNewID)
	for i, a := range os.Args {
		if a == "-r" && i+1 < len(os.Args) {
			id = os.Args[i+1]
		}
	}
	if f := os.Getenv(envStarts); f != "" {
		if fh, err := os.OpenFile(f, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644); err == nil {
			fmt.Fprintln(fh, id)
			fh.Close()
		}
	}
	dir := os.Getenv(envHist)
	if dir != "" && id != "" && os.Getenv(envNoHist) == "" {
		cwd, _ := os.Getwd()
		appendEntry(filepath.Join(dir, id+".jsonl"), history.Entry{
			Seq: 1, At: time.Now(), Kind: "meta", Data: map[string]any{"cwd": cwd},
		})
	}
	meta := map[string]any{"kind": "meta", "text": "ready"}
	if os.Getenv(envMetaID) != "" {
		meta["session"] = id
	}
	say(meta)

	armed := false
	sc := bufio.NewScanner(os.Stdin)
	sc.Buffer(make([]byte, 64*1024), 16*1024*1024)
	for sc.Scan() {
		line := sc.Text()
		if armed {
			armed = false
			say(map[string]any{"kind": "assistant", "text": "answered:" + line})
			say(map[string]any{"kind": "done", "text": ""})
			continue
		}
		if os.Getenv(envNoise) != "" {
			fmt.Println("this line is not json")
		}
		say(map[string]any{"kind": "input", "text": line})
		if os.Getenv(envAsk) != "" {
			armed = true
			say(map[string]any{"kind": "ask", "text": "pick one", "id": "q1", "options": []string{"a", "b"}})
			continue
		}
		say(map[string]any{"kind": "assistant", "text": "echo " + line})
		say(map[string]any{"kind": "done", "text": ""})
	}
	os.Exit(0)
}

func say(obj map[string]any) {
	b, _ := json.Marshal(obj)
	os.Stdout.Write(append(b, '\n'))
}

func appendEntry(path string, e history.Entry) {
	os.MkdirAll(filepath.Dir(path), 0o755)
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return
	}
	defer f.Close()
	b, _ := json.Marshal(e)
	f.Write(append(b, '\n'))
}

// fixture is one hermetic supervisor: its own HOME, its own history
// directory, and a fake child.
type fixture struct {
	sup    *Supervisor
	home   string
	hist   string
	starts string
}

func newFixture(t *testing.T, extraEnv ...string) *fixture {
	t.Helper()
	home := t.TempDir()
	hist := filepath.Join(home, ".bough", "history")
	if err := os.MkdirAll(hist, 0o755); err != nil {
		t.Fatal(err)
	}
	starts := filepath.Join(home, "starts")
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	env := append([]string{
		envFake + "=1",
		envHist + "=" + hist,
		envStarts + "=" + starts,
		"HOME=" + home,
		"PATH=" + os.Getenv("PATH"),
	}, extraEnv...)
	sup, err := NewSupervisor(Options{
		Exe:      exe,
		HistDir:  hist,
		MetaPath: filepath.Join(home, ".bough", "serve", "meta.json"),
		Env:      env,
		Buffer:   50,
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { sup.Close() })
	return &fixture{sup: sup, home: home, hist: hist, starts: starts}
}

// seed writes a session file so Adopt has something to resume.
func (f *fixture) seed(t *testing.T, id string, es ...history.Entry) {
	t.Helper()
	path := filepath.Join(f.hist, id+".jsonl")
	if len(es) == 0 {
		es = []history.Entry{{Seq: 1, At: time.Now(), Kind: "meta", Data: map[string]any{"cwd": f.home}}}
	}
	for _, e := range es {
		appendEntry(path, e)
	}
}

func (f *fixture) startCount(t *testing.T) int {
	t.Helper()
	b, err := os.ReadFile(f.starts)
	if err != nil {
		return 0
	}
	return len(strings.Fields(string(b)))
}

// waitFor polls cond for up to a second. Polling, never sleeping on a
// fixed delay: a child's first line arrives when it arrives.
func waitFor(t *testing.T, what string, cond func() bool) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		if cond() {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s", what)
}

func kinds(evs []Event) []string {
	out := make([]string, len(evs))
	for i, e := range evs {
		out[i] = e.Kind
	}
	return out
}

func hasKind(evs []Event, kind string) bool {
	for _, e := range evs {
		if e.Kind == kind {
			return true
		}
	}
	return false
}

func TestSupervisorCreateAndPrompt(t *testing.T) {
	t.Parallel()
	f := newFixture(t, envNewID+"=sess-create")

	id, err := f.sup.Create(f.home, "hello")
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	if id != "sess-create" {
		t.Fatalf("id = %q, want sess-create", id)
	}
	if !f.sup.Live(id) {
		t.Error("session is not live after Create")
	}
	waitFor(t, "the prompt to round-trip", func() bool {
		return hasKind(f.sup.Recent(id), "done")
	})
	evs := f.sup.Recent(id)
	if evs[0].Kind != "meta" || evs[0].Seq != 1 {
		t.Errorf("first event = %+v, want meta seq 1", evs[0])
	}
	var echoed bool
	for _, e := range evs {
		if e.Kind == "assistant" && e.Text == "echo hello" {
			echoed = true
		}
	}
	if !echoed {
		t.Errorf("prompt never reached the child; events %v", kinds(evs))
	}
	// Seqs are per session and monotonic from 1.
	for i, e := range evs {
		if e.Seq != int64(i+1) {
			t.Fatalf("event %d has seq %d", i, e.Seq)
		}
	}
}

func TestSupervisorCreatePrefersMetaID(t *testing.T) {
	t.Parallel()
	f := newFixture(t, envNewID+"=sess-meta", envMetaID+"=1")
	id, err := f.sup.Create(f.home, "")
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	if id != "sess-meta" {
		t.Fatalf("id = %q, want sess-meta", id)
	}
	// The events emitted before the id resolved must not be lost.
	waitFor(t, "the buffered meta event", func() bool {
		return hasKind(f.sup.Recent(id), "meta")
	})
}

// The one-writer rule: history.ConcurrentWriter exists because two
// processes appending one session file fork the session.
func TestSupervisorAdoptStartsOnlyOneChild(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.seed(t, "sess-adopt")

	var wg sync.WaitGroup
	errs := make([]error, 8)
	for i := range errs {
		wg.Add(1)
		go func() {
			defer wg.Done()
			errs[i] = f.sup.Adopt("sess-adopt")
		}()
	}
	wg.Wait()
	for i, err := range errs {
		if err != nil {
			t.Fatalf("Adopt %d: %v", i, err)
		}
	}
	if !f.sup.Live("sess-adopt") {
		t.Fatal("session is not live after Adopt")
	}
	waitFor(t, "the child to start", func() bool { return f.startCount(t) >= 1 })
	// Give a would-be second child time to record itself.
	time.Sleep(100 * time.Millisecond)
	if n := f.startCount(t); n != 1 {
		t.Fatalf("started %d children for one session, want 1", n)
	}

	// A Send on an already-leased session reuses the same child.
	if err := f.sup.Send("sess-adopt", "again"); err != nil {
		t.Fatalf("Send: %v", err)
	}
	waitFor(t, "the second prompt", func() bool {
		return hasKind(f.sup.Recent("sess-adopt"), "done")
	})
	if n := f.startCount(t); n != 1 {
		t.Fatalf("Send spawned a second child (%d starts)", n)
	}
}

func TestSupervisorSendAdoptsAndAskBlocksSend(t *testing.T) {
	t.Parallel()
	f := newFixture(t, envAsk+"=1")
	f.seed(t, "sess-ask")

	if err := f.sup.Send("sess-ask", "question time"); err != nil {
		t.Fatalf("Send: %v", err)
	}
	waitFor(t, "the ask", func() bool { return f.sup.PendingAsk("sess-ask") != nil })

	ask := f.sup.PendingAsk("sess-ask")
	if ask.ID != "q1" || ask.Text != "pick one" || len(ask.Options) != 2 {
		t.Fatalf("ask = %+v", ask)
	}
	// A prompt now would be eaten as the answer, so it must be refused.
	err := f.sup.Send("sess-ask", "unrelated")
	if err == nil || !strings.Contains(err.Error(), "pending ask") {
		t.Fatalf("Send with an armed ask = %v, want a pending-ask error", err)
	}

	if err := f.sup.Answer("sess-ask", "a"); err != nil {
		t.Fatalf("Answer: %v", err)
	}
	waitFor(t, "the answer to land", func() bool {
		for _, e := range f.sup.Recent("sess-ask") {
			if e.Kind == "assistant" && e.Text == "answered:a" {
				return true
			}
		}
		return false
	})
	if a := f.sup.PendingAsk("sess-ask"); a != nil {
		t.Errorf("ask still pending after the answer: %+v", a)
	}
	if err := f.sup.Answer("sess-ask", "a"); err != ErrNoAsk {
		t.Errorf("second Answer = %v, want ErrNoAsk", err)
	}
}

func TestSupervisorNonJSONStdoutIsSurfaced(t *testing.T) {
	t.Parallel()
	f := newFixture(t, envNoise+"=1")
	f.seed(t, "sess-noise")

	if err := f.sup.Send("sess-noise", "go"); err != nil {
		t.Fatalf("Send: %v", err)
	}
	waitFor(t, "the non-JSON line", func() bool {
		for _, e := range f.sup.Recent("sess-noise") {
			if e.Kind == "stdout" && e.Text == "this line is not json" {
				return true
			}
		}
		return false
	})
}

func TestSupervisorKillDropsTheLease(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.seed(t, "sess-kill")

	if err := f.sup.Adopt("sess-kill"); err != nil {
		t.Fatalf("Adopt: %v", err)
	}
	waitFor(t, "the child", func() bool { return f.startCount(t) == 1 })

	if err := f.sup.Kill("sess-kill"); err != nil {
		t.Fatalf("Kill: %v", err)
	}
	if f.sup.Live("sess-kill") {
		t.Error("lease still held after Kill")
	}
	if !hasKind(f.sup.Recent("sess-kill"), "exit") {
		t.Errorf("no exit event; got %v", kinds(f.sup.Recent("sess-kill")))
	}
	// A dropped lease means the session can be run again.
	if err := f.sup.Adopt("sess-kill"); err != nil {
		t.Fatalf("re-Adopt: %v", err)
	}
	waitFor(t, "the replacement child", func() bool { return f.startCount(t) == 2 })

	// Kill on a session with no child is not an error.
	if err := f.sup.Kill("sess-nobody"); err != nil {
		t.Errorf("Kill of an unrun session = %v, want nil", err)
	}
}

func TestSupervisorInterruptKeepsTheLease(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.seed(t, "sess-int")
	if err := f.sup.Adopt("sess-int"); err != nil {
		t.Fatalf("Adopt: %v", err)
	}
	waitFor(t, "the child", func() bool { return f.startCount(t) == 1 })
	if err := f.sup.Interrupt("sess-int"); err != nil {
		t.Fatalf("Interrupt: %v", err)
	}
	if err := f.sup.Interrupt("sess-unknown"); err == nil {
		t.Error("Interrupt of an unknown session = nil, want ErrUnknownSession")
	}
}

func TestSupervisorArchiveKillsAndPersists(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.seed(t, "sess-arch")
	if err := f.sup.Adopt("sess-arch"); err != nil {
		t.Fatalf("Adopt: %v", err)
	}
	waitFor(t, "the child", func() bool { return f.startCount(t) == 1 })

	if err := f.sup.SetArchived("sess-arch", true); err != nil {
		t.Fatalf("SetArchived: %v", err)
	}
	if f.sup.Live("sess-arch") {
		t.Error("archiving left the child running")
	}
	if !f.sup.Meta("sess-arch").Archived {
		t.Error("meta does not say archived")
	}
	if err := f.sup.Adopt("sess-arch"); err == nil {
		t.Error("adopted an archived session")
	}
	if err := f.sup.SetTitle("sess-arch", "renamed"); err != nil {
		t.Fatalf("SetTitle: %v", err)
	}

	// meta.json is the store; a fresh supervisor must read it back.
	again, err := NewSupervisor(Options{Exe: "/bin/true", HistDir: f.hist, MetaPath: filepath.Join(f.home, ".bough", "serve", "meta.json")})
	if err != nil {
		t.Fatalf("NewSupervisor: %v", err)
	}
	defer again.Close()
	got := again.Meta("sess-arch")
	if !got.Archived || got.Title != "renamed" {
		t.Errorf("reloaded meta = %+v", got)
	}
	if all := again.AllMeta(); len(all) != 1 {
		t.Errorf("AllMeta = %v", all)
	}

	if err := f.sup.SetArchived("sess-arch", false); err != nil {
		t.Fatalf("unarchive: %v", err)
	}
	if f.sup.Meta("sess-arch").Archived {
		t.Error("unarchive did not clear the flag")
	}
	if f.sup.Live("sess-arch") {
		t.Error("unarchive respawned a child")
	}
}

func TestSupervisorMissingMetaFileIsEmpty(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	sup, err := NewSupervisor(Options{
		Exe:      "/bin/true",
		HistDir:  filepath.Join(home, ".bough", "history"),
		MetaPath: filepath.Join(home, ".bough", "serve", "meta.json"),
	})
	if err != nil {
		t.Fatalf("NewSupervisor: %v", err)
	}
	defer sup.Close()
	if len(sup.AllMeta()) != 0 {
		t.Errorf("AllMeta = %v, want empty", sup.AllMeta())
	}
	if sup.Live("nope") || sup.PendingAsk("nope") != nil || sup.Recent("nope") != nil {
		t.Error("an unknown session reads as live/asking/eventful")
	}
}

func TestSupervisorEntriesAndList(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.seed(t, "sess-read",
		history.Entry{Seq: 1, At: time.Now(), Kind: "meta", Data: map[string]any{"cwd": f.home}},
		history.Entry{Seq: 2, At: time.Now(), Kind: "input", Data: map[string]any{"text": "hi"}},
	)
	es, err := f.sup.Entries("sess-read")
	if err != nil || len(es) != 2 {
		t.Fatalf("Entries = %v, %v", es, err)
	}
	if _, err := f.sup.Entries("nope"); err == nil {
		t.Error("Entries of an unknown session = nil error")
	}
	infos, err := f.sup.List()
	if err != nil || len(infos) != 1 || infos[0].ID != "sess-read" {
		t.Fatalf("List = %+v, %v", infos, err)
	}
	if f.sup.HistDir() != f.hist {
		t.Errorf("HistDir = %q", f.sup.HistDir())
	}
}

func TestSupervisorSubscribeAndDrop(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.seed(t, "sess-sub")

	ch, unsub := f.sup.Subscribe("sess-sub")
	if err := f.sup.Send("sess-sub", "hey"); err != nil {
		t.Fatalf("Send: %v", err)
	}
	select {
	case ev, ok := <-ch:
		if !ok {
			t.Fatal("subscription closed early")
		}
		if ev.Session != "sess-sub" {
			t.Errorf("event = %+v", ev)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("no event reached the subscriber")
	}
	unsub()
	unsub() // idempotent: a second close would panic
	waitFor(t, "the subscriber to be unregistered", func() bool {
		f.sup.mu.Lock()
		defer f.sup.mu.Unlock()
		return len(f.sup.subs["sess-sub"]) == 0
	})
	// A gone subscriber must not wedge the session.
	if err := f.sup.Send("sess-sub", "still here"); err != nil {
		t.Fatalf("Send after unsubscribe: %v", err)
	}
	waitFor(t, "the session to keep running", func() bool {
		for _, e := range f.sup.Recent("sess-sub") {
			if e.Kind == "assistant" && e.Text == "echo still here" {
				return true
			}
		}
		return false
	})
}

// A subscriber that stops reading is dropped, never waited for: one
// stalled SSE client must not stall every other reader of the session.
func TestSupervisorSlowSubscriberIsDropped(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	ch, unsub := f.sup.Subscribe("sess-slow")
	defer unsub()

	fake := &child{id: "sess-slow", done: make(chan struct{})}
	for i := 0; i < subBuffer+5; i++ {
		f.sup.emit(fake, "assistant", fmt.Sprintf("line %d", i), nil)
	}
	drained := 0
	for range ch {
		drained++
	}
	if drained > subBuffer {
		t.Fatalf("drained %d events, want at most the %d-deep buffer", drained, subBuffer)
	}
	// The ring still holds the tail, so a reconnect catches up.
	if n := len(f.sup.Recent("sess-slow")); n != 50 {
		t.Errorf("ring = %d events, want the configured 50", n)
	}
}

func TestSupervisorCloseKillsEveryChild(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.seed(t, "sess-a")
	f.seed(t, "sess-b")
	for _, id := range []string{"sess-a", "sess-b"} {
		if err := f.sup.Adopt(id); err != nil {
			t.Fatalf("Adopt %s: %v", id, err)
		}
	}
	waitFor(t, "both children", func() bool { return f.startCount(t) == 2 })
	ch, _ := f.sup.Subscribe("sess-a")

	if err := f.sup.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if f.sup.Live("sess-a") || f.sup.Live("sess-b") {
		t.Error("a lease survived Close")
	}
	for range ch { // must terminate: Close closes every subscriber
	}
	if err := f.sup.Adopt("sess-a"); err == nil {
		t.Error("adopted a session on a closed supervisor")
	}
}
