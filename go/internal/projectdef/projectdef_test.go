package projectdef

import (
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"gopkg.in/yaml.v3"
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
		{"no repos", "checks: {fast: go test}\n", ""},
		{"empty repos", "repos: []\n", ""},
		{"both", "repos:\n  - path: /x\n    remote: git@h:a/x.git\n", "exactly one"},
		{"neither", "repos:\n  - branch: main\n", "exactly one"},
		{"dup", "repos:\n  - path: /a/x\n  - remote: git@h:b/x.git\n", "duplicate"},
		{"unknown key", "repo:\n  - path: /x\n", "parse"},
		{"secret ok", "repos:\n  - path: /x\nsecrets:\n  DEVPI_URL: keychain:bough/web/DEVPI_URL\n", ""},
		{"secret bad name", "repos:\n  - path: /x\nsecrets:\n  1X: keychain:a\n", "secrets.1X: bad env name"},
		{"secret reserved", "repos:\n  - path: /x\nsecrets:\n  PATH: keychain:a\n", "secrets.PATH: reserved env name"},
		{"secret reserved prefix", "repos:\n  - path: /x\nsecrets:\n  GIT_CONFIG_COUNT: keychain:a\n", "secrets.GIT_CONFIG_COUNT: reserved env name"},
		{"env reserved", "repos:\n  - path: /x\nenv:\n  HTTPS_PROXY: http://evil\n", "env.HTTPS_PROXY: reserved env name"},
		{"env reserved prefix", "repos:\n  - path: /x\nenv:\n  GIT_CONFIG_KEY_0: x\n", "env.GIT_CONFIG_KEY_0: reserved env name"},
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
	// A project with no repos is a project: a brief and an orb, nothing
	// checked out.
	if err := WriteFile(home, "demo", FileYAML, "repos: []\n"); err != nil {
		t.Fatalf("empty repos refused: %v", err)
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
	os.MkdirAll(filepath.Join(home, ".circleci"), 0o755)
	for _, extra := range []string{
		"checks: {fast: make}\n", "env: {A: b}\n", "identity: [.circleci]\n",
		"cpus: 4\n", "memory: 8G\n", "ports: [3000]\n", "caches: [/root/.cache/uv]\n", "lsp: [.]\n",
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
	if err := WriteFile(home, "s", FileYAML, "repos:\n  - path: "+home+"\nenv: {DEVPI_URL: https://user:pw@devpi}\n"); err != nil {
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

// A5: project.yml is checked against the host when it is written, so a
// typo'd path or size fails at save instead of at the next session start.
func TestWriteChecksHost(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	if _, err := Create(home, "v"); err != nil {
		t.Fatal(err)
	}
	os.MkdirAll(filepath.Join(home, "repos", "real"), 0o755)
	os.MkdirAll(filepath.Join(home, ".circleci"), 0o755)
	ok := "repos:\n  - path: ~/repos/real\nmemory: 8G\ncpus: 2\nidentity: [gh, .circleci]\n"
	if err := WriteFile(home, "v", FileYAML, ok); err != nil {
		t.Fatalf("valid yml refused: %v", err)
	}
	cases := []struct{ name, yml, want string }{
		{"placeholder", "repos:\n  - path: ~/repos/example\n", "repos[0].path: ~/repos/example is the template placeholder"},
		{"missing repo", "repos:\n  - path: ~/repos/nope\n", "repos[0].path: ~/repos/nope does not exist"},
		{"repo is file", "repos:\n  - path: ~/.bough/projects/v/project.yml\n", "is not a directory"},
		{"memory", "repos:\n  - path: ~/repos/real\nmemory: lots\n", `memory: "lots" is not a size`},
		{"memory unit only", "repos:\n  - path: ~/repos/real\nmemory: G\n", `memory: "G" is not a size`},
		{"cpus", "repos:\n  - path: ~/repos/real\ncpus: two\n", "cpus"},
		{"identity dir", "repos:\n  - path: ~/repos/real\nidentity: [.aws]\n", "identity: ~/.aws does not exist"},
	}
	for _, c := range cases {
		err := WriteFile(home, "v", FileYAML, c.yml)
		if err == nil || !strings.Contains(err.Error(), c.want) {
			t.Errorf("%s: err %v, want %q", c.name, err, c.want)
		}
	}
	// Every problem is reported at once, without Go's error-chain prefixes.
	err := WriteFile(home, "v", FileYAML, "repos:\n  - path: ~/repos/nope\nmemory: 8 gigs\nidentity: [.aws]\n")
	if err == nil || strings.Count(err.Error(), "\n") != 3 || strings.Contains(err.Error(), "projectdef:") {
		t.Errorf("combined err = %v", err)
	}
	if got, _ := ReadFile(home, "v", FileYAML); got != ok {
		t.Errorf("refused write reached disk: %q", got)
	}
	// Remote repos are not checked on the host.
	if err := WriteFile(home, "v", FileYAML, "repos:\n  - remote: git@h:a/x.git\nmemory: 512M\n"); err != nil {
		t.Errorf("remote: %v", err)
	}
}

// A5: storing a secret ref is not a host edit; it must work on a fresh
// skeleton whose repo is still the placeholder.
func TestSetSecretSkipsHostCheck(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	if _, err := Create(home, "fresh"); err != nil {
		t.Fatal(err)
	}
	if err := SetSecret(home, "fresh", "TOKEN", "keychain:bough/fresh/TOKEN"); err != nil {
		t.Fatalf("SetSecret on skeleton: %v", err)
	}
}

func TestParsePorts(t *testing.T) {
	t.Parallel()
	d, err := Parse([]byte("repos:\n  - path: /x\nports: [3000, \"8080:80\"]\n"))
	if err != nil {
		t.Fatal(err)
	}
	want := []Port{{Host: 3000, Guest: 3000}, {Host: 8080, Guest: 80}}
	if !slices.Equal(d.Ports, want) {
		t.Fatalf("ports = %v", d.Ports)
	}
	b, _ := yaml.Marshal(d)
	if back, err := Parse(b); err != nil || !slices.Equal(back.Ports, want) {
		t.Fatalf("round trip %s: %v %v", b, back.Ports, err)
	}
	for yml, msg := range map[string]string{
		"ports: [0]\n":              "ports: \"",
		"ports: [70000]\n":          "ports: \"",
		"ports: [\"web\"]\n":        "ports: \"",
		"ports: [\"1:2:3\"]\n":      "ports: \"",
		"ports: [3000, \"3000:1\"]": "ports[1]: host port 3000 is listed twice",
	} {
		if _, err := Parse([]byte("repos:\n  - path: /x\n" + yml)); err == nil || !strings.Contains(err.Error(), msg) {
			t.Errorf("%q: err %v, want %q", yml, err, msg)
		}
	}
	if p, err := ParsePorts("3000, 8080:80"); err != nil || !slices.Equal(p, want) {
		t.Fatalf("ParsePorts = %v %v", p, err)
	}
	if p, err := ParsePorts(""); err != nil || p != nil {
		t.Fatalf("ParsePorts empty = %v %v", p, err)
	}
}

// The runtime's -m takes IEC sizes: `container` allocates exactly 8 GiB
// for 8GiB. Validation used to refuse them, and because `bough project
// set` validates before writing, one such size in project.yml blocked
// every later edit to that project.
func TestMemorySizesTheRuntimeTakes(t *testing.T) {
	for _, size := range []string{"8G", "8g", "8GB", "8gb", "8GiB", "8Gi", "512M", "512MiB", "1T", "1P", "2048"} {
		if !memoryRE.MatchString(size) {
			t.Errorf("memory %q rejected, but the runtime takes it", size)
		}
	}
	for _, bad := range []string{"lots", "8Q", "G", "", "-8G", "8.5G"} {
		if memoryRE.MatchString(bad) {
			t.Errorf("memory %q accepted, but it is not a size", bad)
		}
	}
}

// MEMORY.md is prose: it is not parsed, and clearing the editor leaves an
// empty brief instead of deleting the file the way an empty script does.
func TestMemoryFile(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	if _, err := Create(home, "m"); err != nil {
		t.Fatal(err)
	}
	if s, err := ReadFile(home, "m", FileMemory); s != "" || err != nil {
		t.Fatalf("missing MEMORY.md = %q, %v", s, err)
	}
	brief := "# bough\n\nThe orb builds from setup.sh: not a repo: a brief.\n"
	if err := WriteFile(home, "m", FileMemory, brief); err != nil {
		t.Fatal(err)
	}
	if s, _ := ReadFile(home, "m", FileMemory); s != brief {
		t.Fatalf("round trip = %q", s)
	}
	path := filepath.Join(Root(home), "m", FileMemory)
	if fi, err := os.Stat(path); err != nil || fi.Mode().Perm() != 0o644 {
		t.Fatalf("stat = %v, %v", fi, err)
	}
	if err := WriteFile(home, "m", FileMemory, ""); err != nil {
		t.Fatal(err)
	}
	if fi, err := os.Stat(path); err != nil || fi.Size() != 0 {
		t.Fatalf("empty write did not leave an empty file: %v, %v", fi, err)
	}
	// An empty script still deletes.
	if err := WriteFile(home, "m", FileSetup, ""); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(Root(home), "m", FileSetup)); !os.IsNotExist(err) {
		t.Fatalf("empty setup.sh survived: %v", err)
	}
}

// The name is spliced into project.yml as text: a Marshal round trip of
// Def would drop the comments the file ships and the user adds.
func TestSetName(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	p, err := Create(home, "demo")
	if err != nil {
		t.Fatal(err)
	}
	if p.DisplayName() != "demo" {
		t.Fatalf("DisplayName = %q, want the slug", p.DisplayName())
	}
	before, _ := ReadFile(home, "demo", FileYAML)
	if err := SetName(home, "demo", "Bough web"); err != nil {
		t.Fatal(err)
	}
	got, _ := ReadFile(home, "demo", FileYAML)
	for _, keep := range []string{"# bough project definition", "# caches: [/root/.cache/go-build]", "# env: {GOFLAGS: -mod=mod}", Placeholder} {
		if !strings.Contains(got, keep) {
			t.Fatalf("SetName dropped %q:\n%s", keep, got)
		}
	}
	if strings.Index(got, "repos:") > strings.Index(got, "checks:") {
		t.Fatalf("key order moved:\n%s", got)
	}
	if !strings.Contains(got, "\nname: Bough web\n") {
		t.Fatalf("name line missing:\n%s", got)
	}
	if again := func() string { SetName(home, "demo", "Bough web"); s, _ := ReadFile(home, "demo", FileYAML); return s }(); again != got {
		t.Fatalf("not idempotent:\n%s", again)
	}
	if err := SetName(home, "demo", "Bough: the web one"); err != nil {
		t.Fatal(err)
	}
	renamed, _ := ReadFile(home, "demo", FileYAML)
	if strings.Count(renamed, "\nname:") != 1 {
		t.Fatalf("second name line:\n%s", renamed)
	}
	q, err := Load(home, "demo")
	if err != nil || q.DisplayName() != "Bough: the web one" {
		t.Fatalf("DisplayName = %q, %v", q.Def.Name, err)
	}
	if lines := strings.Count(renamed, "\n"); lines != strings.Count(before, "\n")+1 {
		t.Fatalf("line count moved by %d", lines-strings.Count(before, "\n"))
	}
	for _, bad := range []string{"", "  ", "one\ntwo"} {
		if err := SetName(home, "demo", bad); err == nil {
			t.Errorf("SetName(%q) accepted", bad)
		}
	}
	if err := SetName(home, "missing", "x"); err == nil {
		t.Fatal("SetName on a missing project")
	}
}

// A label-only project migrates into a definition with no repos and no
// build script; it must save again through the web editor unchanged.
func TestCreateEmpty(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	p, err := CreateEmpty(home, "lab", "Lab notes")
	if err != nil {
		t.Fatal(err)
	}
	if len(p.Def.Repos) != 0 || p.DisplayName() != "Lab notes" {
		t.Fatalf("def %+v", p.Def)
	}
	for _, f := range []string{FileSetup, FileDockerfile, FileResume, FileMemory} {
		if _, err := os.Stat(filepath.Join(p.Dir, f)); !os.IsNotExist(err) {
			t.Fatalf("CreateEmpty wrote %s", f)
		}
	}
	yml, _ := ReadFile(home, "lab", FileYAML)
	if strings.Contains(yml, Placeholder) {
		t.Fatalf("placeholder in a migrated definition:\n%s", yml)
	}
	if err := WriteFile(home, "lab", FileYAML, yml); err != nil {
		t.Fatalf("re-saving what CreateEmpty wrote: %v", err)
	}
	if _, err := CreateEmpty(home, "lab", "again"); err == nil {
		t.Fatal("second CreateEmpty succeeded")
	}
	if p, err := CreateEmpty(home, "plain", ""); err != nil || p.DisplayName() != "plain" {
		t.Fatalf("unnamed = %+v, %v", p, err)
	}
}

// A broken project.yml must still list: the only editor that can fix it
// is on that project's own page.
func TestListAll(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	if _, err := Create(home, "good"); err != nil {
		t.Fatal(err)
	}
	bad := filepath.Join(Root(home), "bad")
	os.MkdirAll(bad, 0o755)
	os.WriteFile(filepath.Join(bad, FileYAML), []byte("repo:\n  - path: /x\n"), 0o644)
	os.MkdirAll(filepath.Join(Root(home), "no-yaml"), 0o755)
	os.MkdirAll(filepath.Join(Root(home), "Bad Slug"), 0o755)
	all := ListAll(home)
	if len(all) != 2 || all[0].Slug != "bad" || all[1].Slug != "good" {
		t.Fatalf("ListAll = %+v", all)
	}
	if all[0].Err == nil || !strings.Contains(all[0].Err.Error(), "bad") {
		t.Fatalf("broken entry = %+v", all[0])
	}
	if all[1].Err != nil || all[1].Dir != filepath.Join(Root(home), "good") {
		t.Fatalf("good entry = %+v", all[1])
	}
	if ps, err := List(home); err == nil || len(ps) != 1 {
		t.Fatalf("List = %v, %v", ps, err)
	}
	if all := ListAll(filepath.Join(home, "nope")); all != nil {
		t.Fatalf("no root = %+v", all)
	}
}
