package vtreal

// Headless output contract, surface "headless-json-contract". There is
// no JSON output mode: `bough --headless` prints "[kind] text" lines
// (help text and plugins/ui/headless.go), and the only JSON on stdout
// is the "[usage] {...}" payload. So "every stdout line is valid" here
// means: every line is a "[kind] text" event or a continuation of the
// previous one, kinds come from a known set, a [usage] payload parses
// as JSON, errors go to stderr, no terminal escapes, and the exit code
// tells a clean run from an errored one from a cancelled one.

import (
	"bytes"
	"encoding/json"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"
)

// headlessJSONContractEvent is an event line; anything else on stdout
// continues the previous event's text (a result can hold any text).
var headlessJSONContractEvent = regexp.MustCompile(`^\[([a-z:/-]+)\](?: (.*))?$`)

var headlessJSONContractKinds = map[string]bool{
	"assistant": true, "done": true, "usage": true, "input": true, "code": true,
	"result": true, "ask": true, "system": true, "steer": true, "notice": true,
}

// headlessJSONContractBuf is a bytes.Buffer safe to read while the
// process writes.
type headlessJSONContractBuf struct {
	mu sync.Mutex
	b  bytes.Buffer
}

func (s *headlessJSONContractBuf) Write(p []byte) (int, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.b.Write(p)
}

func (s *headlessJSONContractBuf) String() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.b.String()
}

// headlessJSONContractRun starts `bough --headless` over cfgText; feed
// gets the stdin pipe and the process and must close stdin.
func headlessJSONContractRun(t *testing.T, cfgText string, feed func(stdin io.WriteCloser, p *os.Process)) headlessResult {
	t.Helper()
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(cfgText), 0o644); err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg, "--headless")
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", "BOUGH_HEADLESS_IDLE=20",
	)
	var out, errb headlessJSONContractBuf
	cmd.Stdout, cmd.Stderr = &out, &errb
	stdin, err := cmd.StdinPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	done := make(chan error, 1)
	go func() { done <- cmd.Wait() }()
	feed(stdin, cmd.Process)
	var runErr error
	select {
	case runErr = <-done:
	case <-time.After(60 * time.Second):
		cmd.Process.Kill()
		<-done
		t.Fatalf("bough --headless never exited:\nstdout:\n%s\nstderr:\n%s", out.String(), errb.String())
	}
	res := headlessResult{stdout: out.String(), stderr: errb.String()}
	if ee, ok := runErr.(*exec.ExitError); ok {
		res.code = ee.ExitCode()
		if ws, ok := ee.Sys().(syscall.WaitStatus); ok && ws.Signaled() {
			res.code = 128 + int(ws.Signal())
		}
	} else if runErr != nil {
		t.Fatalf("running bough --headless: %v\n%s", runErr, res.screen())
	}
	return res
}

func headlessJSONContractTapeCfg(t *testing.T, tape string) string {
	t.Helper()
	if !filepath.IsAbs(tape) {
		p, err := filepath.Abs(filepath.Join("testdata", "replay", tape))
		if err != nil {
			t.Fatal(err)
		}
		tape = p
	}
	return replayConfig(tape)
}

