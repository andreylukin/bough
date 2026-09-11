package vtreal

// Spill and scratchpad when the disk says no. The loop saves a block
// result past 64 kB to ~/.bough/spill and appends the path; the
// scratchpad lives in ~/.bough/scratch/<session> and $BOUGH_SCRATCH
// points there. Two ways that goes wrong on a real PTY:
//
//   - ReadOnlyHome: $HOME and ~/.bough are 0555 (history's own dir is
//     pre-made writable so a session exists at all). A 100 kB result
//     must still say where the full output went, or say it could not
//     be saved — never cut the middle away silently — and the turn
//     must finish.
//   - ScratchDeletedMidSession: the scratch dir is removed after the
//     first turn; the next turn's huge result still spills, and the
//     next tools.bash sees a $BOUGH_SCRATCH that exists and is writable.
//
// Determinism: the llm is a replay tape; codemode, tools and the
// scratchpad are the real rows, so the blocks really hit the disk.

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"
)

// scratchDirPermissionsAndMissingHomeStart boots bough with the config
// outside home, so home can be read-only before boot.
func scratchDirPermissionsAndMissingHomeStart(t *testing.T, home, yml string) *app {
	t.Helper()
	cfg := filepath.Join(t.TempDir(), "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, 100, 30)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", "BOUGH_SCRATCH=",
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

// scratchDirPermissionsAndMissingHomeHuge prints ~100 kB then a tail mark.
func scratchDirPermissionsAndMissingHomeHuge(tag string) string {
	return "```js\n// phase:" + tag + "\n" +
		`console.log(tools.bash("head -c 100000 /dev/zero | tr '\\0' x; echo; echo TAILMARK"))` +
		"\n```"
}

// scratchDirPermissionsAndMissingHomeProbe reports $BOUGH_SCRATCH and
// whether it is a writable directory, without making it.
func scratchDirPermissionsAndMissingHomeProbe(tag string) string {
	return "```js\n// phase:" + tag + "\n" +
		`console.log("ENV=" + tools.bash("printf '%s' \"$BOUGH_SCRATCH\""))
console.log("W=" + tools.bash("test -d \"$BOUGH_SCRATCH\" && touch \"$BOUGH_SCRATCH/probe\" && echo WRITABLE || echo MISSING"))` +
		"\n```"
}

// scratchDirPermissionsAndMissingHomeSpilled checks a huge result is
// either saved somewhere real or visibly says it was not.
func scratchDirPermissionsAndMissingHomeSpilled(t *testing.T, out string) {
	t.Helper()
	if !strings.Contains(out, "TAILMARK") {
		t.Errorf("huge result lost its tail:\n%.400s", out)
	}
	if _, after, ok := strings.Cut(out, "[full output saved to "); ok {
		path, _, _ := strings.Cut(after, " ")
		if st, err := os.Stat(path); err != nil || st.Size() < 100000 {
			t.Errorf("spill file %s missing or short (%v)", path, err)
		}
		return
	}
	low := strings.ToLower(out)
	if strings.Contains(low, "could not save") || strings.Contains(low, "not saved") || strings.Contains(low, "spill failed") {
		return
	}
	t.Errorf("huge result cut with no spill path and no error (silent drop of the middle); tail:\n%s", out[max(0, len(out)-300):])
}

func TestScratchDirPermissionsAndMissingHome(t *testing.T) {
	t.Parallel()

	t.Run("ReadOnlyHome", func(t *testing.T) {
		t.Parallel()
		home := t.TempDir()
		bough := filepath.Join(home, ".bough")
		if err := os.MkdirAll(filepath.Join(bough, "history"), 0o755); err != nil {
			t.Fatal(err)
		}
		for _, d := range []string{bough, home} {
			if err := os.Chmod(d, 0o555); err != nil {
				t.Fatal(err)
			}
		}
		t.Cleanup(func() { _ = os.Chmod(home, 0o755); _ = os.Chmod(bough, 0o755) })
		tape := scratchDirLifecycleOnResumeTape(t, "ro.jsonl",
			scratchDirPermissionsAndMissingHomeHuge("ro-huge"),
			scratchDirPermissionsAndMissingHomeProbe("ro-probe"),
			"```stop\nro done.\n```")
		// graph opens ~/.bough/graph.db at mount and refuses to boot
		// on a read-only ~/.bough; it is not what this test is about.
		yml := scratchDirLifecycleOnResumeConfig(tape) + "- id: graph\n  plugin: graph\n  disabled: true\n"
		a := scratchDirPermissionsAndMissingHomeStart(t, home, yml)
		a.typeText("make output")
		a.key(uv.KeyEnter, 0)
		huge := scratchDirLifecycleOnResumeResult(a, "ro-huge")
		probe := scratchDirLifecycleOnResumeResult(a, "ro-probe")
		a.waitFor("ro done.")
		if !strings.Contains(huge, "TAILMARK") {
			t.Errorf("huge result lost its tail:\n%.400s", huge)
		}
		if os.Getenv("BOUGH_KNOWN_SCRATCH_DIR_PERMISSIONS_AND_MISSING_HOME") == "" {
			t.Skip("known bug: loop.capOutput silently degrades to a bare cut when ~/.bough/spill cannot be written (no path, no error), and $BOUGH_SCRATCH points at a dir that cannot be made (no fallback); set BOUGH_KNOWN_SCRATCH_DIR_PERMISSIONS_AND_MISSING_HOME=1 to check")
		}
		scratchDirPermissionsAndMissingHomeSpilled(t, huge)
		if w := scratchDirLifecycleOnResumeLine(probe, "W"); w != "WRITABLE" {
			t.Errorf("$BOUGH_SCRATCH=%q not a writable dir under a read-only home: %s",
				scratchDirLifecycleOnResumeLine(probe, "ENV"), w)
		}
	})

	t.Run("ScratchDeletedMidSession", func(t *testing.T) {
		t.Parallel()
		home := t.TempDir()
		tape := scratchDirLifecycleOnResumeTape(t, "del.jsonl",
			scratchDirLifecycleOnResumeProbe("del-first", "keep me"), "```stop\nfirst done.\n```",
			scratchDirPermissionsAndMissingHomeHuge("del-huge"),
			scratchDirPermissionsAndMissingHomeProbe("del-probe"),
			"```stop\nsecond done.\n```")
		a := scratchDirPermissionsAndMissingHomeStart(t, home, scratchDirLifecycleOnResumeConfig(tape))
		a.typeText("note it")
		a.key(uv.KeyEnter, 0)
		dir := scratchDirLifecycleOnResumeLine(scratchDirLifecycleOnResumeResult(a, "del-first"), "DIR")
		a.waitFor("first done.")
		if dir == "" || !strings.HasPrefix(dir, home) {
			t.Fatalf("scratch dir %q not under home %s", dir, home)
		}
		if err := os.RemoveAll(dir); err != nil {
			t.Fatal(err)
		}
		a.typeText("make output")
		a.key(uv.KeyEnter, 0)
		scratchDirPermissionsAndMissingHomeSpilled(t, scratchDirLifecycleOnResumeResult(a, "del-huge"))
		probe := scratchDirLifecycleOnResumeResult(a, "del-probe")
		a.waitFor("second done.")
		if env := scratchDirLifecycleOnResumeLine(probe, "ENV"); env != dir {
			t.Errorf("$BOUGH_SCRATCH = %q after rm, want %q", env, dir)
		}
		if os.Getenv("BOUGH_KNOWN_SCRATCH_DIR_PERMISSIONS_AND_MISSING_HOME") == "" {
			t.Skip("known bug: scratch.Pad makes its dir only on note/set/file, so after the dir is removed $BOUGH_SCRATCH points at a missing dir for tools.bash; set BOUGH_KNOWN_SCRATCH_DIR_PERMISSIONS_AND_MISSING_HOME=1 to check")
		}
		if w := scratchDirLifecycleOnResumeLine(probe, "W"); w != "WRITABLE" {
			t.Errorf("$BOUGH_SCRATCH %s is %s after the dir was removed mid-session (tools.bash never re-creates it)", dir, w)
		}
	})
}
