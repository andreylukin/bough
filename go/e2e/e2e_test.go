// Package e2e execs the real bough binary. TestMain builds it once
// per run; every test gets its own temp HOME, temp cwd, config copy,
// and process, so all tests run in parallel.
package e2e

import (
	"bytes"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"runtime"
	"strings"
	"sync"
	"testing"
	"time"
)

var (
	boughBin string
	repoRoot string
)

func TestMain(m *testing.M) {
	_, file, _, ok := runtime.Caller(0)
	if !ok {
		fmt.Fprintln(os.Stderr, "e2e: cannot locate source file")
		os.Exit(1)
	}
	repoRoot = filepath.Dir(filepath.Dir(file))

	if bin := os.Getenv("BOUGH_BIN"); bin != "" {
		boughBin = bin
		os.Exit(m.Run())
	}
	dir, err := os.MkdirTemp("", "bough-e2e-bin-")
	if err != nil {
		fmt.Fprintln(os.Stderr, "e2e:", err)
		os.Exit(1)
	}
	// ".exe" on Windows, or the file builds and then cannot be
	// executed: "executable file not found in %PATH%", which was
	// about half of the Windows failures on its own.
	boughBin = filepath.Join(dir, "bough"+exeSuffix())
	build := exec.Command("go", "build", "-o", boughBin, "./cmd/bough")
	build.Dir = repoRoot
	if out, err := build.CombinedOutput(); err != nil {
		fmt.Fprintf(os.Stderr, "e2e: build: %v\n%s", err, out)
		os.RemoveAll(dir)
		os.Exit(1)
	}
	code := m.Run()
	os.RemoveAll(dir)
	os.Exit(code)
}

// safeBuf is a concurrency-safe output accumulator (stdout+stderr).
type safeBuf struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (b *safeBuf) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Write(p)
}

func (b *safeBuf) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.String()
}

// launchOpts configures one isolated bough process.
type launchOpts struct {
	sets   []string          // extra --set overrides; llm.plugin=llm-echo is always first
	args   []string          // extra CLI args (e.g. -c, -r <id>)
	home   map[string]string // files under temp HOME, relative path -> content
	cwd    map[string]string // files under temp cwd, relative path -> content
	config string            // replaces the copied bough.yml when non-empty
	from   *bough            // reuse this instance's HOME/cwd/config (session resume tests)
}

// bough is one running (or exited) bough process.
type bough struct {
	t      *testing.T
	home   string
	cwd    string
	config string
	cmd    *exec.Cmd
	stdin  *os.File
	out    *safeBuf
	exited chan error
}

