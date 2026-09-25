//go:build !windows

// These run the real binary headless on the default engine with
// --set llm.plugin=llm-control, one test per mode. The row runs on the
// unreal-agent harness, which does not build for Windows.
package llm

import (
	"bytes"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"
)

var boughBin string

func TestMain(m *testing.M) {
	_, file, _, _ := runtime.Caller(0)
	root := filepath.Join(filepath.Dir(file), "..", "..", "..")
	os.Setenv("BOUGH_WEB_ADDR", "127.0.0.1:0")
	if bin := os.Getenv("BOUGH_BIN"); bin != "" {
		boughBin = bin
		os.Exit(m.Run())
	}
	dir, err := os.MkdirTemp("", "bough-llm-control-bin-")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	boughBin = filepath.Join(dir, "bough")
	build := exec.Command("go", "build", "-o", boughBin, "./cmd/bough")
	build.Dir = root
	if out, err := build.CombinedOutput(); err != nil {
		fmt.Fprintf(os.Stderr, "build: %v\n%s", err, out)
		os.RemoveAll(dir)
		os.Exit(1)
	}
	code := m.Run()
	os.RemoveAll(dir)
	os.Exit(code)
}

type buf struct {
	mu sync.Mutex
	b  bytes.Buffer
}

func (b *buf) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.b.Write(p)
}

func (b *buf) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.b.String()
}

type run struct {
	t      *testing.T
	home   string
	out    *buf
	stdin  *os.File
	exited chan error
}

// start launches `bough --headless --set llm.plugin=llm-control` in a
// fresh HOME and cwd with an open stdin; the default bough.yml is
// copied in, so the row under test is the only change to it.
func start(t *testing.T, args ...string) *run { return startWith(t, false, nil, args...) }

// startHeld is start with HoldStart in place before the process runs.
func startHeld(t *testing.T) *run { return startWith(t, true, nil) }

// startEnv is start with env added to the child's environment.
func startEnv(t *testing.T, env []string, args ...string) *run {
	t.Helper()
	return startWith(t, false, env, args...)
}

func startWith(t *testing.T, hold bool, env []string, args ...string) *run {
	t.Helper()
	base := t.TempDir()
	home, cwd := filepath.Join(base, "home"), filepath.Join(base, "cwd")
	for _, d := range []string{home, cwd} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	_, file, _, _ := runtime.Caller(0)
	yml, err := os.ReadFile(filepath.Join(filepath.Dir(file), "..", "..", "..", "bough.yml"))
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(cwd, "bough.yml"), yml, 0o644); err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(boughBin, append([]string{"--config", "bough.yml", "--set", "llm.plugin=llm-control", "--headless"}, args...)...)
	cmd.Dir = cwd
	cmd.Env = []string{"HOME=" + home}
	for _, kv := range os.Environ() {
		if !strings.HasPrefix(kv, "HOME=") && !strings.HasPrefix(kv, "BOUGH_ROOT=") {
			cmd.Env = append(cmd.Env, kv)
		}
	}
	if hold {
		HoldStart(t, Dir(home))
	}
	cmd.Env = append(cmd.Env, env...)
	r := &run{t: t, home: home, out: &buf{}, exited: make(chan error, 1)}
	cmd.Stdout, cmd.Stderr = r.out, r.out
	pr, pw, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	cmd.Stdin, r.stdin = pr, pw
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	pr.Close()
	go func() { r.exited <- cmd.Wait() }()
	t.Cleanup(func() {
		pw.Close()
		select {
		case <-r.exited:
		case <-time.After(5 * time.Second):
			cmd.Process.Kill()
		}
	})
	return r
}

func (r *run) dir() string { return Dir(r.home) }

func (r *run) send(line string) {
	r.t.Helper()
	if _, err := fmt.Fprintln(r.stdin, line); err != nil {
		r.t.Fatal(err)
	}
}

