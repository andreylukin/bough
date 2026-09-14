package projectdef

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func git(t *testing.T, dir string, args ...string) {
	t.Helper()
	cmd := exec.Command("git", append([]string{"-C", dir, "-c", "user.email=t@t", "-c", "user.name=t", "-c", "commit.gpgsign=false"}, args...)...)
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("git %v: %v\n%s", args, err, out)
	}
}

// newRepo makes a git repo on branch main with the given files committed.
func newRepo(t *testing.T, files map[string]string) string {
	t.Helper()
	dir := t.TempDir()
	git(t, dir, "init", "-q", "-b", "main")
	for n, c := range files {
		if err := os.WriteFile(filepath.Join(dir, n), []byte(c), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	git(t, dir, "add", "-A")
	git(t, dir, "commit", "-q", "-m", "init", "--allow-empty")
	return dir
}

func TestParseValidate(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name, yml, want string
	}{
		{"ok", "repos:\n  - path: /x\n", ""},
		{"no repos", "checks: {fast: go test}\n", "at least one repo"},
		{"both", "repos:\n  - path: /x\n    remote: git@h:a/x.git\n", "exactly one"},
		{"neither", "repos:\n  - branch: main\n", "exactly one"},
		{"dup", "repos:\n  - path: /a/x\n  - remote: git@h:b/x.git\n", "duplicate"},
		{"unknown key", "repo:\n  - path: /x\n", "parse"},
	}
	for _, c := range cases {
		_, err := Parse([]byte(c.yml))
		if c.want == "" && err != nil {
			t.Errorf("%s: %v", c.name, err)
		}
		if c.want != "" && (err == nil || !strings.Contains(err.Error(), c.want)) {
			t.Errorf("%s: err %v, want %q", c.name, err, c.want)
		}
	}
	d, _ := Parse([]byte("repos:\n  - remote: git@github.com:me/thing.git\n"))
	if n := d.Repos[0].RepoName(); n != "thing" {
		t.Errorf("RepoName = %q", n)
	}
}

func TestSlug(t *testing.T) {
	t.Parallel()
	for _, s := range []string{"a", "bough", "a-1"} {
		if ValidSlug(s) != nil {
			t.Errorf("%q rejected", s)
		}
	}
	for _, s := range []string{"", "-a", "A", "a/b", "..", strings.Repeat("a", 64)} {
		if ValidSlug(s) == nil {
			t.Errorf("%q accepted", s)
		}
	}
	if _, err := Create(t.TempDir(), "../x"); err == nil {
		t.Error("Create accepted a bad slug")
	}
}

func TestCreateListWrite(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	p, err := Create(home, "demo")
	if err != nil {
		t.Fatal(err)
	}
	if p.Dir != filepath.Join(home, ".bough", "projects", "demo") {
		t.Fatalf("dir %s", p.Dir)
	}
	if _, err := Create(home, "demo"); err == nil {
		t.Fatal("second Create succeeded")
	}
	if s, _ := ReadFile(home, "demo", FileSetup); s == "" {
		t.Fatal("skeleton setup.sh missing")
	}
	if s, err := ReadFile(home, "demo", FileResume); s != "" || err != nil {
		t.Fatalf("missing resume = %q, %v", s, err)
	}
	if err := WriteFile(home, "demo", "evil.sh", "x"); err == nil {
		t.Fatal("non-editable name accepted")
	}
	if err := WriteFile(home, "demo", FileYAML, "repos: []\n"); err == nil {
		t.Fatal("empty repos accepted")
	}
	if err := WriteFile(home, "demo", FileYAML, ""); err == nil {
		t.Fatal("project.yml deleted")
	}
	// Dockerfile alongside setup.sh is allowed; Dockerfile wins.
	if err := WriteFile(home, "demo", FileDockerfile, "FROM debian\n"); err != nil {
		t.Fatal(err)
	}
	if !p.UsesDockerfile() {
		t.Fatal("Dockerfile does not win")
	}
	if err := WriteFile(home, "demo", FileDockerfile, ""); err != nil {
		t.Fatal(err)
	}
	if p.UsesDockerfile() {
		t.Fatal("empty text did not delete")
	}
	ents, _ := os.ReadDir(p.Dir)
	for _, e := range ents {
		if strings.Contains(e.Name(), ".tmp-") {
			t.Fatalf("temp file left: %s", e.Name())
		}
	}
	// A broken sibling is skipped and reported, the good one still listed.
	bad := filepath.Join(Root(home), "bad")
	os.MkdirAll(bad, 0o755)
	os.WriteFile(filepath.Join(bad, FileYAML), []byte(":::"), 0o644)
	ps, err := List(home)
	if err == nil || len(ps) != 1 || ps[0].Slug != "demo" {
		t.Fatalf("List = %v, %v", ps, err)
	}
}

func TestImageHash(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	repo := newRepo(t, map[string]string{"go.sum": "v1\n", "main.go": "package main\n"})
	p, err := Create(home, "h")
	if err != nil {
		t.Fatal(err)
	}
	yml := "repos:\n  - path: " + repo + "\n    branch: main\n"
	if err := WriteFile(home, "h", FileYAML, yml); err != nil {
		t.Fatal(err)
	}
	hash := func() string {
		t.Helper()
		p, err = Load(home, "h")
		if err != nil {
			t.Fatal(err)
		}
		h, err := ImageHash(home, p)
		if err != nil {
			t.Fatal(err)
		}
		if len(h) != 12 {
			t.Fatalf("hash %q", h)
		}
		return h
	}
	h0 := hash()
	if hash() != h0 {
		t.Fatal("unstable")
	}
	// Unrelated repo file and uncommitted lockfile edits do not rebuild.
	os.WriteFile(filepath.Join(repo, "main.go"), []byte("package main // x\n"), 0o644)
	git(t, repo, "commit", "-qam", "code")
	os.WriteFile(filepath.Join(repo, "go.sum"), []byte("dirty\n"), 0o644)
	if hash() != h0 {
		t.Fatal("unrelated change moved the hash")
	}
	git(t, repo, "commit", "-qam", "lock")
	h1 := hash()
	if h1 == h0 {
		t.Fatal("lockfile at branch head did not move the hash")
	}
	WriteFile(home, "h", FileSetup, "#!/bin/sh\necho changed\n")
	h2 := hash()
	if h2 == h1 {
		t.Fatal("setup.sh did not move the hash")
	}
	WriteFile(home, "h", FileYAML, yml+"checks: {fast: make}\n")
	h3 := hash()
	if h3 == h2 {
		t.Fatal("project.yml did not move the hash")
	}
	WriteFile(home, "h", FileDockerfile, "FROM debian\n")
	h4 := hash()
	if h4 == h3 {
		t.Fatal("Dockerfile did not move the hash")
	}
	WriteFile(home, "h", FileSetup, "#!/bin/sh\necho ignored now\n")
	if hash() != h4 {
		t.Fatal("setup.sh counted while Dockerfile wins")
	}
	if ImageTag("h", h4) != "bough-orb/h:"+h4 {
		t.Fatal("tag")
	}
}
