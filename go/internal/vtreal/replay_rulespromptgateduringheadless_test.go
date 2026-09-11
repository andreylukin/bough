package vtreal

// rules-prompt-gate-during-headless: a Codex prefix_rule with decision
// "prompt" gates `touch`, and the replayed model runs it through a
// REAL codemode under `bough --headless` (no TTY). Nobody at a
// keyboard can answer the gate, so the command must fail closed: the
// sentinel stays absent, the refusal reason shows on stdout, and bough
// exits within the timeout instead of waiting on input.

import (
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"
)

const rulesPromptGateDuringHeadlessAck = "RPGH-ACK leaving it."

// rulesPromptGateDuringHeadlessRun writes the tape, the config (replay
// llm, real codemode) and the rule into a fresh $HOME, pipes one
// prompt into `bough --headless`, holds stdin open for keepOpen, and
// returns the streams and the sentinel path.
func rulesPromptGateDuringHeadlessRun(t *testing.T, keepOpen time.Duration) (headlessResult, string) {
	t.Helper()
	home := t.TempDir()
	sentinel := filepath.Join(home, "SENTINEL-rpgh")
	code := fmt.Sprintf("tools.bash(%q)\n", "touch "+sentinel)
	entries := []map[string]any{
		{"kind": "input", "data": map[string]any{"text": "mark the build"}},
		{"kind": "assistant", "data": map[string]any{"text": "Marking it.\n\n```js\n" + code + "```"}},
		{"kind": "result", "data": map[string]any{"code": code, "text": "TAPE-RESULT-UNUSED\n"}},
		// A stop straight after a failed block is nudged once.
		{"kind": "assistant", "data": map[string]any{"text": "```stop\n" + rulesPromptGateDuringHeadlessAck + "\n```"}},
		{"kind": "assistant", "data": map[string]any{"text": "```stop\n" + rulesPromptGateDuringHeadlessAck + "\n```"}},
		{"kind": "done", "data": map[string]any{"text": ""}},
	}
	var sb strings.Builder
	for i, e := range entries {
		e["seq"] = i + 1
		e["at"] = "2026-09-10T10:00:00Z"
		b, err := json.Marshal(e)
		if err != nil {
			t.Fatal(err)
		}
		sb.Write(b)
		sb.WriteByte('\n')
	}
	tape := filepath.Join(home, "rpgh.jsonl")
	yml := strings.Replace(replayConfig(tape),
		fmt.Sprintf("  plugin: replay\n  config: {file: %q, provide: codemode}\n", tape),
		"  plugin: codemode\n- id: tools\n  plugin: tools-basic\n", 1)
	if !strings.Contains(yml, "plugin: codemode\n") {
		t.Fatalf("real codemode not swapped in:\n%s", yml)
	}
	rule := "prefix_rule(pattern = [\"touch\"], decision = \"prompt\", justification = \"RPGH-MARKER no touching\")\n"
	files := map[string]string{
		tape:                             sb.String(),
		filepath.Join(home, "bough.yml"): yml,
		filepath.Join(home, ".codex", "rules", "team.rules"): rule,
	}
	for p, s := range files {
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(p, []byte(s), 0o644); err != nil {
			t.Fatal(err)
		}
	}

	cmd := exec.Command(bin, "-config", filepath.Join(home, "bough.yml"), "--headless")
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "NO_COLOR=", "BOUGH_VERBOSE=", "BOUGH_HEADLESS_IDLE=20",
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
	io.WriteString(stdin, "mark the build\n")
	if keepOpen > 0 {
		select {
		case <-time.After(keepOpen):
		case err := <-done:
			done <- err
		}
	}
	stdin.Close()
	var runErr error
	select {
	case runErr = <-done:
	case <-time.After(60 * time.Second):
		cmd.Process.Kill()
		<-done
		t.Fatalf("bough --headless hung at the prompt gate:\nstdout:\n%s\nstderr:\n%s", out.String(), errb.String())
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
	return res, sentinel
}

func rulesPromptGateDuringHeadlessCheck(t *testing.T, r headlessResult, sentinel string) {
	t.Helper()
	t.Run("NeverRuns", func(t *testing.T) {
		if _, err := os.Stat(sentinel); err == nil {
			t.Errorf("gated command ran headless: %s exists\n%s", sentinel, r.screen())
		}
	})
	t.Run("ReasonVisible", func(t *testing.T) {
		if !strings.Contains(r.stdout, "RPGH-MARKER") && !strings.Contains(r.stdout, "command not run") && !strings.Contains(r.stdout, "command refused") {
			t.Errorf("no refusal reason on stdout:\n%s", r.screen())
		}
	})
	t.Run("TurnEnds", func(t *testing.T) {
		if !strings.Contains(r.stdout, "[done]") {
			t.Errorf("turn never reached [done]:\n%s", r.screen())
		}
		if i := strings.IndexAny(r.stdout, "\x1b\x9b"); i >= 0 {
			t.Errorf("stdout carries a terminal escape at byte %d:\n%s", i, r.screen())
		}
	})
}

// stdin closes right after the prompt: no line can ever answer.
func TestRulesPromptGateDuringHeadlessStdinClosed(t *testing.T) {
	t.Parallel()
	start := time.Now()
	r, sentinel := rulesPromptGateDuringHeadlessRun(t, 0)
	t.Logf("exit %d after %s", r.code, time.Since(start).Round(time.Millisecond))
	rulesPromptGateDuringHeadlessCheck(t, r, sentinel)
}

// stdin stays open but silent for a while (a harness that never
// answers), then closes; the command must still not run.
func TestRulesPromptGateDuringHeadlessStdinSilent(t *testing.T) {
	t.Parallel()
	r, sentinel := rulesPromptGateDuringHeadlessRun(t, 5*time.Second)
	rulesPromptGateDuringHeadlessCheck(t, r, sentinel)
}
