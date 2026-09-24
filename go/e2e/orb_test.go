package e2e

import (
	"os"
	"os/exec"
	"path/filepath"
	"testing"
)

// A default session is local: the prompt carries the read-only section
// and the catalogue has no tools.write.
func TestHeadlessLocalSessionIsReadOnly(t *testing.T) {
	t.Parallel()
	out := runHeadless(t, launchOpts{}, "SYSTEM!")
	mustContain(t, out, "Session mode: local")
	mustNotContain(t, out, "tools.write(path")
}

// Started inside a git checkout, a local session can write there: the
// prompt names the checkout and the catalogue has tools.write.
func TestHeadlessLocalSessionInCheckoutWrites(t *testing.T) {
	t.Parallel()
	out := runHeadless(t, launchOpts{cwd: map[string]string{".git/HEAD": "ref: refs/heads/main\n"}}, "SYSTEM!")
	mustContain(t, out, "Session mode: local, with write access to", "tools.write(path")
}

// --project with the fake runtime mounts the orb row and routes
// tools.bash through its exec seam.
func TestHeadlessProjectSessionExecsThroughOrb(t *testing.T) {
	t.Parallel()
	// A plain first run makes the sandbox HOME, where the project's
	// repo and definition then go.
	b := launchHeadless(t, launchOpts{})
	b.closeStdin()
	b.waitExit()

	home := b.home
	repo := filepath.Join(home, "src", "app")
	if err := os.MkdirAll(repo, 0o755); err != nil {
		t.Fatal(err)
	}
	for _, args := range [][]string{{"init", "-q", "-b", "main"}, {"commit", "-q", "--allow-empty", "-m", "init"}} {
		c := exec.Command("git", args...)
		c.Dir = repo
		c.Env = append(env(home), "GIT_AUTHOR_NAME=t", "GIT_AUTHOR_EMAIL=t@t", "GIT_COMMITTER_NAME=t", "GIT_COMMITTER_EMAIL=t@t")
		if out, err := c.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v\n%s", args, err, out)
		}
	}
	yml := "repos:\n  - path: " + repo + "\n    branch: main\n"
	p := launchHeadless(t, launchOpts{from: b, sets: []string{"orb.runtime=fake"}, args: []string{"--project", "demo"},
		home: map[string]string{".bough/projects/demo/project.yml": yml, ".bough/projects/demo/setup.sh": "true\n"}})
	// One turn at a time: a line sent while SYSTEM!'s turn runs steers
	// it, and the steered turn drops the prompt reply as stale, so the
	// section went missing whenever CODE! beat that turn's first block.
	p.send("SYSTEM!")
	p.waitFor("[done]")
	p.send("CODE!")
	p.closeStdin()
	if code := p.waitExit(); code != 0 {
		t.Fatalf("exit code %d; output:\n%s", code, p.out.String())
	}
	out := p.out.String()
	mustContain(t, out, "Project session: demo", "hi from codemode")
	mustNotContain(t, out, "Session mode: local", "orb not ready")
}