func (r *run) waitFor(s string) {
	r.t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		if strings.Contains(r.out.String(), s) {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	r.t.Fatalf("%q not in output after 30s:\n%s", s, r.out.String())
}

// finish closes stdin and returns the exit code and the output.
func (r *run) finish() (int, string) {
	r.t.Helper()
	r.stdin.Close()
	select {
	case err := <-r.exited:
		r.exited <- err
		if ee, ok := err.(*exec.ExitError); ok {
			return ee.ExitCode(), r.out.String()
		}
		if err != nil {
			r.t.Fatal(err)
		}
		return 0, r.out.String()
	case <-time.After(30 * time.Second):
		r.t.Fatalf("no exit after 30s:\n%s", r.out.String())
		return -1, ""
	}
}

func TestControlOK(t *testing.T) {
	t.Parallel()
	r := start(t)
	Queue(t, r.dir(), "001", Turn{Mode: "ok", Text: "all good from control"})
	r.send("hello")
	r.waitFor("[assistant] all good from control")
	code, out := r.finish()
	if code != 0 {
		t.Fatalf("exit %d:\n%s", code, out)
	}
	WaitTaken(t, r.dir(), "001", time.Second)
}

func TestControlError(t *testing.T) {
	t.Parallel()
	r := start(t)
	Queue(t, r.dir(), "001", Turn{Mode: "error", Error: "control says boom"})
	r.send("hello")
	r.waitFor("control says boom")
	code, out := r.finish()
	if code != 1 {
		t.Fatalf("exit %d, want 1 for an errored turn:\n%s", code, out)
	}
	if !strings.Contains(out, "[error]") {
		t.Fatalf("no [error] line:\n%s", out)
	}
}

// Slow streams one word per delay: the first fragment is on stdout
// while the last word is still unsent, so a test can act mid-stream.
func TestControlSlow(t *testing.T) {
	t.Parallel()
	r := start(t, "--json")
	Queue(t, r.dir(), "001", Turn{Mode: "slow", Text: "alpha beta gamma delta omega", DelayMS: 300})
	r.send("hello")
	r.waitFor(`"kind":"assistant-delta","text":"alpha `)
	if out := r.out.String(); strings.Contains(out, "omega") {
		t.Fatalf("whole reply arrived with the first fragment:\n%s", out)
	}
	r.waitFor(`"text":"alpha beta gamma delta omega"`)
	code, out := r.finish()
	if code != 0 {
		t.Fatalf("exit %d:\n%s", code, out)
	}
	if n := strings.Count(out, `"kind":"assistant-delta"`); n < 5 {
		t.Fatalf("%d deltas, want one per word:\n%s", n, out)
	}
}

// A held turn released with a Turn in the release file answers as that
// turn says, so a test can take a session to "running" first and only
// then decide whether the turn finishes or fails.
func TestControlBlockReleaseError(t *testing.T) {
	t.Parallel()
	r := start(t)
	Queue(t, r.dir(), "001", Turn{Mode: "block", Text: "never sent"})
	r.send("hello")
	WaitTaken(t, r.dir(), "001", 30*time.Second)
	ReleaseWith(t, r.dir(), "001", Turn{Mode: "error", Error: "released as boom"})
	r.waitFor("released as boom")
	code, out := r.finish()
	if code != 1 {
		t.Fatalf("exit %d, want 1 for an errored turn:\n%s", code, out)
	}
	if strings.Contains(out, "never sent") {
		t.Fatalf("held text sent despite the error release:\n%s", out)
	}
}

// A held turn released with Bash answers with a bash tool call instead
// of text: the engine runs it, records the call with its exit, and asks
// the model again, which takes the next queued turn. That is how a test
// puts a failed test run into a session that is still running.
func TestControlBlockReleaseBash(t *testing.T) {
	t.Parallel()
	r := start(t, "--json")
	Queue(t, r.dir(), "001", Turn{Mode: "block", Text: "never sent"})
	r.send("hello")
	WaitTaken(t, r.dir(), "001", 30*time.Second)
	Queue(t, r.dir(), "002", Turn{Mode: "ok", Text: "after the call"})
	ReleaseWith(t, r.dir(), "001", Turn{Bash: "echo ran-by-control; exit 3 # go test"})
	r.waitFor("after the call")
	code, out := r.finish()
	if code != 0 {
		t.Fatalf("exit %d:\n%s", code, out)
	}
	if !strings.Contains(out, "ran-by-control") || strings.Contains(out, "never sent") {
		t.Fatalf("the release did not run its bash call:\n%s", out)
	}
	WaitTaken(t, r.dir(), "002", time.Second)
}

// A held turn released with Calls answers with all of them in one
// reply, and the engine runs them at once: two bash calls that each
// wait for the other's file both finish, which they could not one after
// the other.
func TestControlBlockReleaseCalls(t *testing.T) {
	t.Parallel()
	r := start(t, "--json")
	Queue(t, r.dir(), "001", Turn{Mode: "block", Text: "never sent"})
	r.send("hello")
	WaitTaken(t, r.dir(), "001", 30*time.Second)
	Queue(t, r.dir(), "002", Turn{Mode: "ok", Text: "after both calls"})
	wait := func(mine, other string) string {
		return fmt.Sprintf("touch %s; for i in $(seq 200); do [ -f %s ] && echo both-ran && exit 0; sleep 0.05; done; exit 1", mine, other)
	}
	ReleaseWith(t, r.dir(), "001", Turn{Calls: []Call{
		{ID: "a", Name: "bash", Args: map[string]any{"command": wait("a.flag", "b.flag")}},
		{ID: "b", Name: "bash", Args: map[string]any{"command": wait("b.flag", "a.flag")}},
	}})
	r.waitFor("after both calls")
	code, out := r.finish()
	if code != 0 || strings.Count(out, "both-ran") < 2 || strings.Contains(out, "never sent") {
		t.Fatalf("exit %d; want both calls to run at once:\n%s", code, out)
	}
}

// Say streams live text out of a held turn without ending it: the delta
// is on stdout, and the turn still waits for its release.
func TestControlBlockSay(t *testing.T) {
	t.Parallel()
	r := start(t, "--json")
	Queue(t, r.dir(), "001", Turn{Mode: "block", Text: "released after saying"})
	r.send("hello")
	WaitTaken(t, r.dir(), "001", 30*time.Second)
	Say(t, r.dir(), "001", 1, "partial ")
	r.waitFor(`"kind":"assistant-delta","text":"partial "`)
	deadline := time.Now().Add(5 * time.Second)
	for !Said(r.dir(), "001", 1) {
		if time.Now().After(deadline) {
			t.Fatal("say 1 not marked said")
		}
		time.Sleep(10 * time.Millisecond)
	}
	if out := r.out.String(); strings.Contains(out, `"kind":"done"`) || strings.Contains(out, "released after saying") {
		t.Fatalf("the say ended the held turn:\n%s", out)
	}
	Release(t, r.dir(), "001")
	r.waitFor(`"kind":"done"`)
	if code, out := r.finish(); code != 0 {
		t.Fatalf("exit %d:\n%s", code, out)
	}
}

// Block holds the request until the test releases it: nothing replies
// while it is held, and the queued text arrives once it is released.
func TestControlBlock(t *testing.T) {
	t.Parallel()
	r := start(t)
	Queue(t, r.dir(), "001", Turn{Mode: "block", Text: "released at last"})
	r.send("hello")
	WaitTaken(t, r.dir(), "001", 30*time.Second)
	time.Sleep(500 * time.Millisecond)
	if out := r.out.String(); strings.Contains(out, "released at last") || strings.Contains(out, "[done]") {
		t.Fatalf("replied while blocked:\n%s", out)
	}
	Release(t, r.dir(), "001")
	r.waitFor("[assistant] released at last")
	code, out := r.finish()
	if code != 0 {
		t.Fatalf("exit %d:\n%s", code, out)
	}
}

// histFiles lists the session files the run's HOME holds.
func (r *run) histFiles() []string {
	files, _ := filepath.Glob(filepath.Join(r.home, ".bough", "history", "*.jsonl"))
	return files
}

// A held start parks the process before the history row writes its
// file, so a test can see a session that exists but has no history
// yet; releasing it lets the session start as usual.
func TestControlHoldStart(t *testing.T) {
	t.Parallel()
	r := startHeld(t)
	if pid := WaitHeld(t, r.dir(), 30*time.Second); pid <= 0 {
		t.Fatalf("held pid %d", pid)
	}
	time.Sleep(300 * time.Millisecond)
	if f := r.histFiles(); len(f) != 0 {
		t.Fatalf("history written while the start was held: %v", f)
	}
	ReleaseStart(t, r.dir())
	Queue(t, r.dir(), "001", Turn{Mode: "ok", Text: "started after the hold"})
	r.send("hello")
	r.waitFor("[assistant] started after the hold")
	if code, out := r.finish(); code != 0 {
		t.Fatalf("exit %d:\n%s", code, out)
	}
	if f := r.histFiles(); len(f) != 1 {
		t.Fatalf("history files after the release: %v", f)
	}
}

// ExitStart makes a held process exit instead: a session that dies
// before it writes any history.
func TestControlHoldExit(t *testing.T) {
	t.Parallel()
	r := startHeld(t)
	WaitHeld(t, r.dir(), 30*time.Second)
	ExitStart(t, r.dir())
	code, out := r.finish()
	if code == 0 {
		t.Fatalf("exit 0 from a held start told to exit:\n%s", out)
	}
	if f := r.histFiles(); len(f) != 0 {
		t.Fatalf("history written by a start told to exit: %v", f)
	}
}

// Stream sends one fragment while the turn stays held: the delta is on
// stdout, nothing is recorded, and the release still answers after it.
func TestControlBlockStream(t *testing.T) {
	t.Parallel()
	r := start(t, "--json")
	Queue(t, r.dir(), "001", Turn{Mode: "block", Text: "whole reply"})
	r.send("hello")
	WaitTaken(t, r.dir(), "001", 30*time.Second)
	Stream(t, r.dir(), "001", "partial ", 30*time.Second)
	r.waitFor(`"kind":"assistant-delta","text":"partial "`)
	if out := r.out.String(); strings.Contains(out, `"kind":"assistant","text"`) || strings.Contains(out, `"kind":"done"`) {
		t.Fatalf("a streamed fragment recorded or ended the turn:\n%s", out)
	}
	Release(t, r.dir(), "001")
	r.waitFor(`"kind":"assistant","text":"whole reply"`)
	if code, out := r.finish(); code != 0 {
		t.Fatalf("exit %d:\n%s", code, out)
	}
}

// hold_boot holds a fresh session before its history file exists, the
// file serve's Create waits for, until the test releases it: that is
// how a model test keeps a project's main thread or a thread starting.
func TestControlHoldBoot(t *testing.T) {
	t.Parallel()
	const id = "01a0d000-0000-7000-8000-00000000b007"
	r := startEnv(t, []string{"BOUGH_SESSION_ID=" + id}, "--set", "llm.hold_boot=true")
	if role := WaitBooting(t, r.dir(), id, 30*time.Second); role != "session" {
		t.Errorf("booting role = %q, want session", role)
	}
	hist := filepath.Join(r.home, ".bough", "history", id+".jsonl")
	time.Sleep(300 * time.Millisecond)
	if _, err := os.Stat(hist); err == nil {
		t.Fatalf("history written while the boot is held:\n%s", r.out.String())
	}
	ReleaseBoot(t, r.dir(), id)
	Queue(t, r.dir(), "001", Turn{Mode: "ok", Text: "booted and answering"})
	r.send("hello")
	r.waitFor("[assistant] booted and answering")
	if _, err := os.Stat(hist); err != nil {
		t.Fatalf("no history after the release: %v", err)
	}
	if code, out := r.finish(); code != 0 {
		t.Fatalf("exit %d:\n%s", code, out)
	}
}

// Call answers with one tool call instead of text: the engine runs the
// tool, and its result goes out on the next request, which takes the
// next queued turn. Here the call is ask, answered on stdin.
func TestControlCall(t *testing.T) {
	t.Parallel()
	r := start(t)
	Queue(t, r.dir(), "001", Turn{Mode: "call", Tool: "ask", Args: map[string]any{"question": "which colour?", "options": []string{"red", "blue"}}})
	Queue(t, r.dir(), "002", Turn{Mode: "ok", Text: "noted the colour"})
	r.send("hello")
	r.waitFor("[ask] which colour?")
	r.send("2")
	r.waitFor("[assistant] noted the colour")
	code, out := r.finish()
	if code != 0 {
		t.Fatalf("exit %d:\n%s", code, out)
	}
}

// A release that carries a tool call answers with it (after its text,
// when there is any), so the turn goes on: the call runs, and the next
// request takes the next queued turn.
func TestControlBlockReleaseCall(t *testing.T) {
	t.Parallel()
	r := start(t, "--json")
	Queue(t, r.dir(), "001", Turn{Mode: "block"})
	Queue(t, r.dir(), "002", Turn{Mode: "ok", Text: "after the call"})
	r.send("hello")
	WaitTaken(t, r.dir(), "001", 30*time.Second)
	ReleaseWith(t, r.dir(), "001", Turn{Text: "calling now", Call: &Call{Name: "bash", Args: map[string]any{"command": "echo from-the-call"}}})
	r.waitFor(`"kind":"assistant","text":"calling now"`)
	r.waitFor(`from-the-call`)
	r.waitFor(`"kind":"assistant","text":"after the call"`)
	if code, out := r.finish(); code != 0 {
		t.Fatalf("exit %d:\n%s", code, out)
	}
}

// A release that carries a Call answers the held request with that tool
// call, so a test can make the model ask (tools.ask) at a moment it
// picks; the answer goes back as the call's result and the next queued
// turn answers it.
func TestControlBlockReleaseAsk(t *testing.T) {
	t.Parallel()
	r := start(t)
	Queue(t, r.dir(), "001", Turn{Mode: "block", Text: "never sent"})
	Queue(t, r.dir(), "002", Turn{Mode: "ok", Text: "thanks for blue"})
	r.send("hello")
	WaitTaken(t, r.dir(), "001", 30*time.Second)
	ReleaseWith(t, r.dir(), "001", Turn{Call: &Call{Name: "ask", Args: map[string]any{"question": "which colour?"}}})
	r.waitFor("which colour?")
	r.send("blue")
	r.waitFor("[assistant] thanks for blue")
	code, out := r.finish()
	if code != 0 {
		t.Fatalf("exit %d:\n%s", code, out)
	}
	if strings.Contains(out, "never sent") {
		t.Fatalf("held text sent despite the call release:\n%s", out)
	}
}

// A held turn released with a call turn makes that call, so a test can
// hold a session in "running" and only then have the model ask.
func TestControlBlockReleaseCallTurn(t *testing.T) {
	t.Parallel()
	r := start(t)
	Queue(t, r.dir(), "001", Turn{Mode: "block", Text: "never sent"})
	Queue(t, r.dir(), "002", Turn{Mode: "ok", Text: "answer received"})
	r.send("hello")
	WaitTaken(t, r.dir(), "001", 30*time.Second)
	ReleaseWith(t, r.dir(), "001", Turn{Mode: "call", Tool: "ask", Args: map[string]any{"question": "go on?"}})
	r.waitFor("[ask] go on?")
	r.send("yes")
	r.waitFor("[assistant] answer received")
	code, out := r.finish()
	if code != 0 {
		t.Fatalf("exit %d:\n%s", code, out)
	}
	if strings.Contains(out, "never sent") {
		t.Fatalf("held text sent despite the call release:\n%s", out)
	}
}

// A held turn released as a tool call makes the model call that tool:
// here the engine's ask, so a test can take a session to "needs-you".
// The next request, once the answer is in, takes the next queued turn.
func TestControlBlockReleaseCallAskNeedsYou(t *testing.T) {
	t.Parallel()
	r := start(t)
	Queue(t, r.dir(), "001", Turn{Mode: "block", Text: "never sent"})
	r.send("hello")
	WaitTaken(t, r.dir(), "001", 30*time.Second)
	ReleaseWith(t, r.dir(), "001", Turn{Mode: "call", Tool: "ask", Args: map[string]any{"question": "control asks?", "options": []string{"yes", "no"}}})
	r.waitFor("[ask] control asks?")
	Queue(t, r.dir(), "002", Turn{Mode: "ok", Text: "answered and done"})
	r.send("yes")
	r.waitFor("[assistant] answered and done")
	code, out := r.finish()
	if code != 0 {
		t.Fatalf("exit %d:\n%s", code, out)
	}
	if strings.Contains(out, "never sent") {
		t.Fatalf("held text sent despite the call release:\n%s", out)
	}
}

// A turn with calls makes them: the engine runs the tool in the
// session's cwd and asks again, and the next queued turn answers. This
// is how a test has the agent edit a file through the shell.
func TestControlCalls(t *testing.T) {
	t.Parallel()
	r := start(t)
	Queue(t, r.dir(), "001", Turn{Mode: "ok", Calls: []Call{{Name: "bash", Args: map[string]any{"command": "printf shell > made.txt"}}}})
	Queue(t, r.dir(), "002", Turn{Mode: "ok", Text: "made it"})
	r.send("hello")
	r.waitFor("[assistant] made it")
	code, out := r.finish()
	if code != 0 {
		t.Fatalf("exit %d:\n%s", code, out)
	}
	b, err := os.ReadFile(filepath.Join(filepath.Dir(r.home), "cwd", "made.txt"))
	if err != nil || string(b) != "shell" {
		t.Fatalf("the bash call did not run: %q %v\n%s", b, err, out)
	}
}

// Every taken turn leaves the user messages its request carried, so a
// test can see what reached the model: a job notice sent at a request
// boundary is recorded nowhere else.
func TestControlRecordsRequest(t *testing.T) {
	t.Parallel()
	r := start(t)
	Queue(t, r.dir(), "001", Turn{Mode: "ok", Calls: []Call{{Name: "bash", Args: map[string]any{"command": "true"}}}})
	Queue(t, r.dir(), "002", Turn{Mode: "ok", Text: "seen"})
	r.send("hello from the person")
	r.waitFor("[assistant] seen")
	if code, out := r.finish(); code != 0 {
		t.Fatalf("exit %d:\n%s", code, out)
	}
	for _, name := range []string{"001", "002"} {
		got, err := Request(r.dir(), name)
		if err != nil {
			t.Fatal(err)
		}
		if !slices.ContainsFunc(got, func(s string) bool { return strings.Contains(s, "hello from the person") }) {
			t.Fatalf("request %s carried %q, want the prompt", name, got)
		}
	}
}

func TestControlRecordsRawRequest(t *testing.T) {
	t.Parallel()
	r := start(t)
	Queue(t, r.dir(), "001", Turn{Mode: "ok", Text: "noted"})
	r.send("the marker 7f3e")
	r.waitFor("[assistant] noted")
	code, out := r.finish()
	if code != 0 {
		t.Fatalf("exit %d:\n%s", code, out)
	}
	b, err := os.ReadFile(filepath.Join(r.dir(), "001.req"))
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(b), "the marker 7f3e") {
		t.Fatalf("001.req does not carry the prompt:\n%s", b)
	}
}

