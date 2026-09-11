package vtreal

// Surface "headless-json-cancel-sigterm-partial-history": `bough
// --headless --json` (bough has no -p; --headless is its print mode)
// over a tape whose long reply streams at delay_ms, SIGTERM sent once
// the first JSONL event line is read (headless never prints
// assistant-delta, so the first line is the whole first block). The
// run is in its own process group so leftovers can be counted.
// Contract: every stdout line is whole JSON, the last event is
// terminal, exit 130 on each run, the history file's last turn holds a
// cancelled entry, and nothing of the group outlives bough.

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

type headlessJSONCancelSIGTERMPartialHistoryResult struct {
	headlessResult
	home string
	pgid int
}

func headlessJSONCancelSIGTERMPartialHistoryRun(t *testing.T) headlessJSONCancelSIGTERMPartialHistoryResult {
	t.Helper()
	tape := headlessStdinEOFAndSIGINTTape(t) // block, then a slow long reply
	cfgText := strings.Replace(replayConfig(tape), "config: {file: "+strconv.Quote(tape)+"}",
		"config: {file: "+strconv.Quote(tape)+", delay_ms: 200}", 1)
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(cfgText), 0o644); err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg, "--headless", "--json")
	cmd.Dir = home
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	cmd.Env = append(os.Environ(), "HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", "BOUGH_HEADLESS_IDLE=30")
	cmd.Stdin = strings.NewReader("start the long one\n")
	var errb headlessJSONContractBuf
	cmd.Stderr = &errb
	pr, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	pgid := cmd.Process.Pid
	t.Cleanup(func() { syscall.Kill(-pgid, syscall.SIGKILL) })
	var out bytes.Buffer
	first, readDone := make(chan struct{}), make(chan struct{})
	go func() {
		defer close(readDone)
		br := bufio.NewReader(pr)
		signalled := false
		for {
			l, err := br.ReadString('\n')
			out.WriteString(l)
			if !signalled && strings.HasSuffix(l, "\n") && json.Valid([]byte(l)) {
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
		cmd.Process.Signal(syscall.SIGTERM)
	case <-readDone:
	case <-time.After(30 * time.Second):
		syscall.Kill(-pgid, syscall.SIGKILL)
	}
	<-readDone
	done := make(chan error, 1)
	go func() { done <- cmd.Wait() }()
	var runErr error
	select {
	case runErr = <-done:
	case <-time.After(30 * time.Second):
		syscall.Kill(-pgid, syscall.SIGKILL)
		runErr = <-done
		t.Errorf("bough never exited after SIGTERM")
	}
	r := headlessJSONCancelSIGTERMPartialHistoryResult{headlessResult{stdout: out.String(), stderr: errb.String()}, home, pgid}
	if ee, ok := runErr.(*exec.ExitError); ok {
		r.code = ee.ExitCode()
		if ws, ok := ee.Sys().(syscall.WaitStatus); ok && ws.Signaled() {
			r.code = 128 + int(ws.Signal())
		}
	} else if runErr != nil {
		t.Fatalf("running bough: %v\n%s", runErr, r.screen())
	}
	return r
}

func TestHeadlessJSONCancelSIGTERMPartialHistory(t *testing.T) {
	t.Parallel()
	r := headlessJSONCancelSIGTERMPartialHistoryRun(t)

	t.Run("StdoutWholeJSONLEndingTerminal", func(t *testing.T) {
		if r.stdout != "" && !strings.HasSuffix(r.stdout, "\n") {
			t.Fatalf("stdout ends on a truncated line:\n%s", r.screen())
		}
		last := ""
		for i, l := range strings.Split(strings.TrimRight(r.stdout, "\n"), "\n") {
			if l == "" {
				continue
			}
			var ev struct{ Kind string }
			if err := json.Unmarshal([]byte(l), &ev); err != nil {
				t.Fatalf("stdout line %d is not JSON (%v): %q\n%s", i+1, err, l, r.screen())
			}
			if ev.Kind != "usage" { // [done]'s trailer
				last = ev.Kind
			}
		}
		if last != "done" && last != "cancelled" {
			t.Errorf("last event %q, want done/cancelled:\n%s", last, r.screen())
		}
		if strings.Contains(r.stdout, "BETAREPLY") {
			t.Errorf("a later turn ran despite SIGTERM:\n%s", r.screen())
		}
	})

	t.Run("ExitCodeStable130", func(t *testing.T) {
		if r.code != headlessStdinEOFAndSIGINTCancelExit {
			t.Fatalf("SIGTERM mid-turn: want exit %d, got %d:\n%s", headlessStdinEOFAndSIGINTCancelExit, r.code, r.screen())
		}
		r2 := headlessJSONCancelSIGTERMPartialHistoryRun(t)
		if r2.code != r.code {
			t.Errorf("exit code unstable: %d then %d\n%s", r.code, r2.code, r2.screen())
		}
	})

	t.Run("HistoryPartialTurnCancelled", func(t *testing.T) {
		kinds := headlessStdinEOFAndSIGINTHistory(t, headlessStdinEOFAndSIGINTResult{r.headlessResult, r.home})
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

	t.Run("NoLingeringChildren", func(t *testing.T) {
		deadline := time.Now().Add(3 * time.Second)
		for {
			o, _ := exec.Command("pgrep", "-g", strconv.Itoa(r.pgid)).Output()
			if len(bytes.TrimSpace(o)) == 0 {
				return
			}
			if time.Now().After(deadline) {
				t.Fatalf("processes outlive bough in its group %d: %s", r.pgid, o)
			}
			time.Sleep(100 * time.Millisecond)
		}
	})
}
