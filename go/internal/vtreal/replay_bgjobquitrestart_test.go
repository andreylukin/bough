package vtreal

// bgjob-quit-restart: a detached background job, then double ctrl+c,
// then `bough -c` on the same $HOME. Quitting must not leave the job's
// process behind, both runs exit 0, and the resumed session's job strip
// must not claim a job from a dead process is still running.

import (
	"fmt"
	"math/rand/v2"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// bgjobquitrestartTape writes a tape whose one turn starts `sleep
// <marker>` as a background job; the odd duration is unique per run so
// pgrep finds only this run's process.
func bgjobquitrestartTape(t *testing.T) (tape, marker string) {
	t.Helper()
	marker = fmt.Sprintf("sleep 3%03d.%04d", rand.IntN(1000), rand.IntN(10000))
	code := fmt.Sprintf("console.log(tools.bash(%q, 600))", marker)
	lines := []string{
		`{"seq":1,"at":"2026-09-10T11:00:00Z","kind":"meta","data":{"cwd":"/tmp/demo"}}`,
		`{"seq":2,"at":"2026-09-10T11:00:01Z","kind":"input","data":{"text":"start the long build"}}`,
		fmt.Sprintf(`{"seq":3,"at":"2026-09-10T11:00:02Z","kind":"assistant","data":{"text":%q}}`, "```js\n"+code+"\n```"),
		`{"seq":4,"at":"2026-09-10T11:00:03Z","kind":"assistant","data":{"text":"` + "```stop\\nThe build runs in the background as job 1.\\n```" + `"}}`,
		`{"seq":5,"at":"2026-09-10T11:00:03Z","kind":"done","data":{"text":""}}`,
	}
	tape = filepath.Join(t.TempDir(), "bgjob.jsonl")
	if err := os.WriteFile(tape, []byte(strings.Join(lines, "\n")+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	return tape, marker
}

// bgjobquitrestartBoot starts bough in an existing $HOME with extra args.
func bgjobquitrestartBoot(t *testing.T, home, yml string, args ...string) *app {
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

// bgjobquitrestartQuit sends ctrl+c twice and requires exit status 0.
func bgjobquitrestartQuit(t *testing.T, a *app) {
	t.Helper()
	a.key('c', uv.ModCtrl)
	a.waitFor("ctrl+c")
	a.key('c', uv.ModCtrl)
	done := make(chan error, 1)
	go func() { done <- a.term.Wait(a.cmd) }()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("exit: %v (want status 0)", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatalf("no exit after two ctrl+c:\n%s", a.text())
	}
}

// bgjobquitrestartPids lists live processes whose command line has marker.
func bgjobquitrestartPids(marker string) string {
	out, _ := exec.Command("pgrep", "-f", marker).Output()
	return strings.TrimSpace(string(out))
}

// bgjobquitrestartKill is the safety net: never leave the sleep behind.
func bgjobquitrestartKill(marker string) { _ = exec.Command("pkill", "-f", marker).Run() }

func TestBgjobQuitRestart(t *testing.T) {
	t.Parallel()
	tape, marker := bgjobquitrestartTape(t)
	t.Cleanup(func() { bgjobquitrestartKill(marker) })
	home := t.TempDir()
	yml := jobsConfig(tape)

	a := bgjobquitrestartBoot(t, home, yml)
	jobsSay(a, "start the long build")
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.waitUntil(func(s string) bool { return strings.Contains(s, "job 1") && strings.Contains(s, marker) },
		"the job strip to name job 1")
	if bgjobquitrestartPids(marker) == "" {
		t.Fatalf("the job's process %q is not running before quit", marker)
	}
	bgjobquitrestartQuit(t, a)

	t.Run("no orphan after quit", func(t *testing.T) {
		deadline := time.Now().Add(5 * time.Second)
		pids := bgjobquitrestartPids(marker)
		for pids != "" && time.Now().Before(deadline) {
			time.Sleep(100 * time.Millisecond)
			pids = bgjobquitrestartPids(marker)
		}
		if pids != "" {
			t.Fatalf("detached job %q still alive 5s after bough exited: pids %s", marker, pids)
		}
	})

	t.Run("resume strip not running", func(t *testing.T) {
		b := bgjobquitrestartBoot(t, home, yml, "-c")
		b.waitFor("start the long build") // the resumed transcript
		time.Sleep(2 * time.Second)
		s := b.settled()
		for _, l := range strings.Split(s, "\n") {
			// A strip row is "job 1 · <elapsed> · <cmd>"-shaped.
			if strings.Contains(l, "job 1") && strings.Contains(l, marker) && strings.Contains(l, "·") {
				t.Fatalf("resumed session shows the dead job as running:\n%s", s)
			}
		}
		b.check("resumed")
		bgjobquitrestartQuit(t, b)
	})
}
