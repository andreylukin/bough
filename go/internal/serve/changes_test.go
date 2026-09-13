package serve

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
)

func TestChanges(t *testing.T) {
	t.Parallel()
	if _, ok := Changes(context.Background(), t.TempDir()); ok {
		t.Fatal("a plain directory is not a repository")
	}
	dir := t.TempDir()
	run := func(args ...string) {
		t.Helper()
		cmd := exec.Command("git", append([]string{"-C", dir, "-c", "user.email=t@t", "-c", "user.name=t"}, args...)...)
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v\n%s", args, err, out)
		}
	}
	write := func(name, body string) {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	run("init", "-q")
	write("a.go", "one\ntwo\n")
	run("add", ".")
	run("commit", "-qm", "init")
	write("a.go", "one\nthree\nfour\n")
	write("b.go", "new\n")

	files, ok := Changes(context.Background(), dir)
	if !ok || len(files) != 2 {
		t.Fatalf("changes = %+v ok=%v, want a.go and b.go", files, ok)
	}
	if files[0] != (Change{Path: "a.go", Add: 2, Del: 1}) || !files[1].New || files[1].Path != "b.go" {
		t.Fatalf("changes = %+v", files)
	}
}
