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
func start(t *testing.T, args ...string) *run {
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
