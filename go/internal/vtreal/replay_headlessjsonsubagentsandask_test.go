package vtreal

// Headless runs whose tape spawns a subagent (tools.spawn) and then
// blocks on tools.ask. The model side comes off the tape; codemode,
// workers and ask are the REAL rows, so the subagent actually runs
// (it consumes the tape's second reply) and the ask actually blocks
// on stdin. Asserted: a --json stream is NDJSON line by line, the
// subagent's events are tagged as such, an ask answered on stdin
// resumes the turn, an ask nobody answers ends in a documented
// non-interactive failure (non-zero exit, "[error]"), and stdout never
// carries a terminal escape.

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

const headlessJSONSubagentsAndAskKnown = "BOUGH_KNOWN_HEADLESS_JSON_SUBAGENTS_AND_ASK"

// headlessJSONSubagentsAndAskConfig overlays only the model (replay)
// and a short ask timeout on the embedded default, so codemode,
// workers and ask are real.
func headlessJSONSubagentsAndAskConfig(tape string) string {
	return fmt.Sprintf(`
- id: llm
  plugin: replay
  config: {file: %q}
- id: ask
  plugin: ask
  config: {timeout_minutes: 1}
- id: session-title
  plugin: session-title
  disabled: true
- id: auto-memory
  plugin: auto-memory
  disabled: true
- id: memory-tier
  plugin: memory-tier
  disabled: true
- id: activity
  plugin: activity
  disabled: true
- id: attention
  plugin: attention
  disabled: true
`, tape)
}

// headlessJSONSubagentsAndAskRun sends the tape's prompt, answers the
// ask with answer once "[ask] Pick a color" shows on stdout (answer "" =
// never answer), then closes stdin.
func headlessJSONSubagentsAndAskRun(t *testing.T, answer string, idle int, extra ...string) headlessResult {
	t.Helper()
	tape, err := filepath.Abs(filepath.Join("testdata", "replay", "headless-json-subagents-ask.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(headlessJSONSubagentsAndAskConfig(tape)), 0o644); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 120*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, bin, append([]string{"-config", cfg, "--headless"}, extra...)...)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", fmt.Sprintf("BOUGH_HEADLESS_IDLE=%d", idle),
	)
	stdin, err := cmd.StdinPipe()
	if err != nil {
		t.Fatal(err)
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	var errb bytes.Buffer
	cmd.Stderr = &errb
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	io.WriteString(stdin, "delegate then ask\n")
	var once sync.Once
	closeIn := func() { once.Do(func() { stdin.Close() }) }
	if answer == "" {
		closeIn()
	}
	var out bytes.Buffer
	sc := bufio.NewScanner(stdout)
	for sc.Scan() {
		out.WriteString(sc.Text() + "\n")
		isAsk := strings.HasPrefix(sc.Text(), "[ask] Pick a color") ||
			strings.Contains(sc.Text(), `"kind":"ask"`) && strings.Contains(sc.Text(), "Pick a color")
		if answer != "" && isAsk {
			once.Do(func() {
				io.WriteString(stdin, answer+"\n")
				stdin.Close()
			})
		}
	}
	closeIn()
	runErr := cmd.Wait()
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

func headlessJSONSubagentsAndAskNoEscapes(t *testing.T, r headlessResult) {
	t.Helper()
	if i := strings.IndexAny(r.stdout, "\x1b\x9b\x07"); i >= 0 {
		t.Errorf("stdout carries a terminal escape at byte %d:\n%s", i, r.screen())
	}
}

func TestHeadlessJSONSubagentsAndAsk(t *testing.T) {
	t.Parallel()

	t.Run("PlainSubagentTaggedAskAnswered", func(t *testing.T) {
		t.Parallel()
		r := headlessJSONSubagentsAndAskRun(t, "2", 30)
		if r.code != 0 {
			t.Errorf("an answered ask must end the turn cleanly (exit 0):\n%s", r.screen())
		}
		headlessJSONSubagentsAndAskNoEscapes(t, r)
		var kinds []string
		for _, ev := range headlessEvents(r.stdout) {
			kinds = append(kinds, ev[0])
		}
		seq := strings.Join(kinds, ",")
		for _, want := range []string{"sub:start", "sub:assistant", "sub:done", "ask", "result", "done"} {
			if !strings.Contains(","+seq+",", ","+want+",") {
				t.Errorf("missing [%s] event (kinds %s):\n%s", want, seq, r.screen())
			}
		}
		if strings.Index(seq, "sub:done") > strings.Index(seq, "ask") {
			t.Errorf("the subagent finishes before the ask (kinds %s)", seq)
		}
		if !strings.Contains(r.stdout, "[sub:start] count the widgets") {
			t.Errorf("sub:start must carry the task:\n%s", r.screen())
		}
		if !strings.Contains(r.stdout, "you picked vermilion") {
			t.Errorf("answer 2 must resolve to option 2:\n%s", r.screen())
		}
		if !strings.Contains(r.stdout, "[assistant] Color locked in.") {
			t.Errorf("the turn must resume to the tape's final reply:\n%s", r.screen())
		}
		if strings.Contains(r.stderr, "[error]") {
			t.Errorf("clean run must not print [error]:\n%s", r.screen())
		}
	})

	t.Run("JSONStreamIsNDJSON", func(t *testing.T) {
		t.Parallel()
		r := headlessJSONSubagentsAndAskRun(t, "2", 30, "--json")
		if r.code != 0 {
			t.Fatalf("--json run must exit 0:\n%s", r.screen())
		}
		headlessJSONSubagentsAndAskNoEscapes(t, r)
		sawSub := false
		for i, l := range strings.Split(strings.TrimRight(r.stdout, "\n"), "\n") {
			var obj map[string]any
			if err := json.Unmarshal([]byte(l), &obj); err != nil {
				t.Errorf("stdout line %d is not a JSON object: %q (%v)", i+1, l, err)
				continue
			}
			if k, _ := obj["kind"].(string); strings.HasPrefix(k, "sub:") {
				sawSub = true
			}
		}
		if !sawSub {
			t.Errorf("subagent events must be tagged kind sub:*:\n%s", r.screen())
		}
	})

	t.Run("UnansweredAskAtEOFFails", func(t *testing.T) {
		t.Parallel()
		r := headlessJSONSubagentsAndAskRun(t, "", 5)
		headlessJSONSubagentsAndAskNoEscapes(t, r)
		if !strings.Contains(r.stdout, "[ask] Pick a color") {
			t.Fatalf("the ask must be printed before EOF handling:\n%s", r.screen())
		}
		if r.code == 0 {
			t.Errorf("an ask nobody can answer must not exit 0:\n%s", r.screen())
		}
		if !strings.Contains(r.stderr, "[error]") {
			t.Errorf("an abandoned ask must be reported as [error] on stderr:\n%s", r.screen())
		}
	})
}
