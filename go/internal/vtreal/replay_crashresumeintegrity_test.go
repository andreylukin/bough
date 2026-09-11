package vtreal

// Crash-resume integrity: bough is SIGKILLed while a reply is still
// streaming, then restarted with --resume on the same session. The
// log must still parse line by line, the resumed transcript must show
// the prompt that was in flight plus a marker that its turn never
// finished, and a new turn must append to the same file.
//
// Determinism: the replay tape streams one word per delay_ms, and the
// kill waits until the Nth word is on screen (the gate), so the kill
// always lands mid-stream, never before or after it.

import (
	"bufio"
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

	"github.com/andreylukin/bough/plugins/history"
)

const crashResumeIntegrityPrompt = "stream a long answer"

// crashResumeIntegrityTape writes a one-turn tape into dir.
func crashResumeIntegrityTape(t *testing.T, dir, name, input, reply string) string {
	t.Helper()
	lines := []map[string]any{
		{"seq": 1, "at": "2026-09-10T12:00:00Z", "kind": "meta", "data": map[string]any{"cwd": "/tmp/demo"}},
		{"seq": 2, "at": "2026-09-10T12:00:01Z", "kind": "input", "data": map[string]any{"text": input}},
		{"seq": 3, "at": "2026-09-10T12:00:02Z", "kind": "assistant", "data": map[string]any{"text": reply}},
		{"seq": 4, "at": "2026-09-10T12:00:02Z", "kind": "done", "data": map[string]any{"text": ""}},
	}
	var b strings.Builder
	for _, l := range lines {
		j, _ := json.Marshal(l)
		b.Write(append(j, '\n'))
	}
	p := filepath.Join(dir, name)
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// crashResumeIntegrityConfig is replayConfig with a streaming delay.
func crashResumeIntegrityConfig(tape string, delayMS int) string {
	return strings.Replace(replayConfig(tape),
		fmt.Sprintf("config: {file: %q}\n", tape),
		fmt.Sprintf("config: {file: %q, delay_ms: %d}\n", tape, delayMS), 1)
}

// crashResumeIntegrityStart boots bough in an existing $HOME with extra
// arguments (startCfg always makes a fresh $HOME).
func crashResumeIntegrityStart(t *testing.T, home, yml string, args ...string) *app {
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

// crashResumeIntegritySession is the one session file under home.
func crashResumeIntegritySession(t *testing.T, home string) string {
	t.Helper()
	paths, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
	if len(paths) != 1 {
		t.Fatalf("want one session file, got %v", paths)
	}
	return paths[0]
}

// crashResumeIntegrityLines checks every line of the log is one valid
// JSON entry (history.Read skips corrupt lines, so it cannot be the
// judge) and returns the entries.
func crashResumeIntegrityLines(t *testing.T, path string) []history.Entry {
	t.Helper()
	f, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	var out []history.Entry
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 0, 64*1024), 4*1024*1024)
	n := 0
	for sc.Scan() {
		n++
		var e history.Entry
		if err := json.Unmarshal(sc.Bytes(), &e); err != nil {
			t.Errorf("%s:%d is not a valid entry: %v\n%q", path, n, err, sc.Text())
			continue
		}
		out = append(out, e)
	}
	if err := sc.Err(); err != nil {
		t.Fatal(err)
	}
	return out
}

func crashResumeIntegrityKinds(es []history.Entry) string {
	var ks []string
	for _, e := range es {
		ks = append(ks, e.Kind)
	}
	return strings.Join(ks, ",")
}

// crashResumeIntegrityKill boots, submits the long prompt, waits for
// word n on screen (the gate), SIGKILLs and returns home + session.
func crashResumeIntegrityKill(t *testing.T, n int) (home, session string) {
	t.Helper()
	home = t.TempDir()
	var words []string
	for i := 1; i <= 60; i++ {
		words = append(words, fmt.Sprintf("w%02d", i))
	}
	tape := crashResumeIntegrityTape(t, home, "crash-tape.jsonl", crashResumeIntegrityPrompt,
		"```stop\n"+strings.Join(words, " ")+"\n```")
	a := crashResumeIntegrityStart(t, home, crashResumeIntegrityConfig(tape, 150))
	a.typeText(crashResumeIntegrityPrompt)
	a.key(uv.KeyEnter, 0)
	a.waitFor(fmt.Sprintf("w%02d", n))
	if s := a.text(); strings.Contains(s, "w60") {
		t.Fatalf("gate too late: the whole reply already streamed:\n%s", s)
	}
	if err := a.cmd.Process.Signal(syscall.SIGKILL); err != nil {
		t.Fatal(err)
	}
	_ = a.term.Wait(a.cmd)
	return home, crashResumeIntegritySession(t, home)
}

func TestCrashResumeIntegrity(t *testing.T) {
	t.Parallel()
	for _, n := range []int{3, 12} {
		t.Run(fmt.Sprintf("after_%d_deltas", n), func(t *testing.T) {
			t.Parallel()
			home, session := crashResumeIntegrityKill(t, n)

			t.Run("log_parses", func(t *testing.T) {
				es := crashResumeIntegrityLines(t, session)
				saw := false
				for _, e := range es {
					if e.Kind == "input" && history.Prompt(e) == crashResumeIntegrityPrompt {
						saw = true
					}
					if e.Kind == "done" {
						t.Errorf("a killed turn recorded done: %s", crashResumeIntegrityKinds(es))
					}
				}
				if !saw {
					t.Errorf("in-flight prompt not on disk: %s", crashResumeIntegrityKinds(es))
				}
			})

			id := strings.TrimSuffix(filepath.Base(session), ".jsonl")
			next := crashResumeIntegrityTape(t, home, "next-tape.jsonl", "and now",
				"```stop\nContinued after the crash.\n```")
			b := crashResumeIntegrityStart(t, home, crashResumeIntegrityConfig(next, 0), "--resume", id)
			b.waitFor("resumed ")
			b.check("resumed boot")
			s := b.settled()

			t.Run("prompt_shown", func(t *testing.T) {
				if !strings.Contains(s, "❯ "+crashResumeIntegrityPrompt) {
					t.Errorf("resumed transcript lost the in-flight prompt:\n%s", s)
				}
			})

			t.Run("incomplete_marker", func(t *testing.T) {
				low := strings.ToLower(s)
				if !strings.Contains(low, "cancelled") && !strings.Contains(low, "interrupted") &&
					!strings.Contains(low, "incomplete") && !strings.Contains(low, "without a reply") {
					t.Errorf("killed turn resumed with no cancelled/incomplete marker:\n%s", s)
				}
			})

			t.Run("new_turn_appends", func(t *testing.T) {
				before := len(crashResumeIntegrityLines(t, session))
				b.typeText("and now")
				b.key(uv.KeyEnter, 0)
				b.waitFor("Continued after the crash.")
				deadline := time.Now().Add(30 * time.Second)
				for resumeDones(session) < 1 && time.Now().Before(deadline) {
					time.Sleep(50 * time.Millisecond)
				}
				es := crashResumeIntegrityLines(t, session)
				if len(es) <= before || resumeDones(session) < 1 {
					t.Fatalf("new turn did not append to %s (%d -> %d lines): %s", session, before, len(es), crashResumeIntegrityKinds(es))
				}
				for i := 1; i < len(es); i++ {
					if es[i].Seq <= es[i-1].Seq {
						t.Errorf("seq not increasing at line %d: %s", i+1, crashResumeIntegrityKinds(es))
					}
				}
				b.check("after the new turn")
			})
		})
	}
}
