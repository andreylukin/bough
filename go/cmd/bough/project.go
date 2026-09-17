// `bough project`: read and change project definitions in
// ~/.bough/projects/<slug>/ from a shell, so an agent can add a repo or
// set the checks without hand-editing yaml. Every write goes through
// projectdef.WriteFile, the same validation the web editor uses, so a
// typo fails here instead of at the next session start. In a project
// orb the guest's `bough` relays this command to the host.
package main

import (
	"errors"
	"flag"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"

	"github.com/andreylukin/bough/internal/projectdef"
	"gopkg.in/yaml.v3"
)

const projectUsage = `usage: bough project <command>
  list                                  every project: repos, image and orb state
  show <slug> [file]                    print project.yml, Dockerfile, setup.sh, resume.sh (or one)
  create <slug> <repo>...               new project; a repo is a local path (~ ok) or a git remote
  add-repo <slug> <repo> [--branch B] [--name N]
  remove-repo <slug> <name>
  add-identity <slug> <dir>             lend the container gh, or a $HOME config dir like .aws (read-only; <dir>:rw for read-write)
  remove-identity <slug> <dir>
  set <slug> <key> <value>              checks.fast, checks.full, base, memory, cpus, env.NAME,
                                        secrets.NAME keychain:<service>, ports 3000,8080:80
                                        (127.0.0.1 forwards; host:container); "" clears
  write <slug> <file>                   replace a file with stdin (empty stdin deletes a script)
  build <slug>                          build the project's image now (streams the log)
  status [slug|session]                 orbs and their state; one session's orb in detail
  logs <slug|session>                   a project's build.log, or a session's resume.log
  stop <session> [--yes]                stop a session's container (asks when its session runs)
  rm <session> [--branches] [--yes]     remove a session's orb: container, worktrees, orb dir
  prune [slug] [--branches] [--yes]     remove failed and archived sessions' orbs and unused images
                                        branches bough/<session> are kept; --branches deletes merged or pushed ones
Changes apply to the next session started in the project.`

func runProject(args []string) {
	if err := project(os.Stdout, os.Stdin, args); err != nil {
		fmt.Fprintln(os.Stderr, "bough: project:", err)
		os.Exit(1)
	}
}

func project(out io.Writer, in io.Reader, args []string) error {
	home, err := os.UserHomeDir()
	if err != nil {
		return err
	}
	if len(args) == 0 {
		return fmt.Errorf("%s", projectUsage)
	}
	if args[0] == "--help" || args[0] == "-h" || args[0] == "help" || args[0] == "-help" {
		fmt.Fprintln(out, projectUsage)
		return nil
	}
	need := func(n int) error {
		if len(args)-1 < n {
			return fmt.Errorf("%s", projectUsage)
		}
		return nil
	}
	switch args[0] {
	case "list":
		return projectList(out, home)
	case "build":
		return projectBuild(out, home, args[1:])
	case "status":
		return projectStatus(out, home, args[1:])
	case "logs":
		return projectLogs(out, home, args[1:])
	case "stop":
		return projectStop(out, in, home, args[1:])
	case "show":
		if err := need(1); err != nil {
			return err
		}
		// A missing project must fail, not print nothing: scripts test
		// `show` to decide whether to create one.
		if _, err := loadProject(home, args[1]); err != nil {
			return err
		}
		files := projectdef.EditableFiles
		if len(args) > 2 {
			files = args[2:3]
		}
		for _, f := range files {
			text, err := projectdef.ReadFile(home, args[1], f)
			if err != nil {
				return err
			}
			if text != "" {
				fmt.Fprintf(out, "== %s ==\n%s\n", f, strings.TrimRight(text, "\n"))
			}
		}
		return nil
	case "create":
		if len(args) == 2 {
			return fmt.Errorf("create needs a repo: bough project create %s <path or remote>...", args[1])
		}
		if err := need(2); err != nil {
			return err
		}
		if _, err := projectdef.Create(home, args[1]); errors.Is(err, fs.ErrExist) {
			return fmt.Errorf("project %q exists (bough project show %s)", args[1], args[1])
		} else if err != nil {
			return err
		}
		// The skeleton's example repo is a placeholder; the given repos replace it.
		err := mutate(out, home, args[1], func(d *projectdef.Def) error {
			d.Repos = nil
			for _, r := range args[2:] {
				d.Repos = append(d.Repos, repoArg(r))
			}
			return nil
		})
		if err != nil {
			// A refused create leaves nothing behind: the slug was just made here.
			os.RemoveAll(filepath.Join(projectdef.Root(home), args[1]))
		}
		return err
	case "add-repo":
		fs := flag.NewFlagSet("add-repo", flag.ContinueOnError)
		branch := fs.String("branch", "", "base branch")
		name := fs.String("name", "", "worktree dir name")
		if err := need(2); err != nil {
			return err
		}
		if err := fs.Parse(args[3:]); err != nil {
			return err
		}
		return mutate(out, home, args[1], func(d *projectdef.Def) error {
			r := repoArg(args[2])
			r.Branch, r.Name = *branch, *name
			d.Repos = append(d.Repos, r)
			return nil
		})
	case "add-identity", "remove-identity":
		if err := need(2); err != nil {
			return err
		}
		dir := strings.TrimPrefix(strings.TrimPrefix(args[2], "~/"), "$HOME/")
		return mutate(out, home, args[1], func(d *projectdef.Def) error {
			i := slices.Index(d.Identity, dir)
			switch {
			case args[0] == "add-identity" && i < 0:
				d.Identity = append(d.Identity, dir)
			case args[0] == "remove-identity" && i >= 0:
				d.Identity = slices.Delete(d.Identity, i, i+1)
			case args[0] == "remove-identity":
				return fmt.Errorf("no identity dir %q", dir)
			}
			return nil
		})
	case "remove-repo":
		if err := need(2); err != nil {
			return err
		}
		return mutate(out, home, args[1], func(d *projectdef.Def) error {
			for i, r := range d.Repos {
				if r.RepoName() == args[2] {
					d.Repos = append(d.Repos[:i], d.Repos[i+1:]...)
					return nil
				}
			}
			return fmt.Errorf("no repo named %q", args[2])
		})
	case "set":
		if err := need(3); err != nil {
			return err
		}
		return mutate(out, home, args[1], func(d *projectdef.Def) error { return setKey(d, args[2], args[3]) })
	case "rm":
		return projectRm(out, in, home, args[1:])
	case "prune":
		return projectPrune(out, in, home, args[1:])
	case "write":
		if err := need(2); err != nil {
			return err
		}
		b, err := io.ReadAll(in)
		if err != nil {
			return err
		}
		if err := projectdef.WriteFile(home, args[1], args[2], string(b)); err != nil {
			return err
		}
		fmt.Fprintf(out, "wrote %s/%s\n", args[1], args[2])
		return nil
	}
	return fmt.Errorf("unknown command %q\n%s", args[0], projectUsage)
}

