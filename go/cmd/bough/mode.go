package main

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/testhold"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
)

// modeInputs is everything that can choose a session's mode, gathered
// so the precedence lives in one pure function.
type modeInputs struct {
	FlagProject string // --project <slug>
	FlagLocal   bool   // --local
	EnvMode     string // $BOUGH_MODE
	EnvProject  string // $BOUGH_PROJECT
	// Resumed is true when a session file is being resumed; its meta
	// then decides and flags/env only earn a notice.
	Resumed     bool
	MetaMode    string
	MetaProject string
}

// resolveMode applies: resume meta > flags > env > local. err is a
// usage error (exit 2); notice is a line for stderr.
func resolveMode(in modeInputs) (mode, project, notice string, err error) {
	if in.FlagProject != "" && in.FlagLocal {
		return "", "", "", fmt.Errorf("use either --project or --local, not both")
	}
	if in.Resumed {
		mode, project = in.MetaMode, in.MetaProject
		if mode != "project" {
			mode, project = "local", ""
		}
		if (in.FlagProject != "" && in.FlagProject != project) || (in.FlagLocal && mode != "local") {
			notice = fmt.Sprintf("resumed session is %s; ignoring the mode flag", describeMode(mode, project))
		}
		return mode, project, notice, nil
	}
	switch {
	case in.FlagProject != "":
		return "project", in.FlagProject, "", nil
	case in.FlagLocal:
		return "local", "", "", nil
	}
	switch in.EnvMode {
	case "", "local":
		if in.EnvMode == "" && in.EnvProject != "" {
			return "project", in.EnvProject, "", nil
		}
		return "local", "", "", nil
	case "project":
		if in.EnvProject == "" {
			return "", "", "", fmt.Errorf("BOUGH_MODE=project needs BOUGH_PROJECT=<slug>")
		}
		return "project", in.EnvProject, "", nil
	}
	return "", "", "", fmt.Errorf("BOUGH_MODE=%q: want local or project", in.EnvMode)
}

func describeMode(mode, project string) string {
	if mode == "project" {
		return "project " + project
	}
	return "local"
}

// takeModeEnv reads and clears BOUGH_MODE/BOUGH_PROJECT: like
// BOUGH_ORIGIN, a bough the agent runs from its own shell must not
// inherit this session's mode.
func takeModeEnv() (mode, project string) {
	mode, project = os.Getenv("BOUGH_MODE"), os.Getenv("BOUGH_PROJECT")
	os.Unsetenv("BOUGH_MODE")
	os.Unsetenv("BOUGH_PROJECT")
	return mode, project
}

// takeProjectDirEnv reads and clears BOUGH_PROJECT_DIR: the project
// directory serve sets on a LOCAL session that is assigned to a
// project, so context-md prepends that project's MEMORY.md and tools
// may write it. It is a directory, not a slug, and never chooses a
// mode: BOUGH_MODE/BOUGH_PROJECT alone do that. Cleared like the rest
// of the session env so a `bough` the agent runs from tools.bash does
// not inherit this session's project.
func takeProjectDirEnv() string {
	dir := os.Getenv("BOUGH_PROJECT_DIR")
	os.Unsetenv("BOUGH_PROJECT_DIR")
	return dir
}

// takeMainEnv reads and clears BOUGH_PROJECT_MAIN: serve sets it on the
// one long-lived session that is a project's main thread, so the prompt
// can tell it what it is. Cleared like the rest of the session env, or
// a `bough` the agent runs from its own shell would claim to be the
// main thread too.
func takeMainEnv() bool {
	main := os.Getenv("BOUGH_PROJECT_MAIN") != ""
	os.Unsetenv("BOUGH_PROJECT_MAIN")
	return main
}

// takeThreadEnv reads and clears BOUGH_PROJECT_THREAD: serve sets it on
// a thread a person started from the project page, which has main as its
// parent for notices but is not a background agent, so it may delegate.
// Cleared for the same reason as BOUGH_PROJECT_MAIN.
func takeThreadEnv() bool {
	thread := os.Getenv("BOUGH_PROJECT_THREAD") != ""
	os.Unsetenv("BOUGH_PROJECT_THREAD")
	return thread
}

