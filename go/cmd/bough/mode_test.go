package main

import (
	"os"
	"path/filepath"
	"testing"
)

// A local session started in a checkout writes there; anywhere else,
// and in a checkout that holds home, it stays read-only.
func TestDefaultWriteRoot(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	home := filepath.Join(base, "home")
	repo := filepath.Join(home, "src", "app")
	wt := filepath.Join(home, "src", "wt")
	dots := filepath.Join(base, "dots")
	for _, d := range []string{filepath.Join(repo, ".git"), filepath.Join(repo, "pkg", "x"), wt, filepath.Join(home, "plain"), filepath.Join(dots, ".git"), filepath.Join(dots, "proj")} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	// A worktree's .git is a file, not a directory.
	if err := os.WriteFile(filepath.Join(wt, ".git"), []byte("gitdir: elsewhere\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct{ name, cwd, home, want string }{
		{"subdir of a checkout", filepath.Join(repo, "pkg", "x"), home, repo},
		{"checkout root", repo, home, repo},
		{"worktree", wt, home, wt},
		{"not a checkout", filepath.Join(home, "plain"), home, ""},
		{"dotfiles repo at home", filepath.Join(dots, "proj"), dots, ""},
	} {
		if got := defaultWriteRoot(tc.cwd, tc.home); got != tc.want {
			t.Errorf("%s: defaultWriteRoot = %q, want %q", tc.name, got, tc.want)
		}
	}
}

func TestResolveMode(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name          string
		in            modeInputs
		mode, project string
		notice, err   bool
	}{
		{name: "default local", in: modeInputs{}, mode: "local"},
		{name: "flag project", in: modeInputs{FlagProject: "web"}, mode: "project", project: "web"},
		{name: "flag local", in: modeInputs{FlagLocal: true, EnvMode: "project", EnvProject: "web"}, mode: "local"},
		{name: "both flags", in: modeInputs{FlagProject: "web", FlagLocal: true}, err: true},
		{name: "env project", in: modeInputs{EnvMode: "project", EnvProject: "api"}, mode: "project", project: "api"},
		{name: "flag beats env", in: modeInputs{FlagProject: "web", EnvMode: "project", EnvProject: "api"}, mode: "project", project: "web"},
		{name: "env project without slug", in: modeInputs{EnvMode: "project"}, err: true},
		{name: "env bad mode", in: modeInputs{EnvMode: "cloud"}, err: true},
		{name: "env local", in: modeInputs{EnvMode: "local"}, mode: "local"},
		{name: "resume old meta", in: modeInputs{Resumed: true}, mode: "local"},
		{name: "resume project meta beats env", in: modeInputs{Resumed: true, MetaMode: "project", MetaProject: "web", EnvMode: "local"}, mode: "project", project: "web"},
		{name: "resume disagreeing flag", in: modeInputs{Resumed: true, MetaMode: "local", FlagProject: "web"}, mode: "local", notice: true},
		{name: "resume agreeing flag", in: modeInputs{Resumed: true, MetaMode: "project", MetaProject: "web", FlagProject: "web"}, mode: "project", project: "web"},
	} {
		mode, project, notice, err := resolveMode(tc.in)
		if (err != nil) != tc.err {
			t.Errorf("%s: err = %v", tc.name, err)
			continue
		}
		if mode != tc.mode || project != tc.project || (notice != "") != tc.notice {
			t.Errorf("%s: got %q %q notice=%q", tc.name, mode, project, notice)
		}
	}
}

func TestSessionFileTakesLastOverride(t *testing.T) {
	t.Parallel()
	if got := sessionFile(setFlags{"history.file=/a", "llm.model=x", "history.file=/b"}); got != "/b" {
		t.Errorf("sessionFile = %q", got)
	}
}