func TestControlRecordsToolResults(t *testing.T) {
	t.Parallel()
	r := start(t)
	Queue(t, r.dir(), "001", Turn{Mode: "ok", Calls: []Call{{ID: "c1", Name: "bash", Args: map[string]any{"command": "echo recorded-output"}}}})
	Queue(t, r.dir(), "002", Turn{Mode: "ok", Text: "read it"})
	r.send("hello")
	r.waitFor("[assistant] read it")
	if code, out := r.finish(); code != 0 {
		t.Fatalf("exit %d:\n%s", code, out)
	}
	got, err := ToolResults(r.dir(), "002")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(got["c1"], "recorded-output") {
		t.Fatalf("request 002 carried %q, want the bash output for c1", got)
	}
	if first, err := ToolResults(r.dir(), "001"); err != nil || len(first) != 0 {
		t.Fatalf("request 001 carried %q (%v), want no tool results", first, err)
	}
}

func TestControlAPIRetry(t *testing.T) {
	t.Parallel()
	r := start(t, "--json")
	Queue(t, r.dir(), "001", Turn{Mode: "api"})
	r.send("hello")
	WaitAttempt(t, r.dir(), "001", 1, 30*time.Second)
	Stream(t, r.dir(), "001", "first try ", 30*time.Second)
	r.waitFor(`"text":"first try "`)
	AnswerWith(t, r.dir(), "001", Answer{Kind: "transient"})
	WaitRetryWait(t, r.dir(), "001", 30*time.Second)
	r.waitFor(`"kind":"delta-reset"`)
	Retry(t, r.dir(), "001")
	WaitAttempt(t, r.dir(), "001", 2, 30*time.Second)
	AnswerWith(t, r.dir(), "001", Answer{Kind: "ok", Text: "second try"})
	r.waitFor(`"text":"second try"`)
	code, out := r.finish()
	if code != 0 {
		t.Fatalf("exit %d:\n%s", code, out)
	}
	if n := Attempts(r.dir(), "001"); n != 2 {
		t.Fatalf("%d attempts, want 2", n)
	}
}

