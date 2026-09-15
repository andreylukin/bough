package main

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
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
func defaultWriteRoot(cwd, home string) string {
	for dir := filepath.Clean(cwd); ; dir = filepath.Dir(dir) {
		if _, err := os.Stat(filepath.Join(dir, ".git")); err == nil {
			if home != "" {
				if rel, err := filepath.Rel(dir, home); err == nil && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
					return ""
				}
			}
			return dir
		}
		if filepath.Dir(dir) == dir {
			return ""
		}
	}
}

// applyDefaultWriteRoot makes a local session's git checkout writable
// when nothing set BOUGH_WRITE_ROOTS. It goes through the environment
// because tools reads the roots there, and a `bough -p` the agent runs
// from tools.bash should inherit the same boundary.
func applyDefaultWriteRoot(mode string) {
	if mode != "local" || os.Getenv(iorb.WriteRootsEnv) != "" {
		return
	}
	cwd, err := os.Getwd()
	if err != nil {
		return
	}
	home, _ := os.UserHomeDir()
	if root := defaultWriteRoot(cwd, home); root != "" {
		os.Setenv(iorb.WriteRootsEnv, root)
	}
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
		if _, lerr := projectdef.Load(home, project); lerr != nil {
			return "", "", fmt.Errorf("project %s: %w", project, lerr)
		}
	}
	return mode, project, nil
}
