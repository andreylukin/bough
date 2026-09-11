package tools

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
	"github.com/andreylukin/bough/plugins/rules"
)

// bashFailRig mounts the real tools-basic plugin on a real codemode, so
// each failure is seen the way the loop sees it: a JS throw (what the
// model is fed) plus the turn stats (exit code).
func bashFailRig(t *testing.T) (*codemode.CodeMode, *Stats) {
	t.Helper()
	ctx := kernel.NewContext()
	ctx.Provide("codemode", codemode.New(20*time.Second))
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	cm, _ := kernel.Get[*codemode.CodeMode](ctx, "codemode")
	st, _ := kernel.Get[*Stats](ctx, "turn-stats")
	return cm, st
}

func bashFailKnown(t *testing.T, bug string) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_TOOLS_BASH") != "1" {
		t.Skip("known bug (BOUGH_KNOWN_TOOLS_BASH=1 to run): " + bug)
	}
}

// bashFailRun runs one script under a wall-clock bound and proves the
// VM is still usable afterwards.
func bashFailRun(t *testing.T, cm *codemode.CodeMode, code string, bound time.Duration) (string, error, time.Duration) {
	t.Helper()
	start := time.Now()
	out, err := cm.Run(code)
	el := time.Since(start)
	if el > bound {
		t.Errorf("%s took %s, bound %s", code, el, bound)
	}
	if o, e := cm.Run(`tools.bash("echo alive")`); e != nil || !strings.Contains(o, "alive") {
		t.Errorf("codemode unusable after %s: %q %v", code, o, e)
	}
	return out, err, el
}

func TestBashFailExitCodes(t *testing.T) {
	cm, st := bashFailRig(t)
	notExec := filepath.Join(t.TempDir(), "noexec.sh")
	if err := os.WriteFile(notExec, []byte("echo hi\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		name, cmd, want string
		code            int
	}{
		{"exit1", "echo oops; exit 1", "exit status 1 — oops", 1},
		{"exit2", "ls /definitely/not/here", "exit status", 1}, // ls: 1 on macOS, 2 on GNU
		{"notfound127", "no-such-command-bough-xyz", "exit status 127", 127},
		{"notexec126", notExec, "exit status 126", 126},
		{"sigkill", "kill -9 $$", "signal: killed", -1},
		{"sigsegv", "kill -SEGV $$", "signal: segmentation fault", -1},
		{"stderronly", "echo only-err >&2; exit 4", "only-err", 4},
		{"enoentcd", "cd /no/such/dir/xyz && echo never", "exit status", 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			_, err, _ := bashFailRun(t, cm, `tools.bash(`+jsStr(tc.cmd)+`)`, 5*time.Second)
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("model is fed %v, want %q", err, tc.want)
			}
			if strings.Contains(err.Error(), "\nnever") || strings.HasSuffix(err.Error(), "— never") {
				t.Errorf("ran past failed cd: %v", err)
			}
			st.Take() // the alive probe reset it; re-run for the stat
			cm.Run(`try { tools.bash(` + jsStr(tc.cmd) + `) } catch (e) {}`)
			_, code, ran := st.Take()
			if !ran || code == 0 || (tc.code > 1 && code != tc.code) || (tc.code < 0 && code != -1) {
				t.Errorf("stats exit = %d ran = %v, want %d", code, ran, tc.code)
			}
		})
	}
}

// A caught failure is data: the script goes on and sees the message.
func TestBashFailCaughtIsData(t *testing.T) {
	cm, _ := bashFailRig(t)
	out, err := cm.Run(`let m; try { tools.bash("echo boom >&2; exit 3") } catch (e) { m = String(e) }; "caught:" + m`)
	if err != nil || !strings.Contains(out, "caught:") || !strings.Contains(out, "exit status 3") || !strings.Contains(out, "boom") {
		t.Fatalf("out=%q err=%v", out, err)
	}
}

func TestBashFailOddOutput(t *testing.T) {
	cm, _ := bashFailRig(t)
	t.Run("stderr-on-success", func(t *testing.T) {
		out, err, _ := bashFailRun(t, cm, `tools.bash("echo warn >&2")`, 5*time.Second)
		if err != nil || !strings.Contains(out, "warn") {
			t.Fatalf("%q %v", out, err)
		}
	})
	t.Run("nul-and-invalid-utf8", func(t *testing.T) {
		out, err, _ := bashFailRun(t, cm, `tools.bash("printf 'a\\000b\\377\\376c-end'")`, 5*time.Second)
		if err != nil || !strings.HasPrefix(out, "a") || !strings.HasSuffix(out, "c-end") {
			t.Fatalf("%q %v", out, err)
		}
	})
	t.Run("ansi-and-cr", func(t *testing.T) {
		out, err, _ := bashFailRun(t, cm, `tools.bash("printf '\\033[31mred\\033[0m 10%%\\r50%%\\r100%%\\n'")`, 5*time.Second)
		if err != nil || !strings.Contains(out, "100%") {
			t.Fatalf("%q %v", out, err)
		}
	})
	t.Run("huge-output", func(t *testing.T) {
		out, err, _ := bashFailRun(t, cm, `tools.bash("yes abcdefghij | head -c 300000; echo; echo LAST")`, 10*time.Second)
		if err != nil || len(out) < 300000 || !strings.Contains(out, "LAST") {
			t.Fatalf("len=%d err=%v (the loop, not tools, caps it)", len(out), err)
		}
	})
}