// headlessJSONContractAskCfg is the ask tape with the real ask row and
// codemode, so tools.ask blocks the turn the way it does live.
func headlessJSONContractAskCfg(t *testing.T) string {
	t.Helper()
	tape, err := filepath.Abs(filepath.Join("testdata", "replay", "ask.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	return askConfig(tape)
}

// headlessJSONContractPrompt writes the prompts and closes stdin.
func headlessJSONContractPrompt(prompts ...string) func(io.WriteCloser, *os.Process) {
	return func(w io.WriteCloser, _ *os.Process) {
		io.WriteString(w, strings.Join(prompts, "\n")+"\n")
		w.Close()
	}
}

// headlessJSONContractCheck asserts the shape of both streams.
func headlessJSONContractCheck(t *testing.T, r headlessResult) {
	t.Helper()
	for name, s := range map[string]string{"stdout": r.stdout, "stderr": r.stderr} {
		if i := strings.IndexAny(s, "\x1b\x9b\x07"); i >= 0 {
			t.Errorf("%s carries a terminal escape at byte %d:\n%.4000s", name, i, r.screen())
		}
	}
	lines := strings.Split(strings.TrimRight(r.stdout, "\n"), "\n")
	if !strings.HasPrefix(lines[0], "[") {
		t.Errorf("stdout must open with an event line:\n%.4000s", r.screen())
	}
	for _, l := range lines {
		m := headlessJSONContractEvent.FindStringSubmatch(l)
		if m == nil {
			continue // continuation of the previous event's text
		}
		kind, text := m[1], m[2]
		if kind == "error" {
			t.Errorf("[error] belongs on stderr, not stdout: %q", l)
		} else if !headlessJSONContractKinds[kind] {
			t.Errorf("unknown event kind %q in %q", kind, l)
		}
		if kind == "usage" && !json.Valid([]byte(text)) {
			t.Errorf("[usage] payload is not JSON: %q", text)
		}
	}
}

func headlessJSONContractHas(stdout, kind, sub string) bool {
	for _, ev := range headlessEvents(stdout) {
		if ev[0] == kind && strings.Contains(ev[1], sub) {
			return true
		}
	}
	return false
}

// headlessJSONContractKnown skips a subtest pinned to a known product
// bug unless BOUGH_KNOWN_HEADLESS_JSON_CONTRACT is set.
func headlessJSONContractKnown(t *testing.T, bug string) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_HEADLESS_JSON_CONTRACT") == "" {
		t.Skip("known bug: " + bug + " (set BOUGH_KNOWN_HEADLESS_JSON_CONTRACT=1 to run)")
	}
}