// takeSessionEnv reads and clears BOUGH_SPAWNED_BY/BOUGH_SESSION_ID,
// which serve sets on a background agent: left in the environment, a
// `bough -p` the agent runs through tools.bash would nest under the
// wrong parent or try to create the agent's own history file.
func takeSessionEnv() (spawnedBy, sessionID string) {
	spawnedBy, sessionID = os.Getenv("BOUGH_SPAWNED_BY"), os.Getenv("BOUGH_SESSION_ID")
	os.Unsetenv("BOUGH_SPAWNED_BY")
	os.Unsetenv("BOUGH_SESSION_ID")
	return spawnedBy, sessionID
}

// sessionFile is the history file this run resumes ("" = fresh): the
// last history.file override, which is where -c/-r land too.
func sessionFile(sets setFlags) string {
	file := ""
	for _, s := range sets {
		if v, ok := strings.CutPrefix(s, "history.file="); ok {
			file = v
		}
	}
	return file
}

// defaultWriteRoot is the git checkout a session started in, or "".
// Read-only as the only default made the first thing anyone tries ("fix
// the failing tests") impossible: the model had no tools.patch. A
// checkout that holds home (a dotfiles repo at ~) is not a project, so
// it stays read-only.
func defaultWriteRoot(cwd, home string) string { return iorb.CheckoutRoot(cwd, home) }

// applyDefaultWriteRoot makes a local session's git checkout writable
// when nothing set BOUGH_WRITE_ROOTS, and reports whether the launcher
// owns the roots (so /new may move them). It goes through the
// environment because a `bough -p` the agent runs from tools.bash should
// inherit the same boundary.
func applyDefaultWriteRoot(mode string) bool {
	if mode != "local" || os.Getenv(iorb.WriteRootsEnv) != "" {
		return false
	}
	cwd, err := os.Getwd()
	if err != nil {
		return false
	}
	home, _ := os.UserHomeDir()
	if root := defaultWriteRoot(cwd, home); root != "" {
		os.Setenv(iorb.WriteRootsEnv, root)
	}
	return true
}

// moveWriteRoot re-derives the write root from the directory /new moved
// the session to. Decided only at launch, a session started in ~ stayed
// read-only after "/new ~/repos/x". Only a changed root is provided: the
// Provide remounts tools, and with it everything that reads turn-stats.
func moveWriteRoot(ctx *kernel.Context) {
	cwd, err := os.Getwd()
	if err != nil {
		return
	}
	home, _ := os.UserHomeDir()
	root := defaultWriteRoot(cwd, home)
	if root == os.Getenv(iorb.WriteRootsEnv) {
		return
	}
	os.Setenv(iorb.WriteRootsEnv, root)
	ctx.Provide(iorb.WriteRootsKey, iorb.LocalWriteRoots())
}

// chooseMode resolves the session's mode for main and validates a
// project slug before any row mounts.
func chooseMode(flagProject string, flagLocal bool, sets setFlags) (mode, project string, err error) {
	in := modeInputs{FlagProject: flagProject, FlagLocal: flagLocal}
	in.EnvMode, in.EnvProject = takeModeEnv()
	if file := sessionFile(sets); file != "" {
		// A missing file is a fresh session under that name (the history
		// row creates it), so the flags still decide.
		if es, rerr := history.Read(file); rerr == nil {
			in.Resumed = true
			for _, e := range es {
				if e.Kind == "meta" {
					in.MetaMode, _ = e.Data["mode"].(string)
					in.MetaProject, _ = e.Data["project"].(string)
					break
				}
			}
		}
	}
	mode, project, notice, err := resolveMode(in)
	if err != nil {
		return "", "", err
	}
	if notice != "" {
		fmt.Fprintln(os.Stderr, "bough: "+notice)
	}
	if mode == "project" {
		home, herr := os.UserHomeDir()
		if herr != nil {
			return "", "", fmt.Errorf("project %s: home dir: %w", project, herr)
		}
		// Model tests hold a spawned child here, before it reads project.yml.
		testhold.At("boot." + project)
		if _, lerr := projectdef.Load(home, project); lerr != nil {
			return "", "", fmt.Errorf("project %s: %w", project, lerr)
		}
	}
	return mode, project, nil
}

// projectDirFor is the session's project directory, and only in local
// mode: a project session gets the same directory from its slug, and an
// inherited BOUGH_PROJECT_DIR must not make a project session read
// another project's MEMORY.md.
func projectDirFor(mode, dir string) string {
	if mode != "local" || dir == "" {
		return ""
	}
	return filepath.Clean(dir)
}
