package main

import (
	"bytes"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/secrets"
)

// B4: `project status` prints one preflight line per check and fails
// when any check fails, never printing a secret value.
func TestProjectStatusPreflight(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	old := projectRuntime
	projectRuntime = func() container.Runtime { return container.NewFake() }
	origRead := secrets.KeychainRead
	secrets.KeychainRead = func(service string) (string, error) {
		if strings.HasSuffix(service, "/GOOD") {
			return "hunter22-secret", nil
		}
		return "", secrets.ErrNotFound
	}
	t.Cleanup(func() { projectRuntime, secrets.KeychainRead = old, origRead })
	repo := filepath.Join(home, "app")
	if out, err := exec.Command("git", "init", "-q", repo).CombinedOutput(); err != nil {
		t.Fatal(err, string(out))
	}
	var out bytes.Buffer
	if err := project(&out, strings.NewReader(""), []string{"create", "zz", repo}); err != nil {
		t.Fatal(err)
	}
	// A new project lends gh, and its preflight shells out to the host's
	// `gh auth token` (/opt/homebrew/bin first), so the result followed
	// whoever ran the suite. Drop it here; orb.TestPreflight covers the gh
	// check with hostCommand stubbed.
	if err := mutate(&out, home, "zz", func(d *projectdef.Def) error { d.Identity = nil; return nil }); err != nil {
		t.Fatal(err)
	}
	for _, kv := range [][2]string{{"secrets.GOOD", "keychain:bough/zz/GOOD"}, {"secrets.MISSING", "keychain:bough/zz/MISSING"}} {
		if err := project(&out, nil, []string{"set", "zz", kv[0], kv[1]}); err != nil {
			t.Fatal(err)
		}
	}
	out.Reset()
	err := project(&out, nil, []string{"status", "zz"})
	got := out.String()
	for _, want := range []string{"ok    runtime  fake", "ok    clone    app", "ok    secret   GOOD", "fail  secret   MISSING  keychain:bough/zz/MISSING not found"} {
		if !strings.Contains(got, want) {
			t.Errorf("missing %q in\n%s", want, got)
		}
	}
	if strings.Contains(got, "hunter22") {
		t.Fatal("secret value printed")
	}
	if err == nil || !strings.Contains(err.Error(), "1 preflight check failed") {
		t.Fatalf("err = %v", err)
	}
}
