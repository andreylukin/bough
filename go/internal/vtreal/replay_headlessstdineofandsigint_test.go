package vtreal

// Surface "headless-stdin-eof-and-sigint": `bough --headless` with the
// prompt piped and stdin already closed, then SIGINT mid-stream. The
// tape streams slowly (delay_ms), and the signal goes out only after
// the first event line has been read from stdout, so the turn is in
// flight when it lands. Contract: exit 130 (main.go: a signal that
// cancels a turn), stdout ends on a whole line that is a terminal
// event, and the history file's last turn ends in a cancelled entry.

import (
	"bufio"
	"bytes"
	"encoding/json"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"
)

// headlessStdinEOFAndSIGINTCancelExit is the documented exit code of a
// signal-cancelled headless turn.
const headlessStdinEOFAndSIGINTCancelExit = 130

type headlessStdinEOFAndSIGINTResult struct {
	headlessResult
	home string
}

// headlessStdinEOFAndSIGINTTape: the first reply is a block (printed
// whole as "[assistant]" at once), the second a long reply that
// streams at delay_ms, so the turn is still in flight after the first
// stdout line. A second input whose reply must never print follows.
func headlessStdinEOFAndSIGINTTape(t *testing.T) string {
	t.Helper()
	code := "console.log(1)\n"
	long := "ALPHASTART" + strings.Repeat(" filler", 400)
	ents := []map[string]any{
		{"kind": "meta", "data": map[string]any{"cwd": "/tmp/demo"}},
		{"kind": "input", "data": map[string]any{"text": "start the long one"}},
		{"kind": "assistant", "data": map[string]any{"text": "```js\n" + code + "```"}},
		{"kind": "code", "data": map[string]any{"text": code}},
		{"kind": "result", "data": map[string]any{"code": code, "text": "1\n"}},
		{"kind": "assistant", "data": map[string]any{"text": long}},
		{"kind": "done", "data": map[string]any{"text": ""}},
		{"kind": "input", "data": map[string]any{"text": "second"}},
		{"kind": "assistant", "data": map[string]any{"text": "```stop\nBETAREPLY\n```"}},
		{"kind": "done", "data": map[string]any{"text": ""}},
	}
	var b bytes.Buffer
	for i, e := range ents {
		e["seq"], e["at"] = i+1, "2026-09-11T10:00:00Z"
		l, err := json.Marshal(e)
		if err != nil {
			t.Fatal(err)
		}
		b.Write(append(l, '\n'))
	}
	p := filepath.Join(t.TempDir(), "tape.jsonl")
	if err := os.WriteFile(p, b.Bytes(), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// headlessStdinEOFAndSIGINTRun pipes prompt, closes stdin, waits for
// the first "[kind]" stdout line, then sends SIGINT.
func headlessStdinEOFAndSIGINTRun(t *testing.T, prompt string) headlessStdinEOFAndSIGINTResult {
	t.Helper()
	tape := headlessStdinEOFAndSIGINTTape(t)
	cfgText := strings.Replace(replayConfig(tape), "config: {file: "+strconv.Quote(tape)+"}",
		"config: {file: "+strconv.Quote(tape)+", delay_ms: 200}", 1)
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(cfgText), 0o644); err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg, "--headless")
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", "BOUGH_HEADLESS_IDLE=30",
	)
	cmd.Stdin = strings.NewReader(prompt + "\n") // EOF right after the prompt
	var errb headlessJSONContractBuf
	cmd.Stderr = &errb
	pr, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	var out bytes.Buffer
	first := make(chan struct{})
	readDone := make(chan struct{})
	go func() {
		defer close(readDone)
		br := bufio.NewReader(pr)
		signalled := false
		for {
			l, err := br.ReadString('\n')
			out.WriteString(l)
			if !signalled && strings.HasPrefix(l, "[") && strings.HasSuffix(l, "\n") {
				signalled = true
				close(first)
			}
			if err != nil {
				if err != io.EOF {
					t.Errorf("reading stdout: %v", err)
				}
				return
			}
		}
	}()
	select {
	case <-first:
		cmd.Process.Signal(syscall.SIGINT)
	case <-readDone:
	case <-time.After(30 * time.Second):
		cmd.Process.Kill()
	}
	<-readDone
	done := make(chan error, 1)
	go func() { done <- cmd.Wait() }()
	var runErr error
	select {
	case runErr = <-done:
	case <-time.After(30 * time.Second):
		cmd.Process.Kill()
		runErr = <-done
		t.Errorf("bough --headless never exited after SIGINT")
	}
	res := headlessStdinEOFAndSIGINTResult{headlessResult{stdout: out.String(), stderr: errb.String()}, home}
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