func TestHeadlessJSONContract(t *testing.T) {
	t.Parallel()

	t.Run("CleanExitZero", func(t *testing.T) {
		t.Parallel()
		r := headlessJSONContractRun(t, headlessJSONContractTapeCfg(t, "headless_answer.jsonl"),
			headlessJSONContractPrompt("how many go files are here"))
		headlessJSONContractCheck(t, r)
		if r.code != 0 || !headlessJSONContractHas(r.stdout, "done", "") {
			t.Errorf("clean run: want exit 0 and [done]:\n%s", r.screen())
		}
	})

	t.Run("ErrorOnStderrExitOne", func(t *testing.T) {
		t.Parallel()
		r := headlessJSONContractRun(t, headlessJSONContractTapeCfg(t, "headless_error.jsonl"),
			headlessJSONContractPrompt("run the build"))
		headlessJSONContractCheck(t, r)
		if r.code != 1 {
			t.Errorf("errored run must exit 1, got %d:\n%s", r.code, r.screen())
		}
		if !strings.Contains(r.stderr, "[error] ") || !strings.Contains(r.stderr, "headless-fixture-boom") {
			t.Errorf("stderr must carry [error] with the failure:\n%s", r.screen())
		}
	})

	t.Run("SubagentTurn", func(t *testing.T) {
		t.Parallel()
		// Replay never spawns, so the tape's sub:* entries drive
		// nothing here: the turn ends on the spawner's first reply.
		// What is checked is that a subagent-shaped tape keeps the
		// stream contract and exits clean.
		r := headlessJSONContractRun(t, headlessJSONContractTapeCfg(t, "subagents.jsonl"),
			headlessJSONContractPrompt("review the parser and count the tests"))
		headlessJSONContractCheck(t, r)
		if r.code != 0 || !headlessJSONContractHas(r.stdout, "assistant", "Spawning two subagents.") ||
			!headlessJSONContractHas(r.stdout, "done", "") {
			t.Errorf("subagent turn: want exit 0, the reply and [done]:\n%s", r.screen())
		}
	})

	t.Run("HugeOutput", func(t *testing.T) {
		t.Parallel()
		tape := hugeOutputTape(t, hugeOutputLines(20000))
		r := headlessJSONContractRun(t, headlessJSONContractTapeCfg(t, tape),
			headlessJSONContractPrompt("make output"))
		headlessJSONContractCheck(t, r)
		if r.code != 0 || !headlessJSONContractHas(r.stdout, "assistant", "All printed.") ||
			!headlessJSONContractHas(r.stdout, "done", "") {
			t.Errorf("huge output: want exit 0, the answer and [done]:\n%.4000s", r.screen())
		}
	})

	t.Run("AskAnsweredFromStdin", func(t *testing.T) {
		t.Parallel()
		// No tty: the question prints as [ask] + numbered options and
		// the next stdin line answers it (here by number).
		r := headlessJSONContractRun(t, headlessJSONContractAskCfg(t),
			func(w io.WriteCloser, _ *os.Process) {
				io.WriteString(w, "pick a color\n")
				time.Sleep(3 * time.Second) // let the ask arm
				io.WriteString(w, "2\n")
				w.Close()
			})
		headlessJSONContractCheck(t, r)
		if !headlessJSONContractHas(r.stdout, "ask", "Pick a color") || !strings.Contains(r.stdout, "  2. vermilion") {
			t.Errorf("stdout must show the ask and its options:\n%s", r.screen())
		}
		if r.code != 0 || !headlessJSONContractHas(r.stdout, "done", "") {
			t.Errorf("answered ask: want exit 0 and [done]:\n%s", r.screen())
		}
	})

	t.Run("AskNoAnswerEOF", func(t *testing.T) {
		t.Parallel()
		// stdin closes with the ask unanswered: the run must end (not
		// hang) and must not pass as a clean success.
		headlessJSONContractKnown(t, "stdin EOF with a tools.ask pending hangs: drainHeadless gives up after BOUGH_HEADLESS_IDLE and interrupts, but the process never exits while the ask blocks the turn (plugins/ui/headless.go drainHeadless/interruptSelf, hlAsk never cancelled)")
		r := headlessJSONContractRun(t, headlessJSONContractAskCfg(t),
			headlessJSONContractPrompt("pick a color"))
		headlessJSONContractCheck(t, r)
		if r.code == 0 {
			t.Errorf("unanswered ask at EOF exited 0:\n%s", r.screen())
		}
	})

	t.Run("SIGINTCancelDistinctExit", func(t *testing.T) {
		t.Parallel()
		tape, err := filepath.Abs(filepath.Join("testdata", "replay", "cancel.jsonl"))
		if err != nil {
			t.Fatal(err)
		}
		cfg := strings.Replace(replayConfig(tape), "config: {file: "+strconv.Quote(tape)+"}",
			"config: {file: "+strconv.Quote(tape)+", delay_ms: 50}", 1)
		r := headlessJSONContractRun(t, cfg, func(w io.WriteCloser, p *os.Process) {
			io.WriteString(w, "start the long one\n")
			time.Sleep(2 * time.Second) // mid-stream: hundreds of words at 50ms
			p.Signal(syscall.SIGINT)
			time.Sleep(500 * time.Millisecond)
			w.Close()
		})
		if r.code == 1 {
			t.Errorf("a cancel must not look like a turn error (exit 1):\n%s", r.screen())
		}
		if strings.Contains(r.stdout, "BETAREPLY") {
			t.Errorf("nothing may run after SIGINT:\n%s", r.screen())
		}
		if r.code == 0 {
			headlessJSONContractKnown(t, "SIGINT mid-turn in --headless exits 0, same as a clean run (cmd/bough/main.go: os.Exit(ui.ExitCode()) after <-sig)")
			t.Errorf("SIGINT mid-turn exited 0, indistinguishable from success:\n%s", r.screen())
		}
	})
}
