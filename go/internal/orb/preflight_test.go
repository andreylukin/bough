package orb

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/secrets"
)

type downRuntime struct{ *container.Fake }

func (downRuntime) Available(context.Context) error { return errors.New("run: container system start") }

func TestPreflight(t *testing.T) {
	home := t.TempDir()
	repo := filepath.Join(home, "app")
	if out, err := exec.Command("git", "init", "-q", repo).CombinedOutput(); err != nil {
		t.Fatal(err, string(out))
	}
	os.MkdirAll(filepath.Join(home, "notgit"), 0o755)
	origin := filepath.Join(home, "origin.git")
	exec.Command("git", "init", "-q", "--bare", origin).Run()
	exec.Command("git", "-C", repo, "commit", "-q", "--allow-empty", "-m", "x").Run()
	exec.Command("git", "-C", repo, "push", "-q", origin, "HEAD:refs/heads/main").Run()

	origHost, origRead := hostCommand, secrets.KeychainRead
	t.Cleanup(func() { hostCommand, secrets.KeychainRead = origHost, origRead; ghToken.val = "" })
	hostCommand = func(string, ...string) string { return "" }
	ghToken.val = ""
	secrets.KeychainRead = func(service string) (string, error) {
		if service == "bough/p/GOOD" {
			return "s3cr3t-value", nil
		}
		return "", secrets.ErrNotFound
	}
	p := projectdef.Project{Slug: "p", Def: projectdef.Def{
		Repos: []projectdef.Repo{
			{Path: repo},
			{Path: filepath.Join(home, "notgit")},
			{Remote: "file://" + origin},
			{Remote: "file://" + filepath.Join(home, "missing.git")},
		},
		Identity: []string{projectdef.IdentityGitHub},
		Secrets:  map[string]string{"GOOD": "keychain:bough/p/GOOD", "BAD": "keychain:bough/p/BAD"},
	}}
	checks := Preflight(context.Background(), home, downRuntime{container.NewFake()}, p)
	var got []string
	for _, c := range checks {
		got = append(got, c.Kind+" "+c.Name+" "+string(c.Status))
		if strings.Contains(c.Detail, "s3cr3t") {
			t.Fatalf("secret value leaked: %+v", c)
		}
	}
	want := []string{
		"runtime fake fail",
		"clone app ok",
		"clone notgit fail",
		"clone origin ok",
		"clone missing fail",
		"gh GitHub token fail",
		"secret BAD fail",
		"secret GOOD ok",
	}
	if strings.Join(got, "\n") != strings.Join(want, "\n") {
		t.Fatalf("got\n%s\nwant\n%s", strings.Join(got, "\n"), strings.Join(want, "\n"))
	}
	for _, c := range checks {
		if c.Status == PreflightFail && c.Detail == "" {
			t.Errorf("failed check without a reason: %+v", c)
		}
	}
	// No gh identity: no gh check. Runtime up: ok.
	p.Def.Identity, p.Def.Secrets, p.Def.Repos = nil, nil, nil
	checks = Preflight(context.Background(), home, container.NewFake(), p)
	if len(checks) != 1 || checks[0].Status != PreflightOK {
		t.Fatalf("bare project: %+v", checks)
	}
}

func TestPreflightRemoteCredentialsNotEchoed(t *testing.T) {
	home := t.TempDir()
	c := cloneCheck(context.Background(), home, "p", projectdef.Repo{Remote: "https://user:ghp_leaky123@127.0.0.1:1/x.git"})
	if c.Status != PreflightFail || strings.Contains(c.Detail, "ghp_leaky123") {
		t.Fatalf("credential in detail: %+v", c)
	}
}
