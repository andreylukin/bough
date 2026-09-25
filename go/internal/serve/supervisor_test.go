package serve

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/container"
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
	// envSlowMeta creates the history file empty and writes its meta a
	// moment later, the gap the real child has.
	envSlowMeta = "BOUGH_FAKE_SLOWMETA"
	// envTurns makes the fake record input/assistant/done in its history
	// file like the real loop, so background-agent reports can read them.
	envTurns = "BOUGH_FAKE_TURNS"
	// envSlowSig boots slowly before catching SIGINT and takes a moment
	// to read each line, as the real child does: an interrupt in either
	// gap cancels nothing (or kills it) unless serve holds it back.
	envSlowSig = "BOUGH_FAKE_SLOWSIG"
	// envNoInput makes the slow-signal fake (and envTurns' turns) behave
	// like the real headless child, which never prints an "input" line:
	// the first sign a prompt became a turn is its streamed output.
	envNoInput = "BOUGH_FAKE_NOINPUT"
	// envThinking makes its first output the engine's "model is
	// thinking" activity line, which is all the real child prints while
	// its first model request is in flight.
	envThinking = "BOUGH_FAKE_THINKING"
	// envLinger makes the slow-signal fake outlive its cancelled turn
	// the way the real child does (main.go: AwaitCancelled, then the
	// unmount): a line read in that gap still opens a turn, which the
	// exit then cancels.
	envLinger = "BOUGH_FAKE_LINGER"
	// envHangBoot names a file: while it exists, a starting fake parks
	// before it writes any history, the way a hung init or an orb that
	// never comes up leaves a real child.
	envHangBoot = "BOUGH_FAKE_HANGBOOT"
)

func TestMain(m *testing.M) {
	if os.Getenv(envFake) != "" && os.Getenv(envSlowSig) != "" {
		fakeSlowSigChild()
		return
	}
	if os.Getenv(envFake) != "" {
		fakeChild()
		return
	}
	os.Exit(m.Run())
}

