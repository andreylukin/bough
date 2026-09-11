package vtreal

// Surface "headless-json-provider-error-exit": a provider 500 (a
// recorded model-call error that ends the turn) under headless mode.
// The run must end nonzero with the error on stderr, no terminal
// escapes on either stream, and a well-formed stdout. The `--json`
// variant gets JSONL on stdout; the error is a {"kind":"error"} JSON
// line on stderr (main's --json contract, 719af608).

import (
	"encoding/json"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"
)

const headlessJSONProviderErrorExitTape = `{"seq":1,"at":"2026-09-10T10:00:00Z","kind":"meta","data":{"cwd":"/tmp/demo"}}
{"seq":2,"at":"2026-09-10T10:00:01Z","kind":"input","data":{"text":"say hi"}}
{"seq":3,"at":"2026-09-10T10:00:02Z","kind":"error","data":{"text":"500 Internal Server Error: provider-fixture-500"}}
{"seq":4,"at":"2026-09-10T10:00:02Z","kind":"done","data":{"text":""}}
{"seq":5,"at":"2026-09-10T10:00:03Z","kind":"input","data":{"text":"never sent"}}
{"seq":6,"at":"2026-09-10T10:00:04Z","kind":"assistant","data":{"text":"` + "```stop\\nunreached\\n```" + `"}}
{"seq":7,"at":"2026-09-10T10:00:04Z","kind":"done","data":{"text":""}}
`

// The trailing turn is never reached (one prompt is fed); it is there
// because the replay loader rejects a tape with no assistant entry.

// headlessJSONProviderErrorExitRun runs bough with extra args over the
// 500 tape, feeding one prompt then EOF.
func headlessJSONProviderErrorExitRun(t *testing.T, args ...string) headlessResult {
	t.Helper()
	home := t.TempDir()
	tape := filepath.Join(home, "provider500.jsonl")
	if err := os.WriteFile(tape, []byte(headlessJSONProviderErrorExitTape), 0o644); err != nil {
		t.Fatal(err)
	}
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(replayConfig(tape)), 0o644); err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, append([]string{"-config", cfg}, args...)...)
	cmd.Dir = home
	cmd.Env = append(os.Environ(), "HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", "BOUGH_HEADLESS_IDLE=20")
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
	io.WriteString(stdin, "say hi\n")
	stdin.Close()
	var runErr error
	select {
	case runErr = <-done:
	case <-time.After(60 * time.Second):
		cmd.Process.Kill()
		<-done
		t.Fatalf("bough never exited:\nstdout:\n%s\nstderr:\n%s", out.String(), errb.String())
	}
	r := headlessResult{stdout: out.String(), stderr: errb.String()}
	if ee, ok := runErr.(*exec.ExitError); ok {
		r.code = ee.ExitCode()
		if ws, ok := ee.Sys().(syscall.WaitStatus); ok && ws.Signaled() {
			r.code = 128 + int(ws.Signal())
		}
	} else if runErr != nil {
		t.Fatalf("running bough: %v", runErr)
	}
	return r
}

func headlessJSONProviderErrorExitNoANSI(t *testing.T, r headlessResult) {
	t.Helper()
	for name, s := range map[string]string{"stdout": r.stdout, "stderr": r.stderr} {
		if i := strings.IndexAny(s, "\x1b\x9b\x07"); i >= 0 {
			t.Errorf("%s carries a terminal escape at byte %d:\n%s", name, i, r.screen())
		}
	}
}

func TestHeadlessJSONProviderErrorExit(t *testing.T) {
	t.Parallel()

	t.Run("HeadlessText", func(t *testing.T) {
		t.Parallel()
		r := headlessJSONProviderErrorExitRun(t, "--headless")
		headlessJSONContractCheck(t, r)
		headlessJSONProviderErrorExitNoANSI(t, r)
		if r.code == 0 {
			t.Errorf("provider 500 exited 0:\n%s", r.screen())
		}
		if !strings.Contains(r.stderr, "[error] ") || !strings.Contains(r.stderr, "provider-fixture-500") {
			t.Errorf("stderr must carry [error] with the provider failure:\n%s", r.screen())
		}
	})

	t.Run("JSONL", func(t *testing.T) {
		t.Parallel()
		r := headlessJSONProviderErrorExitRun(t, "--headless", "--json")
		headlessJSONProviderErrorExitNoANSI(t, r)
		if r.code == 0 {
			t.Errorf("provider 500 exited 0:\n%s", r.screen())
		}
		lines := strings.Split(strings.TrimRight(r.stdout, "\n"), "\n")
		sawErr := false
		for i, l := range lines {
			var ev map[string]any
			if err := json.Unmarshal([]byte(l), &ev); err != nil {
				t.Fatalf("stdout line %d is not JSON (%v): %q\n%s", i+1, err, l, r.screen())
			}
		}
		for _, l := range strings.Split(r.stderr, "\n") {
			var ev map[string]any
			if json.Unmarshal([]byte(l), &ev) == nil && ev["kind"] == "error" {
				if s, _ := ev["text"].(string); strings.Contains(s, "provider-fixture-500") {
					sawErr = true
				}
			}
		}
		if !sawErr {
			t.Errorf("no error event carrying the provider failure:\n%s", r.screen())
		}
	})
}
