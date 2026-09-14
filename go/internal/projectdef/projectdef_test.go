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
		{"secret ok", "repos:\n  - path: /x\nsecrets:\n  DEVPI_URL: keychain:bough/web/DEVPI_URL\n", ""},
		{"secret bad name", "repos:\n  - path: /x\nsecrets:\n  1X: keychain:a\n", "secrets.1X: bad env name"},
		{"secret reserved", "repos:\n  - path: /x\nsecrets:\n  PATH: keychain:a\n", "secrets.PATH: reserved env name"},
		{"secret reserved prefix", "repos:\n  - path: /x\nsecrets:\n  GIT_CONFIG_COUNT: keychain:a\n", "secrets.GIT_CONFIG_COUNT: reserved env name"},
		{"secret scheme", "repos:\n  - path: /x\nsecrets:\n  X: vault:a\n", `secrets.X: unknown ref scheme "vault" (want keychain:)`},
		{"secret empty", "repos:\n  - path: /x\nsecrets:\n  X: 'keychain:'\n", "secrets.X: bad keychain service"},
		{"secret space", "repos:\n  - path: /x\nsecrets:\n  X: keychain:a b\n", "secrets.X: bad keychain service"},
		{"secret long", "repos:\n  - path: /x\nsecrets:\n  X: keychain:" + strings.Repeat("a", 201) + "\n", "secrets.X: bad keychain service"},
		{"secret in env", "repos:\n  - path: /x\nenv: {X: y}\nsecrets:\n  X: keychain:a\n", "secrets.X: also set in env"},
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
	repo := newRepo(t, map[string]string{"go.sum": "v1\n", "uv.lock": "u1\n", "main.go": "package main\n"})
	p, err := Create(home, "h")
	if err != nil {
		t.Fatal(err)
	}
	yml := "repos:\n  - path: " + repo + "\n    branch: main\n"
	if err := WriteFile(home, "h", FileYAML, yml); err != nil {
		t.Fatal(err)
	}
	setup := "#!/bin/sh\nset -e\n# bough:step apt\ntrue\n# bough:step deps\n# bough:uses go.sum\ntrue\n"
	if err := WriteFile(home, "h", FileSetup, setup); err != nil {
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
	// Run-time settings never rebuild.
	if err := SetSecret(home, "h", "DEVPI_URL", "keychain:bough/h/DEVPI_URL"); err != nil {
		t.Fatal(err)
	}
	for _, extra := range []string{
		"checks: {fast: make}\n", "env: {A: b}\n", "identity: [.circleci]\n",
		"cpus: 4\n", "memory: 8G\n", "caches: [/root/.cache/uv]\n", "lsp: [.]\n",
	} {
		if err := WriteFile(home, "h", FileYAML, yml+extra); err != nil {
			t.Fatal(err)
		}
		if hash() != h0 {
			t.Fatalf("%q moved the hash", extra)
		}
	}
	WriteFile(home, "h", FileResume, "#!/bin/sh\necho resume\n")
	if hash() != h0 {
		t.Fatal("resume.sh moved the hash")
	}
	WriteFile(home, "h", FileYAML, yml)
	// Unrelated repo files, uncommitted lockfile edits and lockfiles no
	// step declares do not rebuild.
	os.WriteFile(filepath.Join(repo, "main.go"), []byte("package main // x\n"), 0o644)
	os.WriteFile(filepath.Join(repo, "uv.lock"), []byte("u2\n"), 0o644)
	git(t, repo, "commit", "-qam", "code")
	os.WriteFile(filepath.Join(repo, "go.sum"), []byte("dirty\n"), 0o644)
	if hash() != h0 {
		t.Fatal("unrelated change moved the hash")
	}
	git(t, repo, "commit", "-qam", "lock")
	h1 := hash()
	if h1 == h0 {
		t.Fatal("declared lockfile at branch head did not move the hash")
	}
	WriteFile(home, "h", FileSetup, setup+"echo changed\n")
	h2 := hash()
	if h2 == h1 {
		t.Fatal("setup.sh did not move the hash")
	}
	WriteFile(home, "h", FileYAML, yml+"base: docker.io/library/debian:trixie\n")
	h3 := hash()
	if h3 == h2 {
		t.Fatal("base did not move the hash")
	}
	WriteFile(home, "h", FileDockerfile, "FROM debian\n")
	h4 := hash()
	if h4 == h3 {
		t.Fatal("Dockerfile did not move the hash")
	}
	// The Dockerfile's build context is the project dir minus project.yml
	// and resume.sh.
	WriteFile(home, "h", FileYAML, yml+"base: docker.io/library/debian:trixie\nchecks: {fast: make}\n")
	WriteFile(home, "h", FileResume, "#!/bin/sh\necho other\n")
	if hash() != h4 {
		t.Fatal("project.yml or resume.sh moved the Dockerfile hash")
	}
	os.WriteFile(filepath.Join(p.Dir, "setup.sh"), []byte("#!/bin/sh\necho copied\n"), 0o644)
	h5 := hash()
	if h5 == h4 {
		t.Fatal("a build-context file did not move the Dockerfile hash")
	}
	os.WriteFile(filepath.Join(p.Dir, "conf.toml"), []byte("x=1\n"), 0o644)
	if hash() == h5 {
		t.Fatal("a new build-context file did not move the Dockerfile hash")
	}
	// A Dockerfile that COPYs resume.sh bakes it in, so it is an input.
	WriteFile(home, "h", FileDockerfile, "FROM debian\nCOPY resume.sh /usr/local/bin/\n")
	h6 := hash()
	WriteFile(home, "h", FileResume, "#!/bin/sh\necho third\n")
	if hash() == h6 {
		t.Fatal("resume.sh named by the Dockerfile did not move the hash")
	}
	if ImageTag("h", h4) != "bough-orb/h:"+h4 {
		t.Fatal("tag")
	}
}

