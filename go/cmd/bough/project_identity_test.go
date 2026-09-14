package main

import (
	"bytes"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/projectdef"
)

func TestProjectIdentityCommands(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	run := func(args ...string) error {
		return project(&bytes.Buffer{}, strings.NewReader(""), args)
	}
	if err := run("create", "ci", "~/repos/ci"); err != nil {
		t.Fatal(err)
	}
	for _, dir := range []string{"~/.circleci", ".circleci", ".config/foo"} {
		if err := run("add-identity", "ci", dir); err != nil {
			t.Fatalf("add-identity %s: %v", dir, err)
		}
	}
	if p, _ := projectdef.Load(home, "ci"); strings.Join(p.Def.Identity, ",") != ".circleci,.config/foo" {
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
	if p, _ := projectdef.Load(home, "ci"); strings.Join(p.Def.Identity, ",") != ".config/foo" {
		t.Errorf("after remove identity = %v", p.Def.Identity)
	}
}
