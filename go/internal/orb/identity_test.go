package orb

import (
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
)

func TestIdentityMountsDefaultEmpty(t *testing.T) {
	home := t.TempDir()
	for _, d := range []string{".aws", ".kube", ".config/gcx", ".config/argocd", ".config/gcloud", ".circleci"} {
		if err := os.MkdirAll(filepath.Join(home, d), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	if ms := identityMounts(home, nil); len(ms) != 0 {
		t.Errorf("no identity listed, mounts = %+v", ms)
	}
}

func TestIdentityMountsOptIn(t *testing.T) {
	home := t.TempDir()
	for _, d := range []string{".circleci", ".config/foo", ".aws"} {
		if err := os.MkdirAll(filepath.Join(home, d), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	// .aws is listed twice; .missing does not exist on the host; gh is no dir.
	ms := identityMounts(home, []string{".config/foo", ".aws:rw", ".aws", ".missing", "gh"})
	got := map[string]int{}
	rw := map[string]bool{}
	for _, m := range ms {
		got[m.Target]++
		rw[m.Target] = !m.ReadOnly
		if m.Source != filepath.Join(home, m.Target[len("/root/"):]) {
			t.Errorf("mount %+v: want the same $HOME path", m)
		}
	}
	if len(ms) != 2 || got["/root/.config/foo"] != 1 || got["/root/.aws"] != 1 {
		t.Errorf("mounts = %+v, want .config/foo and .aws once each", ms)
	}
	if rw["/root/.config/foo"] || !rw["/root/.aws"] {
		t.Errorf("mounts = %+v, want .config/foo read-only and .aws read-write", ms)
	}
}

func TestIdentityEnvGitHubOptIn(t *testing.T) {
	old := hostCommand
	t.Cleanup(func() { hostCommand = old; ghToken.val = "" })
	hostCommand = func(name string, args ...string) string {
		if name == "gh" {
			return "tok"
		}
		return ""
	}
	ghToken.val = ""
	has := func(env []string, s string) bool {
		return slices.ContainsFunc(env, func(e string) bool { return strings.Contains(e, s) })
	}
	if env := identityEnv(nil); has(env, "GH_TOKEN") || has(env, "gh auth git-credential") {
		t.Errorf("default env carries GitHub identity: %v", env)
	}
	if env := identityEnv([]string{".aws"}); has(env, "GH_TOKEN") {
		t.Errorf("dir identity forwarded GH_TOKEN: %v", env)
	}
	if env := identityEnv([]string{"gh"}); !slices.Contains(env, "GH_TOKEN=tok") || !has(env, "gh auth git-credential") {
		t.Errorf("gh identity env = %v, want GH_TOKEN and the gh credential helper", env)
	}
}