func TestParseSteps(t *testing.T) {
	t.Parallel()
	plain := "#!/bin/bash\nset -e\napt-get update\n"
	if s, err := ParseSteps(plain); err != nil || len(s) != 1 || s[0].Script != plain || s[0].Name != "setup" {
		t.Fatalf("no markers = %+v, %v", s, err)
	}
	text := "#!/bin/bash\nset -e\n# bough:step apt\napt-get update\n# bough:step deps\n# bough:uses requirements.txt go/go.sum\nuv pip install\n"
	s, err := ParseSteps(text)
	if err != nil || len(s) != 2 {
		t.Fatalf("steps = %+v, %v", s, err)
	}
	if s[0].Name != "apt" || s[0].Script != "#!/bin/bash\nset -e\n# bough:step apt\napt-get update\n" || len(s[0].Uses) != 0 {
		t.Fatalf("step 1 = %+v", s[0])
	}
	if s[1].Name != "deps" || !strings.HasPrefix(s[1].Script, "#!/bin/bash\nset -e\n# bough:step deps\n") || strings.Join(s[1].Uses, ",") != "requirements.txt,go/go.sum" {
		t.Fatalf("step 2 = %+v", s[1])
	}
	for _, bad := range []string{
		"# bough:uses go.sum\n",
		"# bough:step Bad Name\n",
		"# bough:step a\n# bough:step a\n",
		"# bough:step a\n# bough:uses ../x\n",
		"# bough:step a\n# bough:uses /etc/passwd\n",
		"# bough:step a\n# bough:uses\n",
	} {
		if _, err := ParseSteps(bad); err == nil {
			t.Errorf("ParseSteps(%q) accepted", bad)
		}
	}
}

// A declared file must exist in a repo at its base ref, at any depth.
func TestStepLockfiles(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	repo := newRepo(t, map[string]string{"README": "hi\n"})
	os.MkdirAll(filepath.Join(repo, "go"), 0o755)
	os.WriteFile(filepath.Join(repo, "go", "go.sum"), []byte("v1\n"), 0o644)
	git(t, repo, "add", "-A")
	git(t, repo, "commit", "-qm", "locks")
	if _, err := Create(home, "m"); err != nil {
		t.Fatal(err)
	}
	WriteFile(home, "m", FileYAML, "repos:\n  - path: "+repo+"\n    branch: main\n")
	err := WriteFile(home, "m", FileSetup, "# bough:step deps\n# bough:uses go/nope.sum\ntrue\n")
	if err == nil || !strings.Contains(err.Error(), "go/nope.sum") {
		t.Fatalf("unknown file accepted: %v", err)
	}
	if err := WriteFile(home, "m", FileSetup, "# bough:step deps\n# bough:uses go/go.sum\ntrue\n"); err != nil {
		t.Fatal(err)
	}
	p, _ := Load(home, "m")
	lfs, err := StepLockfiles(home, p, []string{"go/go.sum"})
	if err != nil || len(lfs) != 1 || lfs[0].Rel() != filepath.Base(repo)+"/go/go.sum" || string(lfs[0].Data) != "v1\n" {
		t.Fatalf("lockfiles = %+v, %v", lfs, err)
	}
}

func TestSetSecret(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	if _, err := Create(home, "s"); err != nil {
		t.Fatal(err)
	}
	if err := WriteFile(home, "s", FileYAML, "repos:\n  - path: /x\nenv: {DEVPI_URL: https://user:pw@devpi}\n"); err != nil {
		t.Fatal(err)
	}
	if err := SetSecret(home, "s", "DEVPI_URL", "keychain:bough/s/DEVPI_URL"); err != nil {
		t.Fatal(err)
	}
	p, err := Load(home, "s")
	if err != nil {
		t.Fatal(err)
	}
	if p.Def.Secrets["DEVPI_URL"] != "keychain:bough/s/DEVPI_URL" || len(p.Def.Env) != 0 {
		t.Fatalf("def %+v", p.Def)
	}
	if b, _ := os.ReadFile(filepath.Join(p.Dir, FileYAML)); strings.Contains(string(b), "pw@devpi") {
		t.Fatalf("value on disk: %s", b)
	}
	if err := SetSecret(home, "s", "X", "vault:y"); err == nil {
		t.Fatal("bad ref written")
	}
	if err := SetSecret(home, "missing", "X", "keychain:y"); err == nil {
		t.Fatal("missing project")
	}
}
