// bough update / bough restart against the real binary. The sandbox
// HOME has no pidfile and no repos/bough, and env() blanks BOUGH_ROOT,
// so nothing here can touch a real checkout or session.
package e2e

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func TestRestartNoPidfile(t *testing.T) {
	t.Parallel()
	home, cwd, _ := sandbox(t, launchOpts{})
	out, code := runCLI(t, home, cwd, "restart")
	if code != 0 {
		t.Fatalf("exit code %d; output:\n%s", code, out)
	}
	mustContain(t, out, "no running web session; sessions pick up the new binary on next launch")
}

func TestUpdateOutsideCheckout(t *testing.T) {
	t.Parallel()
	if os.Getenv("BOUGH_BIN") != "" {
		t.Skip("BOUGH_BIN override may live inside a real checkout")
	}
	home, cwd, _ := sandbox(t, launchOpts{})
	// Keep the Go build cache out of the sandbox. HOME is overridden
	// there, so `go build` would put GOCACHE inside it and fill it with
	// read-only entries that TempDir's cleanup cannot remove; the real
	// caches also make the build fast instead of cold.
	caches := []string{"GOCACHE=" + goEnv(t, "GOCACHE"), "GOMODCACHE=" + goEnv(t, "GOMODCACHE")}
	// git writes its objects read-only, and RemoveAll cannot delete
	// those. Cleanups run last-registered-first, so this one makes the
	// clone writable before the directory is torn down.
	t.Cleanup(func() {
		_ = makeWritable(home)
		_ = os.RemoveAll(home) // do it here, while everything is writable
	})

	// A local repo standing in for GitHub: this asserts that update
	// CLONES AND BUILDS MAIN when there is no checkout, so it must not
	// reach the network to do it.
	upstream := fakeUpstream(t)
	out, code := runCLIEnv(t, home, cwd, append(caches, "BOUGH_UPSTREAM="+upstream), "update")

	// It gets as far as building, which is the behaviour under test —
	// the stand-in repo is not bough, so the build itself fails, and
	// that failure is the proof it tried to build main rather than
	// pointing at a release.
	mustContain(t, out, "no checkout found", "cloning "+upstream, "main at ")
	if code == 0 {
		t.Fatalf("building a repo that is not bough should fail; output:\n%s", out)
	}
	// The old behaviour, and what this replaced: never send someone to
	// a tagged release when they asked to update.
	mustNotContain(t, out, "brew upgrade")
}

// fakeUpstream is a git repo with a main branch and one commit, to
// clone from without touching the network.
func fakeUpstream(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	write := func(name, body string) {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	write("go.mod", "module fake\n\ngo 1.27\n")
	for _, argv := range [][]string{
		{"git", "init", "-q", "-b", "main"},
		{"git", "config", "user.email", "t@example.com"},
		{"git", "config", "user.name", "t"},
		{"git", "add", "-A"},
		{"git", "commit", "-qm", "initial"},
	} {
		cmd := exec.Command(argv[0], argv[1:]...)
		cmd.Dir = dir
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("%v: %v\n%s", argv, err, out)
		}
	}
	return dir
}

// makeWritable adds the owner write bit to everything under dir, so a
// git clone can be deleted.
func makeWritable(dir string) error {
	return filepath.Walk(dir, func(p string, info os.FileInfo, err error) error {
		if err != nil {
			return nil // best effort: this only exists to allow cleanup
		}
		return os.Chmod(p, info.Mode().Perm()|0o200)
	})
}

// goEnv reads one value out of `go env`.
func goEnv(t *testing.T, name string) string {
	t.Helper()
	out, err := exec.Command("go", "env", name).Output()
	if err != nil {
		t.Fatalf("go env %s: %v", name, err)
	}
	return strings.TrimSpace(string(out))
}