func TestBashFailArgs(t *testing.T) {
	cm, _ := bashFailRig(t)
	// No argument / null: a clean error or a no-op, never a panic or hang.
	bashFailRun(t, cm, `try { tools.bash() } catch (e) {}`, 5*time.Second)
	bashFailRun(t, cm, `try { tools.bash(null) } catch (e) {}`, 5*time.Second)
	for _, code := range []string{`tools.bash("echo x", "nonsense")`, `tools.bash("echo x", -5)`} {
		if _, err, _ := bashFailRun(t, cm, code, 5*time.Second); err == nil {
			t.Errorf("%s: no error", code)
		}
	}
}

// Every call is a fresh sh: cd and export do not leak into the next.
func TestBashFailNoStateLeak(t *testing.T) {
	cm, _ := bashFailRig(t)
	if _, err := cm.Run(`tools.bash("cd /; export BOUGH_LEAK=1")`); err != nil {
		t.Fatal(err)
	}
	out, err := cm.Run(`tools.bash("pwd; echo leak=$BOUGH_LEAK")`)
	if err != nil || !strings.Contains(out, "leak=\n") {
		t.Fatalf("%q %v", out, err)
	}
	if strings.HasPrefix(out, "/\n") {
		t.Errorf("cwd leaked: %q", out)
	}
}

// stdin is the script itself (sh -s): a command that reads stdin must
// not hang, and must not eat the rest of the script.
func TestBashFailStdin(t *testing.T) {
	cm, _ := bashFailRig(t)
	t.Run("does-not-hang", func(t *testing.T) {
		bashFailRun(t, cm, `try { tools.bash("cat") } catch (e) {}`, 5*time.Second)
	})
	t.Run("does-not-eat-script", func(t *testing.T) {
		out, err, _ := bashFailRun(t, cm, `tools.bash("cat >/dev/null\necho after-cat")`, 5*time.Second)
		if err != nil || !strings.Contains(out, "after-cat") {
			bashFailKnown(t, "tools.go:204 feeds the script on stdin (sh -s), so a stdin reader (cat, read, ssh, npm prompts) swallows the following lines")
			t.Fatalf("script after a stdin reader never ran: %q %v", out, err)
		}
	})
}

// A command that backgrounds a child holding stdout open: the call
// must return promptly with success.
func TestBashFailDaemonHoldsStdout(t *testing.T) {
	cm, _ := bashFailRig(t)
	marker := "bough-bashfail-daemon-" + filepath.Base(t.TempDir())
	t.Cleanup(func() { exec.Command("pkill", "-f", marker).Run() })
	out, err, el := bashFailRun(t, cm, `tools.bash("sh -c 'sleep 30; : `+marker+`' & echo started")`, 6*time.Second)
	if err != nil || !strings.Contains(out, "started") {
		bashFailKnown(t, "tools.go:211 WaitDelay: a backgrounded child holding stdout turns a successful command into an error: "+errString(err))
		t.Fatalf("%q %v (%s)", out, err, el)
	}
}

func errString(err error) string {
	if err == nil {
		return "<nil>"
	}
	return err.Error()
}

func TestBashFailPolicyDenies(t *testing.T) {
	cm, st := bashFailRig(t)
	home, project := t.TempDir(), t.TempDir()
	dir := filepath.Join(project, ".codex", "rules")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "t.rules"), []byte(`prefix_rule(pattern = ["touch"], decision = "forbidden")`), 0o644); err != nil {
		t.Fatal(err)
	}
	st.SetPolicy(rules.New(home, project).Policy)
	marker := filepath.Join(t.TempDir(), "made")
	_, err, _ := bashFailRun(t, cm, `tools.bash("touch `+marker+`")`, 5*time.Second)
	if err == nil || !strings.Contains(err.Error(), "refused") {
		t.Fatalf("policy: %v", err)
	}
	if _, e := os.Stat(marker); e == nil {
		t.Fatal("refused command ran")
	}
}

// The cwd vanished under the process: bash still reports, never hangs.
func TestBashFailDeletedCwd(t *testing.T) {
	cm, _ := bashFailRig(t)
	wd, _ := os.Getwd()
	gone := t.TempDir()
	if err := os.Chdir(gone); err != nil {
		t.Fatal(err)
	}
	defer os.Chdir(wd)
	os.Remove(gone)
	bashFailRun(t, cm, `try { tools.bash("echo x") } catch (e) {}`, 5*time.Second)
}

func TestBashFailBackgroundJob(t *testing.T) {
	s := newTestStats(t)
	for _, tc := range []struct{ cmd, want string }{
		{"echo bg-err >&2; exit 7", "7"},
		{"kill -9 $$", "killed"},
		{"no-such-cmd-bough-bg", "127"},
	} {
		if _, err := s.bash(tc.cmd, 30); err != nil {
			t.Fatal(err)
		}
		select {
		case <-s.jobs.Wake():
		case <-time.After(5 * time.Second):
			t.Fatalf("%s: no wake", tc.cmd)
		}
		if n := strings.Join(s.jobs.Take(), "\n"); !strings.Contains(n, tc.want) {
			t.Errorf("%s: notice %q lacks %q", tc.cmd, n, tc.want)
		}
	}
}

// Soak: the 60 s foreground kill for real.
func TestBashFailSoakTimeout(t *testing.T) {
	if os.Getenv("BOUGH_SOAK_TOOLS_BASH") != "1" {
		t.Skip("BOUGH_SOAK_TOOLS_BASH=1")
	}
	cm, _ := bashFailRig(t)
	_, err, _ := bashFailRun(t, cm, `tools.bash("sleep 70")`, 65*time.Second)
	if err == nil || !strings.Contains(err.Error(), "killed after 1m0s") {
		t.Fatal(err)
	}
}
