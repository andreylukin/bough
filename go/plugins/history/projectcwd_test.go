package history

import (
	"os"
	"path/filepath"
	"testing"
)

// A project session's child starts in $HOME and only chdirs into its
// primary worktree once the orb opens, after meta recorded cwd. Serve
// reads Cwd for Edits and diff, so the listing reports the worktree.
// Not parallel: List reads $HOME.
func TestListProjectSessionCwdIsPrimaryWorktree(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	dir := t.TempDir()
	primary := filepath.Join(home, ".bough", "orbs", "p1", "app")
	writeSession(t, dir, "p1", map[string]any{"cwd": home, "mode": "project", "project": "app"}, [2]string{"input", "hi"})
	writeSession(t, dir, "p2", map[string]any{"cwd": home, "mode": "project", "project": "app"}, [2]string{"input", "hi"})
	writeSession(t, dir, "l1", map[string]any{"cwd": "/w"}, [2]string{"input", "hi"})

	by := func() map[string]SessionInfo {
		infos, err := List(dir)
		if err != nil {
			t.Fatal(err)
		}
		m := map[string]SessionInfo{}
		for _, in := range infos {
			m[in.ID] = in
		}
		return m
	}
	// Before the orb opened there is no worktree to name.
	if got := by()["p1"].Cwd; got != home {
		t.Errorf("before state.json: cwd = %q, want %q", got, home)
	}
	if err := os.MkdirAll(primary, 0o755); err != nil {
		t.Fatal(err)
	}
	state := `{"session":"p1","project":"app","status":"running","primary":"` + primary + `"}`
	if err := os.WriteFile(filepath.Join(home, ".bough", "orbs", "p1", "state.json"), []byte(state), 0o644); err != nil {
		t.Fatal(err)
	}
	m := by() // the transcript is unchanged: its cached row must still pick this up
	if m["p1"].Cwd != primary {
		t.Errorf("project cwd = %q, want primary worktree %q", m["p1"].Cwd, primary)
	}
	if m["p2"].Cwd != home || m["l1"].Cwd != "/w" {
		t.Errorf("others changed: p2=%q l1=%q", m["p2"].Cwd, m["l1"].Cwd)
	}
}

// A project session whose orb failed never left $HOME. Its checkpoints
// must not snapshot whatever repo that happens to be (a dotfiles
// checkout at ~). Not parallel: t.Chdir.
func TestProjectCheckpointsSkipHome(t *testing.T) {
	repo := gitRepo(t)
	t.Chdir(repo)
	c := &Checkpoints{session: "p1", home: repo}
	if tree := c.Snapshot(); tree != "" {
		t.Errorf("snapshot of a project session's home = %q, want none", tree)
	}
	c.home = t.TempDir()
	if tree := c.Snapshot(); tree == "" {
		t.Error("a project session's checkpoint in its worktree was skipped")
	}
}