func TestControlAPIRetriesExhausted(t *testing.T) {
	t.Parallel()
	r := start(t)
	Queue(t, r.dir(), "001", Turn{Mode: "api"})
	r.send("hello")
	WaitAttempt(t, r.dir(), "001", 1, 30*time.Second)
	AnswerWith(t, r.dir(), "001", Answer{Kind: "transient"})
	WaitRetryWait(t, r.dir(), "001", 30*time.Second)
	Retry(t, r.dir(), "001")
	WaitAttempt(t, r.dir(), "001", 2, 30*time.Second)
	AnswerWith(t, r.dir(), "001", Answer{Kind: "transient"})
	r.waitFor("[error]")
	code, out := r.finish()
	if code != 1 {
		t.Fatalf("exit %d, want 1 for an errored turn:\n%s", code, out)
	}
	if n := Attempts(r.dir(), "001"); n != 2 {
		t.Fatalf("%d attempts, want 2:\n%s", n, out)
	}
}

func TestControlAPIAnswers(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		kind, want string
		code       int
	}{
		{"refused", "the model declined", 1},
		{"overflow", "no longer fits the model's context window", 1},
		{"max_tokens", "reply cut off at max_tokens", 0},
		{"fatal", "the request was refused", 1},
	} {
		t.Run(c.kind, func(t *testing.T) {
			t.Parallel()
			r := start(t)
			Queue(t, r.dir(), "001", Turn{Mode: "api", Answer: &Answer{Kind: c.kind, Text: "some text"}})
			r.send("hello")
			r.waitFor(c.want)
			code, out := r.finish()
			if code != c.code {
				t.Fatalf("exit %d, want %d:\n%s", code, c.code, out)
			}
		})
	}
}