// fakeChild stands in for `bough --headless --json [-r id]`.
func fakeChild() {
	id := os.Getenv(envNewID)
	if v := os.Getenv("BOUGH_SESSION_ID"); v != "" {
		id = v
	}
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
	if p := os.Getenv(envHangBoot); p != "" {
		for {
			if _, err := os.Stat(p); err != nil {
				break
			}
			time.Sleep(10 * time.Millisecond)
		}
	}
	dir := os.Getenv(envHist)
	if dir != "" && id != "" && os.Getenv(envNoHist) == "" {
		if os.Getenv(envSlowMeta) != "" {
			// As history.Open does: the file exists before its meta,
			// which waits on git (repoInfo).
			appendEntryRaw(filepath.Join(dir, id+".jsonl"), nil)
			time.Sleep(300 * time.Millisecond)
		}
		cwd, _ := os.Getwd()
		appendEntry(filepath.Join(dir, id+".jsonl"), history.Entry{
			Seq: 1, At: time.Now(), Kind: "meta", Data: map[string]any{"cwd": cwd, "origin": os.Getenv("BOUGH_ORIGIN"), "mode": os.Getenv("BOUGH_MODE"), "project": os.Getenv("BOUGH_PROJECT"), "project_dir": os.Getenv("BOUGH_PROJECT_DIR"), "spawned_by": os.Getenv("BOUGH_SPAWNED_BY"), "project_main": os.Getenv("BOUGH_PROJECT_MAIN"), "args": strings.Join(os.Args[1:], " ")},
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
		if armed && os.Getenv("BOUGH_TYPED_ANSWERS") == "" {
			armed = false
			say(map[string]any{"kind": "assistant", "text": "answered-untyped:" + line})
			say(map[string]any{"kind": "done", "text": ""})
			continue
		}
		if armed {
			// As the real child under serve: an answer names its
			// question and goes to it or nowhere; any other line is a
			// prompt, which steers the turn the question holds open.
			var ta struct {
				Answer *string `json:"answer"`
				Ask    string  `json:"ask"`
			}
			if json.Unmarshal([]byte(line), &ta) == nil && ta.Answer != nil {
				if ta.Ask == "q1" {
					armed = false
					say(map[string]any{"kind": "assistant", "text": "answered:" + *ta.Answer})
					say(map[string]any{"kind": "done", "text": ""})
				}
				continue
			}
			say(map[string]any{"kind": "steer", "text": line})
			continue
		}
		if os.Getenv(envTurns) != "" && dir != "" && id != "" {
			fakeTurn(filepath.Join(dir, id+".jsonl"), line)
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

// fakeSlowSigChild mirrors the real headless child's SIGINT handling:
// the default disposition until boot installs a handler, then a SIGINT
// with no turn read exits quietly, and one mid-turn records cancelled.
func fakeSlowSigChild() {
	time.Sleep(300 * time.Millisecond)
	sig := make(chan os.Signal, 1)
	signal.Notify(sig, os.Interrupt)
	say(map[string]any{"kind": "meta", "text": "ready"})
	lines := make(chan string)
	go func() {
		sc := bufio.NewScanner(os.Stdin)
		for sc.Scan() {
			lines <- sc.Text()
		}
	}()
	for {
		select {
		case <-sig:
			os.Exit(0) // idle: nothing to cancel
		case line := <-lines:
			time.Sleep(150 * time.Millisecond) // the gap before the loop takes it
			select {
			case <-sig:
				os.Exit(0) // not yet a turn: the prompt is dropped, nothing recorded
			default:
			}
			if os.Getenv(envThinking) != "" {
				say(map[string]any{"kind": "activity", "text": "model is thinking"})
			} else if os.Getenv(envNoInput) != "" {
				say(map[string]any{"kind": "assistant-delta", "text": "once"})
			} else {
				say(map[string]any{"kind": "input", "text": line})
			}
			select {
			case <-sig:
				say(map[string]any{"kind": "cancelled"})
				say(map[string]any{"kind": "done"})
				if os.Getenv(envLinger) != "" {
					select {
					case line := <-lines:
						say(map[string]any{"kind": "input", "text": line})
						say(map[string]any{"kind": "cancelled"})
						say(map[string]any{"kind": "done"})
					case <-time.After(time.Second):
					}
				}
				os.Exit(130)
			case <-lines: // a steer lands only at the next boundary, none yet
				<-sig
				say(map[string]any{"kind": "cancelled"})
				say(map[string]any{"kind": "done"})
				os.Exit(130)
			case <-time.After(2 * time.Second):
				say(map[string]any{"kind": "assistant", "text": "echo " + line})
				say(map[string]any{"kind": "done"})
			}
		}
	}
}

// fakeTurn records one turn the way the loop does. The line picks the
// shape: HANG leaves it open, EXIT dies mid-turn, CANCEL is interrupted,
// FAIL records an error, RACE says done before the entries reach disk.
func fakeTurn(path, line string) {
	rec := func(kind string, data map[string]any) {
		appendEntry(path, history.Entry{Seq: nextSeq(path), At: time.Now(), Kind: kind, Data: data})
	}
	rec("input", map[string]any{"text": line})
	if os.Getenv(envNoInput) == "" {
		say(map[string]any{"kind": "input", "text": line})
	}
	switch {
	case strings.HasPrefix(line, "HANG"):
	case strings.HasPrefix(line, "EXIT"):
		os.Exit(0)
	case strings.HasPrefix(line, "CANCEL"):
		rec("cancelled", nil)
		say(map[string]any{"kind": "cancelled"})
		rec("done", nil)
		say(map[string]any{"kind": "done"})
	case strings.HasPrefix(line, "ADOPT"):
		// The engine's shape: the turn closes with a call still running,
		// and the call's end wakes a turn of its own.
		rec("assistant", map[string]any{"text": "started the build"})
		rec("done", map[string]any{"running": 1})
		say(map[string]any{"kind": "done", "running": 1})
		time.Sleep(400 * time.Millisecond)
		rec("input", map[string]any{"text": "[background] job 1000 finished: make", "wake": true})
		say(map[string]any{"kind": "input", "text": "[background] job 1000 finished: make"})
		rec("assistant", map[string]any{"text": "echo " + line})
		rec("done", map[string]any{"wake": true})
		say(map[string]any{"kind": "done", "wake": true})
	case strings.HasPrefix(line, "WAITJOB"):
		// ADOPT without the wake: the call is still running, so the
		// agent waits on its job for as long as the test needs.
		rec("job", map[string]any{"id": 1, "event": "started", "cmd": "make"})
		rec("assistant", map[string]any{"text": "started the build"})
		rec("done", map[string]any{"running": 1})
		say(map[string]any{"kind": "done", "running": 1})
	case strings.HasPrefix(line, "BESIDE"):
		// A turn that finishes while an earlier turn's job still runs.
		rec("assistant", map[string]any{"text": "echo " + line})
		rec("done", map[string]any{"jobs": 1})
		say(map[string]any{"kind": "done", "jobs": 1})
	case strings.HasPrefix(line, "RACE"):
		say(map[string]any{"kind": "done"})
		time.Sleep(300 * time.Millisecond)
		rec("assistant", map[string]any{"text": "echo " + line})
		rec("done", nil)
	default:
		if strings.HasPrefix(line, "FAIL") {
			rec("error", map[string]any{"text": "boom"})
			say(map[string]any{"kind": "error", "text": "boom"})
		}
		rec("assistant", map[string]any{"text": "echo " + line})
		say(map[string]any{"kind": "assistant", "text": "echo " + line})
		rec("done", nil)
		say(map[string]any{"kind": "done"})
	}
}

func nextSeq(path string) int64 {
	b, _ := os.ReadFile(path)
	return int64(strings.Count(string(b), "\n")) + 1
}

func say(obj map[string]any) {
	b, _ := json.Marshal(obj)
	os.Stdout.Write(append(b, '\n'))
}

func appendEntryRaw(path string, b []byte) {
	os.MkdirAll(filepath.Dir(path), 0o755)
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return
	}
	defer f.Close()
	f.Write(b)
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
	rt     *container.Fake
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
	// Always a Fake: the default runtime on darwin is the real Apple CLI.
	rt := container.NewFake()
	sup, err := NewSupervisor(Options{
		Runtime:  rt,
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
	return &fixture{sup: sup, home: home, hist: hist, starts: starts, rt: rt}
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

	id, err := f.sup.Create(CreateOptions{Cwd: f.home, Prompt: "hello"})
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	// Minted by serve, not the child's own pick: see Create.
	if id == "" || id == "sess-create" {
		t.Fatalf("id = %q, want one serve minted", id)
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

// Create's id is only handed out once the session's meta is on disk:
// until then the listing has no cwd for it, and a read by id (GET
// changes, GET diff) ran git in serve's own directory instead.
func TestSupervisorCreateReturnsOnceCwdIsKnown(t *testing.T) {
	t.Parallel()
	f := newFixture(t, envNewID+"=sess-slowmeta", envSlowMeta+"=1")
	cwd := t.TempDir()
	id, err := f.sup.Create(CreateOptions{Cwd: cwd})
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	infos, err := f.sup.List()
	if err != nil {
		t.Fatal(err)
	}
	for _, in := range infos {
		if in.ID == id {
			if want, _ := filepath.EvalSymlinks(cwd); in.Cwd != want && in.Cwd != cwd {
				t.Fatalf("listed cwd right after Create = %q, want %q", in.Cwd, cwd)
			}
			return
		}
	}
	t.Fatalf("%s not listed right after Create", id)
}

func TestSupervisorCreatePrefersMetaID(t *testing.T) {
	t.Parallel()
	f := newFixture(t, envNewID+"=sess-meta", envMetaID+"=1")
	// Only a project child picks its own id; a local one is minted.
	id, err := f.sup.Create(CreateOptions{Mode: "project", Slug: "app"})
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

	if err := f.sup.Answer("sess-ask", "", "a"); err != nil {
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
	if err := f.sup.Answer("sess-ask", "", "a"); err != ErrNoAsk {
		t.Errorf("second Answer = %v, want ErrNoAsk", err)
	}
}

// Answer checks the question it answers, writes the answer and disarms
// that question in one step: an answer naming another question is
// refused and leaves the arm alone, and of answers racing for one
// question (two tabs, a retry, the CLI) exactly one is written; a
// second line would be read as the next question's answer or a steer.
// Found by tests/model/mbt/ask_answer_arm_races_test.go (the spec's
// AnswerOncePerAsk and DisarmOnlyOwn).
func TestSupervisorAnswerIsOnePerAsk(t *testing.T) {
	t.Parallel()
	f := newFixture(t, envAsk+"=1")
	f.seed(t, "sess-once")
	if err := f.sup.Send("sess-once", "question time"); err != nil {
		t.Fatalf("Send: %v", err)
	}
	waitFor(t, "the ask", func() bool { return f.sup.PendingAsk("sess-once") != nil })

	if err := f.sup.Answer("sess-once", "not-q1", "a"); !errors.Is(err, ErrAskExpired) {
		t.Fatalf("Answer naming another question = %v, want ErrAskExpired", err)
	}
	if f.sup.PendingAsk("sess-once") == nil {
		t.Fatal("a refused answer disarmed the question")
	}
	const n = 16
	start, errs := make(chan struct{}), make(chan error, n)
	for range n {
		go func() { <-start; errs <- f.sup.Answer("sess-once", "q1", "a") }()
	}
	close(start)
	written := 0
	for range n {
		if err := <-errs; err == nil {
			written++
		} else if !errors.Is(err, ErrNoAsk) {
			t.Errorf("a losing Answer = %v, want ErrNoAsk", err)
		}
	}
	if written != 1 {
		t.Fatalf("%d of %d racing answers for one question were written, want 1", written, n)
	}
	waitFor(t, "the answer to land", func() bool {
		for _, e := range f.sup.Recent("sess-once") {
			if e.Kind == "assistant" && e.Text == "answered:a" {
				return true
			}
		}
		return false
	})
}

// An ask that returns with no answer (a timeout) disarms on the event
// StatusOf reads: the row says running again, and a /prompt then must
// steer rather than be refused as answering a question nobody waits on.
// A call's live start and another tool's end leave the ask armed.
// Found by tests/model/mbt/ask_answer_test.go (AskPlain, Resolve).
func TestSupervisorAskEndWithoutAnswerDisarms(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	emit := func(kind, text string, extra map[string]any) {
		f.sup.mu.Lock()
		f.sup.emitLocked("sess-end", kind, text, extra)
		f.sup.mu.Unlock()
	}
	for _, end := range []struct {
		kind  string
		extra map[string]any
	}{
		{"call", map[string]any{"tool": "ask", "id": "c1", "error": "ask: no answer after 10m0s"}},
		{"call", map[string]any{"tool": "secret", "id": "c1", "error": "secret: ask: no answer after 10m0s"}},
		{"result", nil},
		// A rule's approval of a bash call ends as the bash call, which
		// says nothing about the ask; the Asker's own end names it.
		{"ask/end", map[string]any{"id": "ask-1"}},
	} {
		emit("ask", "which?", map[string]any{"id": "ask-1"})
		emit("call", "which?", map[string]any{"tool": "ask", "id": "c1", "phase": "start"})
		emit("call", "ls", map[string]any{"tool": "bash", "id": "c0"})
		emit("ask/end", "", map[string]any{"id": "ask-0"})
		if f.sup.PendingAsk("sess-end") == nil {
			t.Fatalf("%s: a live start or another tool's end disarmed the ask", end.kind)
		}
		emit(end.kind, "", end.extra)
		if a := f.sup.PendingAsk("sess-end"); a != nil {
			t.Fatalf("after %s %v the ask is still armed: %+v", end.kind, end.extra["tool"], a)
		}
	}
}

// Every line the child writes to stderr arrives as an "error" event,
// and a config hot reload writes "bough: reloaded ...". An error is not
// the end of a turn or of the ask blocking it, so the arm must survive:
// dropped, the page kept showing the question and every answer was 409.
// Found by tests/model/mbt/ask_across_reload_respawn_test.go (ReloadUi).
func TestSupervisorErrorLineKeepsTheAsk(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.sup.mu.Lock()
	f.sup.emitLocked("sess-err", "ask", "which?", map[string]any{"id": "ask-1"})
	f.sup.emitLocked("sess-err", "error", "bough: reloaded /home/.bough/bough.yml", nil)
	f.sup.mu.Unlock()
	if a := f.sup.PendingAsk("sess-err"); a == nil || a.ID != "ask-1" {
		t.Fatalf("after a stderr line the arm is %+v, want ask-1", a)
	}
}

// An ask whose end history records disarms even when no event said so:
// the end can happen while the child's ui row is between dispose and
// remount (a config reload), and nothing prints it. The row (read off
// history) stopped showing the question while the arm refused every
// /prompt as answering it. An ask history has no end for stays armed.
// Found by tests/model/mbt/ask_across_reload_respawn_test.go (ReloadUi,
// Timeout).
func TestSupervisorAskEndedInHistoryDisarms(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	now := time.Now()
	f.seed(t, "sess-hist",
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": f.home}},
		history.Entry{Seq: 2, At: now, Kind: "input", Data: map[string]any{"text": "go"}},
		history.Entry{Seq: 3, At: now, Kind: "ask", Data: map[string]any{"id": "ask-1", "question": "which?"}})
	f.sup.mu.Lock()
	f.sup.emitLocked("sess-hist", "ask", "which?", map[string]any{"id": "ask-1"})
	f.sup.mu.Unlock()
	if a := f.sup.PendingAsk("sess-hist"); a == nil {
		t.Fatal("an ask history leaves open is not armed")
	}
	appendEntry(filepath.Join(f.hist, "sess-hist.jsonl"), history.Entry{Seq: 4, At: now, Kind: "call",
		Data: map[string]any{"tool": "ask", "id": "c1", "error": "ask: no answer after 10m0s"}})
	if a := f.sup.PendingAsk("sess-hist"); a != nil {
		t.Fatalf("an ask history has ended is still armed: %+v", a)
	}
}

func TestSupervisorAnswerRejectsAskEndedDuringReload(t *testing.T) {
	t.Parallel()
	f := newFixture(t, envAsk+"=1")
	f.seed(t, "sess-reload")
	if err := f.sup.Send("sess-reload", "question time"); err != nil {
		t.Fatal(err)
	}
	waitFor(t, "the ask", func() bool { return f.sup.PendingAsk("sess-reload") != nil })
	// The UI row is absent when the ask ends: history records the end,
	// but no event reaches serve to clear its arm before Answer.
	f.seed(t, "sess-reload",
		history.Entry{Seq: 2, Kind: "ask", Data: map[string]any{"id": "q1", "question": "which?"}},
		history.Entry{Seq: 3, Kind: "call", Data: map[string]any{"tool": "ask", "id": "c1", "error": "timed out"}})
	if err := f.sup.Answer("sess-reload", "q1", "late answer"); !errors.Is(err, ErrNoAsk) {
		t.Fatalf("Answer after the ask ended in history = %v, want ErrNoAsk", err)
	}
}

// The engine's native ask runs beside other calls of the same reply, so
// a sibling run_js's result, its live error, a stderr line or a recorded
// error note all land while the ask is open; none of them is its end,
// and disarming on them made the page's answer 409 while the Asker
// still waited. Only the ask's own call ending (or the turn's close)
// disarms it. Found by tests/model/mbt/ask_beside_parallel_calls_test.go
// (SiblingEndOk after AskOpens with a run_js sibling; HookErrorNote).
func TestSupervisorNativeAskOutlivesItsSiblings(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	emit := func(kind, text string, extra map[string]any) {
		f.sup.mu.Lock()
		f.sup.emitLocked("sess-native", kind, text, extra)
		f.sup.mu.Unlock()
	}
	emit("ask", "which?", map[string]any{"id": "ask-1", "call": "c1"})
	for _, ev := range []struct {
		kind  string
		extra map[string]any
	}{
		{"error", nil},
		{"result", map[string]any{"code": "1", "error": "boom"}},
		{"call", map[string]any{"tool": "bash", "id": "c2", "exit": 3}},
		{"call", map[string]any{"tool": "ask", "id": "c3"}},
	} {
		emit(ev.kind, "", ev.extra)
		if f.sup.PendingAsk("sess-native") == nil {
			t.Fatalf("a %s %v disarmed the native ask of call c1", ev.kind, ev.extra)
		}
	}
	emit("call", "which?", map[string]any{"tool": "ask", "id": "c1", "error": "ask: no answer after 10m0s"})
	if a := f.sup.PendingAsk("sess-native"); a != nil {
		t.Fatalf("the ask's own call ended and it is still armed: %+v", a)
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

// SIGKILL skips the child's own orb unmount, so Kill stops the container;
// the loop demo on the work machine left four running.
func TestSupervisorKillStopsProjectOrb(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.seed(t, "sess-orb")
	ctx := context.Background()
	name := container.OrbName("sess-orb")
	if err := f.rt.Build(ctx, container.BuildSpec{Tag: "img"}, nil); err != nil {
		t.Fatal(err)
	}
	if err := f.rt.Start(ctx, container.RunSpec{Name: name, Image: "img"}); err != nil {
		t.Fatal(err)
	}
	if err := f.sup.Adopt("sess-orb"); err != nil {
		t.Fatalf("Adopt: %v", err)
	}
	waitFor(t, "the child", func() bool { return f.startCount(t) == 1 })
	if err := f.sup.Kill("sess-orb"); err != nil {
		t.Fatalf("Kill: %v", err)
	}
	if st, _ := f.rt.Inspect(ctx, name); st != container.StateStopped {
		t.Errorf("orb after Kill = %v, want stopped", st)
	}
	// A session without a container is left alone: no stop call.
	f.seed(t, "sess-local")
	if err := f.sup.Adopt("sess-local"); err != nil {
		t.Fatalf("Adopt: %v", err)
	}
	waitFor(t, "the local child", func() bool { return f.startCount(t) == 2 })
	f.sup.Kill("sess-local")
	for _, c := range f.rt.CallList() {
		if c == "stop "+container.OrbName("sess-local") {
			t.Errorf("Kill of a local session stopped a container: %v", f.rt.CallList())
		}
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

// An interrupt sent before the child has taken the prompt (still
// booting, or the line not yet read) must still cancel that turn: it
// used to be lost, and the turn ran to the end.
func TestSupervisorInterruptBeforeReady(t *testing.T) {
	t.Parallel()
	for _, warm := range []bool{false, true} {
		f := newFixture(t, envSlowSig+"=1")
		id := fmt.Sprintf("sess-early-%v", warm)
		f.seed(t, id)
		if warm {
			if err := f.sup.Adopt(id); err != nil {
				t.Fatalf("Adopt: %v", err)
			}
			waitFor(t, "boot", func() bool { return hasKind(f.sup.Recent(id), "meta") })
		}
		if err := f.sup.Send(id, "tell a long story"); err != nil {
			t.Fatalf("Send: %v", err)
		}
		if err := f.sup.Interrupt(id); err != nil {
			t.Fatalf("Interrupt: %v", err)
		}
		waitFor(t, "the turn to end", func() bool { return hasKind(f.sup.Recent(id), "exit") || hasKind(f.sup.Recent(id), "done") })
		waitFor(t, "cancelled (warm="+fmt.Sprint(warm)+")", func() bool { return hasKind(f.sup.Recent(id), "cancelled") })
		if hasKind(f.sup.Recent(id), "assistant") {
			t.Errorf("warm=%v: the interrupted turn still replied: %v", warm, kinds(f.sup.Recent(id)))
		}
	}
}

// A steer the running turn has not taken yet must not hold Esc back:
// the turn is live, so the interrupt goes straight through.
func TestSupervisorInterruptWithPendingSteer(t *testing.T) {
	t.Parallel()
	f := newFixture(t, envSlowSig+"=1")
	id := "sess-steer"
	f.seed(t, id)
	if err := f.sup.Send(id, "tell a long story"); err != nil {
		t.Fatalf("Send: %v", err)
	}
	waitFor(t, "the turn", func() bool { return hasKind(f.sup.Recent(id), "input") })
	if err := f.sup.Send(id, "shorter please"); err != nil {
		t.Fatalf("Send steer: %v", err)
	}
	time.Sleep(50 * time.Millisecond)
	if err := f.sup.Interrupt(id); err != nil {
		t.Fatalf("Interrupt: %v", err)
	}
	deadline := time.Now().Add(time.Second)
	for !hasKind(f.sup.Recent(id), "cancelled") {
		if time.Now().After(deadline) {
			t.Fatalf("Esc with a steer pending was held: %v", kinds(f.sup.Recent(id)))
		}
		time.Sleep(10 * time.Millisecond)
	}
}

// The real child reports a taken prompt only through its output. An
// Esc once that output streams must go straight through, not wait out
// the hold limit while the turn keeps running.
func TestSupervisorInterruptMidTurnWithoutInputLine(t *testing.T) {
	t.Parallel()
	f := newFixture(t, envSlowSig+"=1", envNoInput+"=1")
	id := "sess-noinput"
	f.seed(t, id)
	if err := f.sup.Send(id, "tell a long story"); err != nil {
		t.Fatalf("Send: %v", err)
	}
	time.Sleep(time.Second) // boot, read the line, stream the first token
	if err := f.sup.Interrupt(id); err != nil {
		t.Fatalf("Interrupt: %v", err)
	}
	deadline := time.Now().Add(1500 * time.Millisecond)
	for time.Now().Before(deadline) && !hasKind(f.sup.Recent(id), "cancelled") {
		time.Sleep(20 * time.Millisecond)
	}
	if !hasKind(f.sup.Recent(id), "cancelled") {
		t.Fatalf("interrupt mid-turn was held, not sent: %v", kinds(f.sup.Recent(id)))
	}
}

// Until the model's first token the real child prints only "model is
// thinking". A Stop then must reach the turn at once: it used to be
// held for holdLimit (20 s) as if the prompt were still unread.
func TestSupervisorInterruptWhileModelThinks(t *testing.T) {
	t.Parallel()
	f := newFixture(t, envSlowSig+"=1", envThinking+"=1")
	id := "sess-thinking"
	f.seed(t, id)
	if err := f.sup.Send(id, "tell a long story"); err != nil {
		t.Fatalf("Send: %v", err)
	}
	waitFor(t, "the model to start thinking", func() bool { return hasKind(f.sup.Recent(id), "activity") })
	if err := f.sup.Interrupt(id); err != nil {
		t.Fatalf("Interrupt: %v", err)
	}
	deadline := time.Now().Add(1500 * time.Millisecond)
	for time.Now().Before(deadline) && !hasKind(f.sup.Recent(id), "cancelled") {
		time.Sleep(20 * time.Millisecond)
	}
	if !hasKind(f.sup.Recent(id), "cancelled") {
		t.Fatalf("Stop while the model thinks was held, not sent: %v", kinds(f.sup.Recent(id)))
	}
}

// A prompt sent right after a Stop ended the turn must run: the child
// the Stop signalled is on its way out, and a line written to it was
// either lost or opened a turn its exit then cancelled (CI:
// TestSteerQueuePaths saw an input followed by a cancel nobody asked for).
func TestSupervisorSendAfterInterruptRuns(t *testing.T) {
	t.Parallel()
	f := newFixture(t, envSlowSig+"=1", envLinger+"=1")
	id := "sess-after-stop"
	f.seed(t, id)
	if err := f.sup.Send(id, "tell a long story"); err != nil {
		t.Fatalf("Send: %v", err)
	}
	waitFor(t, "the turn", func() bool { return hasKind(f.sup.Recent(id), "input") })
	if err := f.sup.Interrupt(id); err != nil {
		t.Fatalf("Interrupt: %v", err)
	}
	waitFor(t, "the cancel", func() bool { return hasKind(f.sup.Recent(id), "cancelled") })
	if err := f.sup.Send(id, "next one"); err != nil {
		t.Fatalf("Send after Stop: %v", err)
	}
	waitFor(t, "the next prompt's reply", func() bool {
		for _, e := range f.sup.Recent(id) {
			if e.Kind == "assistant" && e.Text == "echo next one" {
				return true
			}
		}
		return false
	})
	evs := f.sup.Recent(id)
	var after []string
	for i, e := range evs {
		if e.Kind == "input" && e.Text == "next one" {
			after = kinds(evs[i:])
		}
	}
	if slices.Contains(after, "cancelled") {
		t.Errorf("the prompt sent after Stop was cancelled: %v", kinds(evs))
	}
}

// A rename whose save fails must change nothing: the page shows the
// error, so a list read must not show the new name either, and a
// restart would bring the old one back (specs/title_rename.fizz,
// FailedRenameChangesNothing).
func TestSupervisorSetTitleFailedSaveChangesNothing(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.seed(t, "sess-name")
	if err := f.sup.SetTitle("sess-name", "before"); err != nil {
		t.Fatalf("SetTitle: %v", err)
	}
	// A directory where the save writes its temp file fails the write.
	tmp := filepath.Join(f.home, ".bough", "serve", "meta.json.tmp")
	if err := os.Mkdir(tmp, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := f.sup.SetTitle("sess-name", "after"); err == nil {
		t.Fatal("SetTitle succeeded with meta.json.tmp blocked")
	}
	if got := f.sup.Meta("sess-name").Title; got != "before" {
		t.Errorf("after a failed save, meta title = %q, want %q", got, "before")
	}
}

// A whitespace-only title from any client (the page trims, curl does
// not) hands the name back to history: stored as is, rowOf preferred it
// to history's title and every page said "Untitled"
// (specs/session_title_writers.fizz, BlankNeverHidesHistory).
func TestSupervisorSetTitleBlankHandsBack(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.seed(t, "sess-blank")
	if err := f.sup.SetTitle("sess-blank", "mine"); err != nil {
		t.Fatalf("SetTitle: %v", err)
	}
	if err := f.sup.SetTitle("sess-blank", " \t "); err != nil {
		t.Fatalf("SetTitle: %v", err)
	}
	if got := f.sup.Meta("sess-blank").Title; got != "" {
		t.Errorf("after a blank rename, meta title = %q, want empty", got)
	}
	if err := f.sup.SetTitle("sess-blank", "  padded  "); err != nil {
		t.Fatalf("SetTitle: %v", err)
	}
	if got := f.sup.Meta("sess-blank").Title; got != "padded" {
		t.Errorf("meta title = %q, want %q", got, "padded")
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

// A model picked from another provider's list carries that provider, so
// the child's llm row moves with it instead of calling the old provider.
func TestSupervisorSetModelNamesTheProvider(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.seed(t, "sess-model")

	if err := f.sup.SetModel("sess-model", "llm-google", "google/gemini-3.8-flash"); err != nil {
		t.Fatalf("SetModel: %v", err)
	}
	waitFor(t, "the /model command to reach the child", func() bool {
		for _, e := range f.sup.Recent("sess-model") {
			if e.Kind == "input" && e.Text == "/model llm-google google/gemini-3.8-flash" {
				return true
			}
		}
		return false
	})
	if got := f.sup.Meta("sess-model").Model; got != "google/gemini-3.8-flash" {
		t.Errorf("meta model = %q, want the bare id the picker matches", got)
	}
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

// A pre-minted ID is the child's BOUGH_SESSION_ID: Create waits for that
// one file, even with an unrelated file appearing (BOUGH_FAKE_NEWID would
// have named another), and Args and Origin reach the child.
func TestCreateWithPreMintedID(t *testing.T) {
	f := newFixture(t, envNewID+"=not-this-one", envTurns+"=1")
	f.seed(t, "someone-else")
	id, err := f.sup.Create(CreateOptions{ID: "loop-child-1", Cwd: f.home, Args: []string{"--set", "llm.model=x"}, Origin: "loop"})
	if err != nil {
		t.Fatal(err)
	}
	if id != "loop-child-1" {
		t.Fatalf("id = %q", id)
	}
	if !f.sup.Live(id) {
		t.Fatal("pre-minted session not leased")
	}
	b, err := os.ReadFile(filepath.Join(f.hist, id+".jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	var meta history.Entry
	if err := json.Unmarshal([]byte(strings.SplitN(string(b), "\n", 2)[0]), &meta); err != nil {
		t.Fatal(err)
	}
	if args, _ := meta.Data["args"].(string); !strings.Contains(args, "--headless --json --set llm.model=x") {
		t.Errorf("argv = %q", args)
	}
	if o, _ := meta.Data["origin"].(string); o != "loop" {
		t.Errorf("origin = %q", o)
	}
	if _, err := os.Stat(filepath.Join(f.hist, "not-this-one.jsonl")); err == nil {
		t.Error("child used BOUGH_FAKE_NEWID over the pre-minted id")
	}
	// A respawn after the child dies keeps the recorded Args.
	f.sup.Kill(id)
	waitFor(t, "child gone", func() bool { return !f.sup.Live(id) })
	if err := f.sup.Send(id, "again"); err != nil {
		t.Fatal(err)
	}
	waitFor(t, "respawned", func() bool { return f.startCount(t) == 2 })
	st, _ := os.ReadFile(f.starts)
	if !strings.Contains(string(st), id) {
		t.Errorf("starts = %q", st)
	}
}

func TestSupervisorStderrLeavesAskArmed(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.sup.mu.Lock()
	f.sup.emitLocked("sess-err", "ask", "which?", map[string]any{"id": "ask-1"})
	f.sup.mu.Unlock()
	f.sup.pumpStderr(newChild("sess-err"), strings.NewReader("bough: reloaded bough.yml\n"))
	if f.sup.PendingAsk("sess-err") == nil {
		t.Fatal("a stderr line disarmed the pending ask")
	}
	if !hasKind(f.sup.Recent("sess-err"), "error") {
		t.Fatal("the stderr line was not relayed")
	}
}

// Two tabs' acks race a new finish and the one that read the older entry
// saves last. Acknowledge used to set it unconditionally, taking the ack
// back and bringing a finish the other tab had seen back unseen
// (tests/model/specs/multi_tab_remote_change.fizz, AckMonotonic).
func TestSupervisorAcknowledgeNeverGoesBack(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	now := time.Now()
	f.seed(t, "sess-ack",
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": f.home}},
		history.Entry{Seq: 2, At: now, Kind: "input"},
		history.Entry{Seq: 3, At: now, Kind: "done"},
		history.Entry{Seq: 4, At: now, Kind: "input"},
		history.Entry{Seq: 5, At: now, Kind: "done"},
	)
	if err := f.sup.Acknowledge("sess-ack", 5); err != nil {
		t.Fatal(err)
	}
	if err := f.sup.Acknowledge("sess-ack", 3); err != nil {
		t.Fatal(err)
	}
	if got := f.sup.Meta("sess-ack").Ack; got != 5 {
		t.Errorf("ack after a late save of seq 3 = %d, want 5", got)
	}
	// A page acks only what it showed; a bare ack takes everything.
	if err := f.sup.Acknowledge("sess-ack", 0); err != nil {
		t.Fatal(err)
	}
	f.seed(t, "sess-shown",
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": f.home}},
		history.Entry{Seq: 2, At: now, Kind: "done"},
		history.Entry{Seq: 3, At: now, Kind: "done"},
	)
	if err := f.sup.Acknowledge("sess-shown", 2); err != nil {
		t.Fatal(err)
	}
	if got := f.sup.Meta("sess-shown").Ack; got != 2 {
		t.Errorf("ack up to 2 saved %d", got)
	}
	if err := f.sup.Acknowledge("sess-shown", 0); err != nil {
		t.Fatal(err)
	}
	if got := f.sup.Meta("sess-shown").Ack; got != 3 {
		t.Errorf("a bare ack saved %d, want the last entry 3", got)
	}
}