// repoArg reads a repo the way a person types it: a remote has a scheme
// or git@, anything else is a host path.
func repoArg(s string) projectdef.Repo {
	if strings.Contains(s, "://") || strings.HasPrefix(s, "git@") {
		return projectdef.Repo{Remote: s}
	}
	return projectdef.Repo{Path: s}
}

func setKey(d *projectdef.Def, key, val string) error {
	switch key {
	case "checks.fast":
		d.Checks.Fast = val
	case "checks.full":
		d.Checks.Full = val
	case "base":
		d.Base = val
	case "memory":
		d.Memory = val
	case "cpus":
		n := 0
		if val != "" {
			var err error
			if n, err = strconv.Atoi(val); err != nil {
				return fmt.Errorf("cpus: %w", err)
			}
		}
		d.CPUs = n
	case "ports":
		ports, err := projectdef.ParsePorts(val)
		if err != nil {
			return fmt.Errorf("ports: %w", err)
		}
		d.Ports = ports
	default:
		if name, ok := strings.CutPrefix(key, "secrets."); ok && name != "" {
			if val == "" {
				delete(d.Secrets, name)
			} else {
				if d.Secrets == nil {
					d.Secrets = map[string]string{}
				}
				d.Secrets[name] = val
			}
			return nil
		}
		name, ok := strings.CutPrefix(key, "env.")
		if !ok || name == "" {
			return fmt.Errorf("unknown key %q (checks.fast, checks.full, base, memory, cpus, ports, env.NAME, secrets.NAME)", key)
		}
		if val == "" {
			delete(d.Env, name)
		} else {
			if d.Env == nil {
				d.Env = map[string]string{}
			}
			d.Env[name] = val
		}
	}
	return nil
}

// mutate rewrites project.yml from the parsed definition. Comments in the
// file do not survive; the fields do.
func mutate(out io.Writer, home, slug string, change func(*projectdef.Def) error) error {
	p, err := loadProject(home, slug)
	if err != nil {
		return err
	}
	if err := change(&p.Def); err != nil {
		return err
	}
	b, err := yaml.Marshal(p.Def)
	if err != nil {
		return err
	}
	if err := projectdef.WriteFile(home, slug, projectdef.FileYAML, string(b)); err != nil {
		return err
	}
	fmt.Fprintf(out, "== %s/%s ==\n%s", slug, projectdef.FileYAML, b)
	return nil
}
