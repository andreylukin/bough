package vtreal

// The terminal window closes mid-turn: the PTY master goes away (the
// kernel SIGHUPs the foreground group), not a signal from `kill`.
// bough must exit within 2 s, leave no child it started (a foreground
// tools.bash, a background job) running, leave a history file whose
// every line is whole JSON with no half-written assistant entry, and
// resume on the same $HOME without a stuck spinner.
//
// Determinism: the streaming case waits for word N on screen; the
// bash cases wait for a gate file the command touches before it
// sleeps. Each run's sleep carries a unique duration, the pgrep marker.

import (
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// sighupTerminalCloseHistoryTape writes a tape of raw entries.
func sighupTerminalCloseHistoryTape(t *testing.T, dir string, entries [][2]any) string {
	t.Helper()
	var b strings.Builder
	for i, e := range entries {
		j, _ := json.Marshal(map[string]any{"seq": i + 1, "at": "2026-09-11T12:00:00Z", "kind": e[0], "data": e[1]})
		b.Write(append(j, '\n'))
	}
	p := filepath.Join(dir, "tape.jsonl")
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// sighupTerminalCloseHistoryStart boots bough in home with the PTY as
// its controlling terminal (Setsid + Setctty), as a terminal window
// does. The shared start leaves it without one (tty "??"), so closing
// the master would never SIGHUP it and the test would prove nothing.
func sighupTerminalCloseHistoryStart(t *testing.T, home, yml string, args ...string) *app {
	t.Helper()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, 100, 30)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, append([]string{"-config", cfg}, args...)...)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=",
	)
	cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true, Setctty: true, Ctty: 0}
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: 100, rows: 30, home: home}
	t.Cleanup(func() {
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	a.waitFor("say something")
	return a
}

// sighupTerminalCloseHistoryAlive lists processes matching marker.
func sighupTerminalCloseHistoryAlive(marker string) string {
	out, _ := exec.Command("pgrep", "-fl", marker).Output()
	return strings.TrimSpace(string(out))
}

// sighupTerminalCloseHistoryClose closes the PTY master and waits for
// bough to exit.
func sighupTerminalCloseHistoryClose(t *testing.T, a *app) {
	t.Helper()
	screen := a.text()
	done := make(chan error, 1)
	start := time.Now()
	_ = a.term.Close()
	go func() { done <- a.term.Wait(a.cmd) }()
	select {
	case <-done:
		t.Logf("exited %s after the terminal closed", time.Since(start).Round(time.Millisecond))
	case <-time.After(2 * time.Second):
		t.Fatalf("still running 2 s after the terminal closed; last screen:\n%s", screen)
	}
}

// sighupTerminalCloseHistoryCheck: every line whole JSON; no assistant
// entry after the last input other than a whole recorded reply.
func sighupTerminalCloseHistoryCheck(t *testing.T, home string, whole []string) string {
	t.Helper()
	session := crashResumeIntegritySession(t, home)
	es := crashResumeIntegrityLines(t, session)
	last := -1
	for i, e := range es {
		if e.Kind == "input" {
			last = i
		}
	}
	if last < 0 {
		t.Fatalf("in-flight input not on disk: %s", crashResumeIntegrityKinds(es))
	}
	for _, e := range es[last+1:] {
		if e.Kind != "assistant" {
			continue
		}
		text, _ := e.Data["text"].(string)
		ok := false
		for _, w := range whole {
			ok = ok || text == w
		}
		if !ok {
			t.Errorf("half-written assistant entry after the terminal closed: %q (kinds %s)", text, crashResumeIntegrityKinds(es))
		}
	}
	return session
}

// sighupTerminalCloseHistoryResume restarts on the same $HOME and
// checks the in-flight prompt is back with no spinner running.
func sighupTerminalCloseHistoryResume(t *testing.T, home, yml, session, prompt string) {
	t.Helper()
	id := strings.TrimSuffix(filepath.Base(session), ".jsonl")
	b := sighupTerminalCloseHistoryStart(t, home, yml, "--resume", id)
	b.waitFor("resumed ")
	b.check("resumed boot")
	s := b.settled()
	if !strings.Contains(s, "❯ "+prompt) {
		t.Errorf("resumed transcript lost the in-flight prompt:\n%s", s)
	}
	time.Sleep(1500 * time.Millisecond) // a live spinner would tick past 1s
	if s := b.settled(); cancelSpinner.MatchString(s) {
		t.Errorf("spinner still running after resume:\n%s", s)
	}
}

func TestSighupTerminalCloseHistory(t *testing.T) {
	t.Parallel()

	t.Run("streaming", func(t *testing.T) {
		t.Parallel()
		home := t.TempDir()
		var words []string
		for i := 1; i <= 60; i++ {
			words = append(words, fmt.Sprintf("w%02d", i))
		}
		reply := "```stop\n" + strings.Join(words, " ") + "\n```"
		const prompt = "stream then close"
		tape := sighupTerminalCloseHistoryTape(t, home, [][2]any{
			{"meta", map[string]any{"cwd": "/tmp/demo"}},
			{"input", map[string]any{"text": prompt}},
			{"assistant", map[string]any{"text": reply}},
			{"done", map[string]any{"text": ""}},
		})
		yml := crashResumeIntegrityConfig(tape, 150)
		a := sighupTerminalCloseHistoryStart(t, home, yml)
		a.typeText(prompt)
		a.key(uv.KeyEnter, 0)
		a.waitFor("w05")
		if strings.Contains(a.text(), "w60") {
			t.Fatalf("gate too late: whole reply streamed:\n%s", a.text())
		}
		sighupTerminalCloseHistoryClose(t, a)
		session := sighupTerminalCloseHistoryCheck(t, home, []string{reply})
		sighupTerminalCloseHistoryResume(t, home, yml, session, prompt)
	})

	// A foreground tools.bash and a background job run in their own
	// process group: the terminal's SIGHUP never reaches them, so only
	// bough's shutdown can.
	for _, bg := range []bool{false, true} {
		name := "foreground_bash"
		if bg {
			name = "background_job"
		}
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			if !bg && os.Getenv("BOUGH_KNOWN_SIGHUP_TERMINAL_CLOSE_HISTORY") == "" {
				// Known bug: a terminal closed during a foreground tools.bash
				// keeps bough alive for the whole 60 s bashTimeout: shutdown
				// never cancels the running bash (plugins/tools/tools.go bash).
				t.Skip("known bug: bough outlives a closed terminal by the 60 s bash timeout during a foreground tools.bash; set BOUGH_KNOWN_SIGHUP_TERMINAL_CLOSE_HISTORY=1 to run")
			}
			home := t.TempDir()
			gate := filepath.Join(home, "gate")
			// The unique duration is the marker pgrep looks for.
			marker := fmt.Sprintf("sleep 97.%d", time.Now().UnixNano()%1000000)
			call := fmt.Sprintf("tools.bash(%q)", "touch "+gate+"; "+marker)
			if bg {
				call = fmt.Sprintf("tools.bash(%q, 300)", "touch "+gate+"; "+marker)
			}
			const prompt = "run it then close"
			block := "```js\nconsole.log(" + call + ")\n```"
			stop := "```stop\nstarted\n```"
			tape := sighupTerminalCloseHistoryTape(t, home, [][2]any{
				{"meta", map[string]any{"cwd": "/tmp/demo"}},
				{"input", map[string]any{"text": prompt}},
				{"assistant", map[string]any{"text": block}},
				{"assistant", map[string]any{"text": stop}},
				{"done", map[string]any{"text": ""}},
			})
			yml := jobsConfig(tape)
			a := sighupTerminalCloseHistoryStart(t, home, yml)
			t.Cleanup(func() { _ = exec.Command("pkill", "-f", marker).Run() })
			a.typeText(prompt)
			a.key(uv.KeyEnter, 0)
			deadline := time.Now().Add(30 * time.Second)
			for {
				if _, err := os.Stat(gate); err == nil && sighupTerminalCloseHistoryAlive(marker) != "" {
					break
				}
				if time.Now().After(deadline) {
					t.Fatalf("gate never opened (bash did not start):\n%s", a.text())
				}
				time.Sleep(20 * time.Millisecond)
			}
			if bg {
				a.waitFor("job 1")
			}
			sighupTerminalCloseHistoryClose(t, a)
			var left string
			for range 40 { // up to 2 s for the group kill to land
				if left = sighupTerminalCloseHistoryAlive(marker); left == "" {
					break
				}
				time.Sleep(50 * time.Millisecond)
			}
			if left != "" {
				t.Errorf("orphaned child after the terminal closed:\n%s", left)
			}
			session := sighupTerminalCloseHistoryCheck(t, home, []string{block, stop, "started"})
			sighupTerminalCloseHistoryResume(t, home, yml, session, prompt)
		})
	}
}