func writeTree(t *testing.T, base string, files map[string]string) {
	t.Helper()
	for rel, content := range files {
		p := filepath.Join(base, rel)
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(p, []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}
}

// sandbox creates the temp HOME + cwd + config copy for one test —
// or reuses a previous instance's (opts.from) for resume round-trips.
func sandbox(t *testing.T, opts launchOpts) (home, cwd, config string) {
	t.Helper()
	if opts.from != nil {
		writeTree(t, opts.from.home, opts.home)
		writeTree(t, opts.from.cwd, opts.cwd)
		return opts.from.home, opts.from.cwd, opts.from.config
	}
	base := t.TempDir()
	home = filepath.Join(base, "home")
	cwd = filepath.Join(base, "cwd")
	for _, d := range []string{home, cwd} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	config = filepath.Join(cwd, "bough.yml")
	content := opts.config
	if content == "" {
		b, err := os.ReadFile(filepath.Join(repoRoot, "bough.yml"))
		if err != nil {
			t.Fatal(err)
		}
		content = string(b)
	}
	if err := os.WriteFile(config, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	writeTree(t, home, opts.home)
	writeTree(t, cwd, opts.cwd)
	return home, cwd, config
}

func env(home string) []string {
	// HOME is replaced and BOUGH_ROOT dropped so no test can reach the
	// real ~/.bough or a real checkout (bough update walks env fallbacks).
	env := []string{"HOME=" + home}
	for _, kv := range os.Environ() {
		if !strings.HasPrefix(kv, "HOME=") && !strings.HasPrefix(kv, "BOUGH_ROOT=") {
			env = append(env, kv)
		}
	}
	return env
}

// launchHeadless starts bough --headless with an open stdin pipe.
func launchHeadless(t *testing.T, opts launchOpts) *bough {
	t.Helper()
	home, cwd, config := sandbox(t, opts)

	args := []string{"--config", "bough.yml", "--set", "llm.plugin=llm-echo"}
	for _, s := range opts.sets {
		args = append(args, "--set", s)
	}
	args = append(args, opts.args...)
	args = append(args, "--headless")

	cmd := exec.Command(boughBin, args...)
	cmd.Dir = cwd
	cmd.Env = env(home)
	out := &safeBuf{}
	cmd.Stdout = out
	cmd.Stderr = out
	stdinR, stdinW, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	cmd.Stdin = stdinR
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	stdinR.Close()

	b := &bough{t: t, home: home, cwd: cwd, config: config, cmd: cmd, stdin: stdinW, out: out, exited: make(chan error, 1)}
	go func() { b.exited <- cmd.Wait() }()
	t.Cleanup(func() {
		stdinW.Close()
		select {
		case <-b.exited:
		default:
			cmd.Process.Kill()
			<-b.exited
		}
	})
	return b
}

func (b *bough) send(line string) {
	b.t.Helper()
	if _, err := fmt.Fprintln(b.stdin, line); err != nil {
		b.t.Fatalf("send %q: %v\noutput:\n%s", line, err, b.out.String())
	}
}

func (b *bough) closeStdin() { b.stdin.Close() }

// waitFor polls the combined output for substr.
func (b *bough) waitFor(substr string) {
	b.t.Helper()
	// Generous: a full-module -race run at max parallelism can starve
	// process startup for many seconds on a loaded machine.
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		if strings.Contains(b.out.String(), substr) {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	b.t.Fatalf("%q not in output after 30s; output:\n%s", substr, b.out.String())
}

// waitExit waits for the process to exit and returns its exit code.
func (b *bough) waitExit() int {
	b.t.Helper()
	select {
	case err := <-b.exited:
		b.exited <- err // keep Cleanup's receive satisfied
		if err == nil {
			return 0
		}
		if ee, ok := err.(*exec.ExitError); ok {
			return ee.ExitCode()
		}
		b.t.Fatalf("wait: %v\noutput:\n%s", err, b.out.String())
		return -1
	case <-time.After(15 * time.Second):
		b.t.Fatalf("process did not exit in 15s; output:\n%s", b.out.String())
		return -1
	}
}

// runHeadless is the one-shot path: send every line, close stdin, wait
// for a clean exit, return the combined output.
func runHeadless(t *testing.T, opts launchOpts, lines ...string) string {
	t.Helper()
	b := launchHeadless(t, opts)
	for _, l := range lines {
		b.send(l)
	}
	b.closeStdin()
	if code := b.waitExit(); code != 0 {
		t.Fatalf("exit code %d; output:\n%s", code, b.out.String())
	}
	return b.out.String()
}

// runCLI execs a bough subcommand (rows, log, ...) in the sandbox and
// returns combined output + exit code.
func runCLI(t *testing.T, home, cwd string, args ...string) (string, int) {
	t.Helper()
	cmd := exec.Command(boughBin, args...)
	cmd.Dir = cwd
	cmd.Env = env(home)
	out, err := cmd.CombinedOutput()
	code := 0
	if err != nil {
		ee, ok := err.(*exec.ExitError)
		if !ok {
			t.Fatalf("exec %v: %v\n%s", args, err, out)
		}
		code = ee.ExitCode()
	}
	return string(out), code
}

// mustContain / mustNotContain / inOrder assert on captured output,
// failing with the full output.
func mustContain(t *testing.T, out string, subs ...string) {
	t.Helper()
	for _, s := range subs {
		if !strings.Contains(out, s) {
			t.Fatalf("output missing %q; output:\n%s", s, out)
		}
	}
}

func mustNotContain(t *testing.T, out string, subs ...string) {
	t.Helper()
	for _, s := range subs {
		if strings.Contains(out, s) {
			t.Fatalf("output unexpectedly contains %q; output:\n%s", s, out)
		}
	}
}

func inOrder(t *testing.T, out string, subs ...string) {
	t.Helper()
	pos := 0
	for _, s := range subs {
		i := strings.Index(out[pos:], s)
		if i < 0 {
			t.Fatalf("%q not found (in order) after byte %d; output:\n%s", s, pos, out)
		}
		pos += i + len(s)
	}
}

var ansiRE = regexp.MustCompile(`\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[@-_]`)

// stripANSI removes escape sequences from raw terminal output.
func stripANSI(s string) string { return ansiRE.ReplaceAllString(s, "") }

// exeSuffix is ".exe" on Windows and "" everywhere else.
func exeSuffix() string {
	if runtime.GOOS == "windows" {
		return ".exe"
	}
	return ""
}
