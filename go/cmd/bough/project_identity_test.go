package main

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/projectdef"
)

func TestProjectIdentityCommands(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	os.MkdirAll(filepath.Join(home, "repos", "ci"), 0o755)
	run := func(args ...string) error {
		return project(&bytes.Buffer{}, strings.NewReader(""), args)
	}
	if err := run("create", "ci", "~/repos/ci"); err != nil {
		t.Fatal(err)
	}
	os.MkdirAll(filepath.Join(home, ".circleci"), 0o755)
	os.MkdirAll(filepath.Join(home, ".config", "foo"), 0o755)
	for _, dir := range []string{"~/.circleci", ".circleci", ".config/foo"} {
		if err := run("add-identity", "ci", dir); err != nil {
			t.Fatalf("add-identity %s: %v", dir, err)
		}
	}
	// A new project starts with the gh identity (projectdef.defaultIdentity);
	// the temp HOME has no Parallel CLI dir, so gh is the only default.
	if p, _ := projectdef.Load(home, "ci"); strings.Join(p.Def.Identity, ",") != "gh,.circleci,.config/foo" {
		t.Errorf("identity = %v, want .circleci once and .config/foo", p.Def.Identity)
	}
	if err := run("add-identity", "ci", ".ssh"); err == nil {
		t.Error(".ssh accepted")
	}
	if err := run("remove-identity", "ci", ".circleci"); err != nil {
		t.Fatal(err)
	}
	if err := run("remove-identity", "ci", ".circleci"); err == nil {
		t.Error("removing an absent identity dir succeeded")
	}
	if p, _ := projectdef.Load(home, "ci"); strings.Join(p.Def.Identity, ",") != "gh,.config/foo" {
		t.Errorf("after remove identity = %v", p.Def.Identity)
	}
}
