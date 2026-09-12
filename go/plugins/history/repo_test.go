package history

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/andreylukin/bough/kernel"
)

// gitRepo makes a checkout with one commit on a known branch.
func gitRepo(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	env := []string{"GIT_AUTHOR_NAME=t", "GIT_AUTHOR_EMAIL=t@t", "GIT_COMMITTER_NAME=t", "GIT_COMMITTER_EMAIL=t@t"}
	if _, err := git(dir, env, "init", "-q", "-b", "feat-x"); err != nil {
		t.Skipf("git unavailable: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dir, "f.txt"), []byte("x"), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := git(dir, env, "add", "f.txt"); err != nil {
		t.Fatal(err)
	}
	if _, err := git(dir, env, "commit", "-qm", "one"); err != nil {
		t.Fatal(err)
	}
	return dir
}

func TestRepoInfoReportsRootAndBranch(t *testing.T) {
	t.Parallel()
	dir := gitRepo(t)
	repo, branch := repoInfo(dir)
	// macOS hands out /var symlinked to /private/var, so compare by
	// resolved path rather than asserting the temp dir verbatim.
	want, _ := filepath.EvalSymlinks(dir)
	got, _ := filepath.EvalSymlinks(repo)
	if got != want {
		t.Errorf("repo = %q, want %q", repo, want)
	}
	if branch != "feat-x" {
		t.Errorf("branch = %q, want feat-x", branch)
	}
}

func TestRepoInfoOutsideRepoIsEmptyNotAnError(t *testing.T) {
	t.Parallel()
	// A home directory that merely holds repos is not itself one, and
	// that is the common case this must stay quiet about.
	repo, branch := repoInfo(t.TempDir())
	if repo != "" || branch != "" {
		t.Errorf("outside a repo = %q/%q, want empty", repo, branch)
	}
}

func TestListReadsRepoAndBranchFromMeta(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	writeSession(t, dir, "a", map[string]any{"cwd": "/w", "repo": "/r/bough", "branch": "main"}, [2]string{"input", "hi"})
	writeSession(t, dir, "b", map[string]any{"cwd": "/w"}, [2]string{"input", "hi"})

	infos, err := List(dir)
	if err != nil {
		t.Fatal(err)
	}
	by := map[string]SessionInfo{}
	for _, in := range infos {
		by[in.ID] = in
	}
	if by["a"].Repo != "/r/bough" || by["a"].Branch != "main" {
		t.Errorf("recorded session = %+v", by["a"])
	}
	if by["b"].Repo != "" || by["b"].Branch != "" {
		t.Errorf("a session predating capture must read back empty, got %+v", by["b"])
	}
}

// Not parallel: t.Chdir is incompatible with it, and Apply reads the
// process working directory to decide what to record.
func TestApplyRecordsRepoAndBranchOnMeta(t *testing.T) {
	repo := gitRepo(t)
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Chdir(repo)

	ctx := kernel.NewContext()
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	s, err := kernel.Get[*Store](ctx, "history")
	if err != nil {
		t.Fatal(err)
	}
	es := s.Entries()
	if len(es) != 1 || es[0].Kind != "meta" {
		t.Fatalf("entries = %+v, want one meta", es)
	}
	if es[0].Data["branch"] != "feat-x" {
		t.Errorf("meta branch = %v, want feat-x", es[0].Data["branch"])
	}
	if es[0].Data["repo"] == nil || es[0].Data["repo"] == "" {
		t.Errorf("meta repo missing: %+v", es[0].Data)
	}
	ctx.Unmount()
}