// headlessStdinEOFAndSIGINTHistory returns the entry kinds of the one
// history file, checking every line is whole JSON.
func headlessStdinEOFAndSIGINTHistory(t *testing.T, r headlessStdinEOFAndSIGINTResult) []string {
	t.Helper()
	paths, _ := filepath.Glob(filepath.Join(r.home, ".bough", "history", "*.jsonl"))
	if len(paths) != 1 {
		t.Fatalf("want one history file, got %v\n%s", paths, r.screen())
	}
	raw, err := os.ReadFile(paths[0])
	if err != nil {
		t.Fatal(err)
	}
	if len(raw) == 0 || raw[len(raw)-1] != '\n' {
		t.Errorf("history ends mid-line: %q", raw[max(0, len(raw)-200):])
	}
	var kinds []string
	for i, l := range bytes.Split(bytes.TrimRight(raw, "\n"), []byte("\n")) {
		var e struct{ Kind string }
		if err := json.Unmarshal(l, &e); err != nil {
			t.Errorf("history line %d is not whole JSON (%v): %q", i+1, err, l)
			continue
		}
		kinds = append(kinds, e.Kind)
	}
	return kinds
}

func TestHeadlessStdinEOFAndSIGINT(t *testing.T) {
	t.Parallel()
	r := headlessStdinEOFAndSIGINTRun(t, "start the long one")

	t.Run("ExitCode130", func(t *testing.T) {
		if r.code != headlessStdinEOFAndSIGINTCancelExit {
			t.Errorf("SIGINT mid-turn: want exit %d, got %d:\n%s", headlessStdinEOFAndSIGINTCancelExit, r.code, r.screen())
		}
	})

	t.Run("StreamEndsOnTerminalEvent", func(t *testing.T) {
		if r.stdout == "" || !strings.HasSuffix(r.stdout, "\n") {
			t.Fatalf("stdout ends on a truncated line:\n%s", r.screen())
		}
		lines := strings.Split(strings.TrimRight(r.stdout, "\n"), "\n")
		lastEv := ""
		for _, l := range lines {
			// [usage] is [done]'s trailer, not an event of its own.
			if m := headlessJSONContractEvent.FindStringSubmatch(l); m != nil && m[1] != "usage" {
				lastEv = m[1]
			}
		}
		if lastEv != "done" && lastEv != "cancelled" {
			t.Errorf("stdout's last event is %q, want a terminal [done]/[cancelled]:\n%s", lastEv, r.screen())
		}
		if strings.Contains(r.stdout, "BETAREPLY") {
			t.Errorf("nothing may run after SIGINT:\n%s", r.screen())
		}
	})

	t.Run("HistoryMarkedCancelled", func(t *testing.T) {
		kinds := headlessStdinEOFAndSIGINTHistory(t, r)
		last := -1
		for i, k := range kinds {
			if k == "input" {
				last = i
			}
		}
		cancelled := false
		for _, k := range kinds[last+1:] {
			cancelled = cancelled || k == "cancelled"
		}
		if last < 0 || !cancelled {
			t.Errorf("last turn has no cancelled entry: kinds %v\n%s", kinds, r.screen())
		}
	})
}
