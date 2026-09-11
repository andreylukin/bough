package vtreal

// Headless mode (`bough --headless`) replayed off a tape: no PTY, no
// TUI, plain pipes. The contract a script depends on is that stdout
// carries the turn's events as "[kind] text" lines and nothing else —
// no terminal escapes, no chrome — that a clean turn exits 0, and
// that a turn whose block failed prints "[error] ..." on stderr and
// exits 1.

import (
	"bytes"
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"
)

type headlessResult struct {
	stdout string
	stderr string
	code   int
}

// screen is what every headless failure prints: there is no terminal,
// so the captured streams are the screen.
func (r headlessResult) screen() string {
	return "exit " + strconv.Itoa(r.code) + "\n--- stdout ---\n" + r.stdout + "--- stderr ---\n" + r.stderr
}

// headlessRun pipes the prompts into `bough --headless` with the
// replay overlay over the given tape, in a fresh $HOME, and returns
// both streams and the exit status.
func headlessRun(t *testing.T, tapeName string, prompts ...string) headlessResult {
	t.Helper()
	tape, err := filepath.Abs(filepath.Join("testdata", "replay", tapeName))
	if err != nil {
		t.Fatal(err)
	}
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(replayConfig(tape)), 0o644); err != nil {
		t.Fatal(err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, bin, "-config", cfg, "--headless")
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", "BOUGH_HEADLESS_IDLE=30",
	)
	cmd.Stdin = strings.NewReader(strings.Join(prompts, "\n") + "\n")
	var out, errb bytes.Buffer
	cmd.Stdout, cmd.Stderr = &out, &errb
	runErr := cmd.Run()
	res := headlessResult{stdout: out.String(), stderr: errb.String()}
	if ee, ok := runErr.(*exec.ExitError); ok {
		res.code = ee.ExitCode()
	} else if runErr != nil {
		t.Fatalf("running bough --headless: %v\n%s", runErr, res.screen())
	}
	if ctx.Err() != nil {
		t.Fatalf("bough --headless never exited:\n%s", res.screen())
	}
	return res
}

// headlessEvents are the "[kind] text" lines of stdout, in order.
func headlessEvents(stdout string) [][2]string {
	var evs [][2]string
	for _, l := range strings.Split(stdout, "\n") {
		if !strings.HasPrefix(l, "[") {
			continue
		}
		kind, text, ok := strings.Cut(strings.TrimPrefix(l, "["), "] ")
		if !ok {
			kind = strings.TrimSuffix(strings.TrimPrefix(l, "["), "]")
		}
		evs = append(evs, [2]string{kind, text})
	}
	return evs
}

func TestHeadless(t *testing.T) {
	t.Parallel()

	t.Run("AnswerOnStdoutExitZero", func(t *testing.T) {
		t.Parallel()
		r := headlessRun(t, "headless_answer.jsonl", "how many go files are here")
		if r.code != 0 {
			t.Errorf("clean tape must exit 0:\n%s", r.screen())
		}
		var answers []string
		sawDone := false
		for _, ev := range headlessEvents(r.stdout) {
			switch ev[0] {
			case "assistant":
				answers = append(answers, ev[1])
			case "done":
				sawDone = true
			}
		}
		if len(answers) != 1 || answers[0] != "Two Go files: a.go and b.go." {
			t.Errorf("stdout must carry exactly the tape's answer, got %q:\n%s", answers, r.screen())
		}
		if !sawDone {
			t.Errorf("stdout must end the turn with [done]:\n%s", r.screen())
		}
		if strings.Contains(r.stderr, "[error]") {
			t.Errorf("clean tape must not print [error]:\n%s", r.screen())
		}
	})

	t.Run("StdoutIsEventLinesOnly", func(t *testing.T) {
		t.Parallel()
		r := headlessRun(t, "headless_answer.jsonl", "how many go files are here")
		// Every stdout line belongs to an event: a "[kind] text" line,
		// or a continuation of the previous event's text. No banner,
		// no prompt, no status bar.
		for _, l := range strings.Split(strings.TrimRight(r.stdout, "\n"), "\n") {
			if strings.HasPrefix(l, "[") || l == "" {
				continue
			}
			t.Errorf("stdout line %q is not part of an event:\n%s", l, r.screen())
		}
		for _, ev := range headlessEvents(r.stdout) {
			switch ev[0] {
			case "assistant", "done", "usage", "input":
			default:
				t.Errorf("unexpected event kind %q on a plain answer turn:\n%s", ev[0], r.screen())
			}
		}
	})

	t.Run("NoEscapeBytes", func(t *testing.T) {
		t.Parallel()
		r := headlessRun(t, "headless_answer.jsonl", "how many go files are here")
		for name, s := range map[string]string{"stdout": r.stdout, "stderr": r.stderr} {
			if i := strings.IndexAny(s, "\x1b\x9b\x07"); i >= 0 {
				t.Errorf("%s carries a terminal escape at byte %d:\n%s", name, i, r.screen())
			}
		}
	})

	t.Run("BlockErrorExitsNonZero", func(t *testing.T) {
		t.Parallel()
		r := headlessRun(t, "headless_error.jsonl", "run the build")
		if r.code == 0 {
			t.Errorf("a turn whose block failed must exit non-zero:\n%s", r.screen())
		}
		if !strings.Contains(r.stderr, "[error] ") || !strings.Contains(r.stderr, "headless-fixture-boom") {
			t.Errorf("the failure must be reported on stderr as [error] ...:\n%s", r.screen())
		}
		if strings.Contains(r.stdout, "[error]") {
			t.Errorf("errors belong on stderr, not stdout:\n%s", r.screen())
		}
		if !strings.Contains(r.stdout, "[done]") {
			t.Errorf("a failed turn still ends with [done] on stdout:\n%s", r.screen())
		}
	})
}
