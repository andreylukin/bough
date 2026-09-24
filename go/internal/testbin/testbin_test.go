package testbin

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

// module writes a tiny main module with one embedded file and one
// test file, so the key can be probed without building bough.
func module(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	files := map[string]string{
		"go.mod":       "module example.com/probe\n\ngo 1.22\n",
		"main.go":      "package main\n\nimport _ \"embed\"\n\n//go:embed msg.txt\nvar msg string\n\nfunc main() { print(msg) }\n",
		"msg.txt":      "hello",
		"main_test.go": "package main\n",
		"notes.md":     "not part of the build",
	}
	for name, body := range files {
		write(t, filepath.Join(dir, name), body)
	}
	return dir
}

func write(t *testing.T, path, body string) {
	t.Helper()
	if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

func TestKeyFollowsBuildInputsOnly(t *testing.T) {
	t.Parallel()
	dir := module(t)
	key := func() string {
		t.Helper()
		k, err := Key(dir, ".")
		if err != nil {
			t.Fatal(err)
		}
		return k
	}
	base := key()
	if again := key(); again != base {
		t.Fatalf("key not stable: %s then %s", base, again)
	}

	// Files the binary does not contain must not cost a rebuild.
	write(t, filepath.Join(dir, "main_test.go"), "package main\n\n// edited\n")
	write(t, filepath.Join(dir, "notes.md"), "edited")
	if k := key(); k != base {
		t.Fatalf("test/doc edit changed the key: %s -> %s", base, k)
	}

	// An embedded asset is compiled in: a stale binary would serve the
	// old one (the web suites embed web/dist).
	write(t, filepath.Join(dir, "msg.txt"), "changed")
	emb := key()
	if emb == base {
		t.Fatal("embedded file edit kept the key")
	}
	write(t, filepath.Join(dir, "main.go"), "package main\n\nimport _ \"embed\"\n\n//go:embed msg.txt\nvar msg string\n\nfunc main() { println(msg) }\n")
	if k := key(); k == emb {
		t.Fatal("go source edit kept the key")
	}
}

func TestBuildReusesCachedBinary(t *testing.T) {
	t.Parallel()
	dir := module(t)
	root := t.TempDir()
	first, err := Build(dir, ".", root)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(first, root) {
		t.Fatalf("binary %s not under cache root %s", first, root)
	}
	out, err := exec.Command(first).CombinedOutput()
	if err != nil || string(out) != "hello" {
		t.Fatalf("built binary: %v %q", err, out)
	}
	before, err := os.Stat(first)
	if err != nil {
		t.Fatal(err)
	}
	// A cache hit must not relink: that link is the seconds saved.
	// Blocking the go command proves no build ran.
	second, err := build(dir, ".", root, "/nonexistent/go")
	if err != nil {
		t.Fatalf("cache hit ran the go command: %v", err)
	}
	after, err := os.Stat(second)
	if err != nil {
		t.Fatal(err)
	}
	if second != first || !os.SameFile(before, after) {
		t.Fatalf("cache hit rebuilt: %s vs %s", first, second)
	}

	write(t, filepath.Join(dir, "msg.txt"), "bye")
	third, err := Build(dir, ".", root)
	if err != nil {
		t.Fatal(err)
	}
	if third == first {
		t.Fatal("changed source reused the old binary")
	}
	out, err = exec.Command(third).CombinedOutput()
	if err != nil || string(out) != "bye" {
		t.Fatalf("rebuilt binary: %v %q", err, out)
	}
}

func TestBoughBinOverrides(t *testing.T) {
	t.Parallel()
	got, err := resolve("/somewhere/bough", func() (string, error) {
		t.Fatal("built despite BOUGH_BIN")
		return "", nil
	})
	if err != nil || got != "/somewhere/bough" {
		t.Fatalf("resolve = %q, %v", got, err)
	}
}
