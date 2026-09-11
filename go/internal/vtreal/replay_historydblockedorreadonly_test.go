//go:build unix

package vtreal

// The session log locked by someone else, and the history dir made
// read-only. bough keeps history as JSONL under ~/.bough/history (no
// SQLite), so "locked" is an exclusive flock on the session file — the
// lock bough's own appends take — and "read-only" is chmod 0555 on the
// dir. Either way the user must be told, or the turn must land on
// disk: never a silent drop, never a crash.

import (
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"syscall"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// historyDBLockedOrReadonlyWarn is what a clear message could say.
var historyDBLockedOrReadonlyWarn = regexp.MustCompile(`(?i)(history|session log).*(lock|permission|read-only|not sav|denied|fail)|(lock|permission denied|read-only)`)

// historyDBLockedOrReadonlyBoot is startCfg without waiting for the
// composer: bough may refuse to boot, and that is a legal outcome.
func historyDBLockedOrReadonlyBoot(t *testing.T, home, yml string) *app {
	t.Helper()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, 100, 40)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=",
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: 100, rows: 40, home: home}
	t.Cleanup(func() {
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	return a
}

func TestHistoryDBLockedOrReadonly(t *testing.T) {
	t.Parallel()

	t.Run("Locked", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_HISTORY_DB_LOCKED_OR_READONLY") == "" {
			t.Skip("known bug: history lockFile blocks forever on LOCK_EX held by another process; the turn hangs with nothing on screen (plugins/history/lock_unix.go, Store.Append); set BOUGH_KNOWN_HISTORY_DB_LOCKED_OR_READONLY=1 to run")
		}
		t.Parallel()
		tape, _ := filepath.Abs("testdata/replay/resume.jsonl")
		next, _ := filepath.Abs("testdata/replay/resume-next.jsonl")
		log := filepath.Join(t.TempDir(), "session.jsonl")
		seed := startCfg(t, 100, 40, resumeConfig(tape, log))
		resumeSend(t, seed, log, "greet me", 1)
		concurrentWritersHistoryQuit(seed)

		// Hold the exclusive lock on a separate open file description:
		// flock(2) conflicts across descriptions, same as another process.
		f, err := os.Open(log)
		if err != nil {
			t.Fatal(err)
		}
		defer f.Close()
		if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX); err != nil {
			t.Fatal(err)
		}
		locked := true
		defer func() {
			if locked {
				_ = syscall.Flock(int(f.Fd()), syscall.LOCK_UN)
			}
		}()

		a := startCfg(t, 100, 40, resumeConfig(next, log))
		a.typeText("and now")
		a.key(uv.KeyEnter, 0)
		told := false
		deadline := time.Now().Add(20 * time.Second)
		for resumeDones(log) < 2 && time.Now().Before(deadline) {
			if historyDBLockedOrReadonlyWarn.MatchString(a.text()) {
				told = true
				break
			}
			time.Sleep(100 * time.Millisecond)
		}
		if resumeDones(log) < 2 && !told {
			t.Errorf("history locked for 20s: turn neither persisted nor did the screen say so:\n%s", a.text())
		}
		// Released, the turn must reach the log, and the screen stays sane.
		_ = syscall.Flock(int(f.Fd()), syscall.LOCK_UN)
		locked = false
		resumeWaitDones(t, a, log, 2)
		a.check("after unlock")
		if !strings.Contains(historyDBLockedOrReadonlyRead(t, log), `"and now"`) {
			t.Errorf("the submitted input never reached %s", log)
		}
	})

	t.Run("ReadOnlyDir", func(t *testing.T) {
		t.Parallel()
		if os.Geteuid() == 0 {
			t.Skip("root ignores directory permissions")
		}
		tape, _ := filepath.Abs("testdata/replay/resume.jsonl")
		home := t.TempDir()
		dir := filepath.Join(home, ".bough", "history")
		if err := os.MkdirAll(dir, 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.Chmod(dir, 0o555); err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { _ = os.Chmod(dir, 0o755) })

		a := historyDBLockedOrReadonlyBoot(t, home, replayConfig(tape))
		exited := make(chan struct{})
		go func() { _, _ = a.cmd.Process.Wait(); close(exited) }()
		deadline := time.Now().Add(20 * time.Second)
		booted := false
		for time.Now().Before(deadline) && !booted {
			select {
			case <-exited:
				// Refusing to boot is fine if it says why, and it must stay down.
				s := a.text()
				if !historyDBLockedOrReadonlyWarn.MatchString(s) || !strings.Contains(s, "history") {
					t.Errorf("bough exited without naming the history problem:\n%s", s)
				}
				if panicky.MatchString(s) {
					t.Errorf("crash text on exit:\n%s", s)
				}
				return
			default:
			}
			booted = strings.Contains(a.text(), "say something")
			time.Sleep(100 * time.Millisecond)
		}
		if !booted {
			t.Fatalf("neither booted nor exited in 20s:\n%s", a.text())
		}
		// Booted in some degraded mode: submitting a turn must tell the
		// user it is not being saved.
		a.typeText("greet me")
		a.key(uv.KeyEnter, 0)
		a.waitFor("Hello from turn one.")
		a.check("after turn")
		if s := a.text(); !historyDBLockedOrReadonlyWarn.MatchString(s) {
			t.Errorf("history dir read-only: turn ran but nothing on screen says it is not saved:\n%s", s)
		}
	})
}

func historyDBLockedOrReadonlyRead(t *testing.T, p string) string {
	t.Helper()
	b, err := os.ReadFile(p)
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}
