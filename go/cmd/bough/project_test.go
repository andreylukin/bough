package main

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
)

func TestProjectCommand(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	old := projectRuntime
	projectRuntime = func() container.Runtime { return container.NewFake() }
	t.Cleanup(func() { projectRuntime = old })
	os.MkdirAll(filepath.Join(home, "repos", "web"), 0o755)
	run := func(stdin string, args ...string) (string, error) {
		var out bytes.Buffer
		err := project(&out, strings.NewReader(stdin), args)
		return out.String(), err
	}

	if _, err := run("", "show", "web"); err == nil {
		t.Error("show of a missing project succeeded")
	}
	// A5: a refused create leaves no skeleton behind to trip the next one.
	if _, err := run("", "create", "web", "~/repos/nope"); err == nil {
		t.Error("create with a missing repo succeeded")
	}
	if _, err := run("", "create", "web", "~/repos/web"); err != nil {
		t.Fatal(err)
	}
	if _, err := run("", "add-repo", "web", "git@github.com:me/api.git", "--branch", "dev"); err != nil {
		t.Fatal(err)
	}
	for _, kv := range [][2]string{{"checks.fast", "go test ./..."}, {"env.GOFLAGS", "-mod=mod"}, {"cpus", "4"}, {"ports", "3000, 8080:80"}} {
		if _, err := run("", "set", "web", kv[0], kv[1]); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := run("git fetch\n", "write", "web", "resume.sh"); err != nil {
		t.Fatal(err)
	}

	p, err := projectdef.Load(home, "web")
	if err != nil {
		t.Fatal(err)
	}
	d := p.Def
	if len(d.Repos) != 2 || d.Repos[0].Path != "~/repos/web" || d.Repos[1].Remote != "git@github.com:me/api.git" || d.Repos[1].Branch != "dev" {
		t.Errorf("repos = %+v", d.Repos)
	}
	if d.Checks.Fast != "go test ./..." || d.Env["GOFLAGS"] != "-mod=mod" || d.CPUs != 4 {
		t.Errorf("def = %+v", d)
	}
	if len(d.Ports) != 2 || d.Ports[1] != (projectdef.Port{Host: 8080, Guest: 80}) {
		t.Errorf("ports = %+v", d.Ports)
	}
	if _, err := run("", "set", "web", "ports", "3000,3000"); err == nil {
		t.Error("duplicate host port accepted")
	}
	if s, _ := projectdef.ReadFile(home, "web", "resume.sh"); s != "git fetch\n" {
		t.Errorf("resume.sh = %q", s)
	}
	if out, _ := run("", "list"); !strings.Contains(out, "web   web, api  not built  -") {
		t.Errorf("list = %q", out)
	}

	// Validation is projectdef's: nothing invalid reaches disk.
	if _, err := run("", "remove-repo", "web", "web"); err != nil {
		t.Fatal(err)
	}
	if _, err := run("", "remove-repo", "web", "api"); err == nil {
		t.Error("removing the last repo succeeded")
	}
	if _, err := run("repo: []\n", "write", "web", "project.yml"); err == nil {
		t.Error("a typo'd project.yml was written")
	}
	if _, err := run("", "set", "web", "nope", "x"); err == nil {
		t.Error("unknown key accepted")
	}
	if _, err := run("", "set", "web", "secrets.DEVPI_URL", "vault:x"); err == nil {
		t.Error("bad secret ref accepted")
	}
	if _, err := run("", "set", "web", "secrets.GOFLAGS", "keychain:x"); err == nil {
		t.Error("secret shadowing env accepted")
	}
	if out, err := run("", "set", "web", "secrets.DEVPI_URL", "keychain:bough/web/DEVPI_URL"); err != nil || !strings.Contains(out, "DEVPI_URL: keychain:bough/web/DEVPI_URL") {
		t.Errorf("set secret: %v\n%s", err, out)
	}
	if _, err := run("", "set", "web", "secrets.DEVPI_URL", ""); err != nil {
		t.Fatal(err)
	}
	if p, _ := projectdef.Load(home, "web"); len(p.Def.Secrets) != 0 {
		t.Errorf("secret not removed: %+v", p.Def.Secrets)
	}
	// A5: host problems are refused at write time, in plain words.
	if _, err := run("", "add-repo", "web", "~/repos/missing"); err == nil || err.Error() != "project.yml: repos[1].path: ~/repos/missing does not exist" {
		t.Errorf("missing repo path: %v", err)
	}
	if _, err := run("", "set", "web", "memory", "8 gigs"); err == nil || !strings.Contains(err.Error(), `memory: "8 gigs" is not a size`) {
		t.Errorf("bad memory: %v", err)
	}
	if _, err := run("", "add-identity", "web", ".aws"); err == nil || !strings.Contains(err.Error(), "identity: ~/.aws does not exist") {
		t.Errorf("missing identity dir: %v", err)
	}
	if p, _ := projectdef.Load(home, "web"); len(p.Def.Repos) != 1 {
		t.Errorf("after refused edits repos = %+v", p.Def.Repos)
	}
}
