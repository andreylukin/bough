package main

import (
	"bytes"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/projectdef"
)

func TestProjectCommand(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	run := func(stdin string, args ...string) (string, error) {
		var out bytes.Buffer
		err := project(&out, strings.NewReader(stdin), args)
		return out.String(), err
	}

	if _, err := run("", "create", "web", "~/repos/web"); err != nil {
		t.Fatal(err)
	}
	if _, err := run("", "add-repo", "web", "git@github.com:me/api.git", "--branch", "dev"); err != nil {
		t.Fatal(err)
	}
	for _, kv := range [][2]string{{"checks.fast", "go test ./..."}, {"env.GOFLAGS", "-mod=mod"}, {"cpus", "4"}} {
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
	if s, _ := projectdef.ReadFile(home, "web", "resume.sh"); s != "git fetch\n" {
		t.Errorf("resume.sh = %q", s)
	}
	if out, _ := run("", "list"); out != "web\tweb, api\n" {
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
	if p, _ := projectdef.Load(home, "web"); len(p.Def.Repos) != 1 {
		t.Errorf("after refused edits repos = %+v", p.Def.Repos)
	}
}
