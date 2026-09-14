package vtreal

import (
	"os"
	"os/exec"
	"path/filepath"
	"testing"
	"time"
)

// Only a project session registers tools.write/patch and takes turn
// checkpoints, so the edit and /undo scenarios run as project "vt" on
// the fake container runtime: commands run on the host, in the
// session's worktree under ~/.bough/orbs/<session>/repo.

const projectSlug = "vt"

// projectRows are the rows a project session mounts on top of a test
// config: the orb needs the scratchpad's dir.
const projectRows = "- id: scratchpad\n  plugin: scratchpad\n- id: orb\n  plugin: orb\n  config: {runtime: fake}\n"

// projectRepo makes $HOME/src/repo a git repo on branch main with files
// committed, and defines project vt on it. It returns the repo path.
func projectRepo(t *testing.T, home string, files map[string]string) string {
	t.Helper()
	repo := filepath.Join(home, "src", "repo")
	if err := os.MkdirAll(repo, 0o755); err != nil {
		t.Fatal(err)
	}
	for name, s := range files {
		if err := os.WriteFile(filepath.Join(repo, name), []byte(s), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	projectGit(t, repo, "init", "-q", "-b", "main")
	projectGit(t, repo, "add", "-A")
	projectGit(t, repo, "commit", "-q", "--allow-empty", "-m", "init")
	def := filepath.Join(home, ".bough", "projects", projectSlug)
	if err := os.MkdirAll(def, 0o755); err != nil {
		t.Fatal(err)
	}
	for name, s := range map[string]string{
		"project.yml": "repos:\n  - path: " + repo + "\n    branch: main\n",
		"setup.sh":    "true\n",
	} {
		if err := os.WriteFile(filepath.Join(def, name), []byte(s), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	return repo
}

func projectGit(t *testing.T, dir string, args ...string) {
	t.Helper()
	cmd := exec.Command("git", append([]string{"-C", dir, "-c", "user.name=t", "-c", "user.email=t@t"}, args...)...)
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("git %v: %v %s", args, err, out)
	}
}

// projectWorktree is session's worktree path; it need not exist yet.
func projectWorktree(home, session string) string {
	return filepath.Join(home, ".bough", "orbs", session, "repo")
}

// projectStart boots bough as a project session with yml plus the orb
// rows and sets a.wt to the worktree the orb opened.
func projectStart(t *testing.T, home string, cols, rows int, yml string) *app {
	t.Helper()
	a := undoStart(t, home, cols, rows, yml+projectRows, "--project", projectSlug)
	deadline := time.Now().Add(30 * time.Second)
	for {
		m, _ := filepath.Glob(projectWorktree(home, "*"))
		if len(m) == 1 {
			a.wt = m[0]
			return a
		}
		if time.Now().After(deadline) {
			t.Fatalf("no orb worktree (%v):\n%s", m, a.text())
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// projectMeta is a seeded session's meta data for project vt.
func projectMeta(cwd string) map[string]any {
	return map[string]any{"cwd": cwd, "mode": "project", "project": projectSlug}
}
