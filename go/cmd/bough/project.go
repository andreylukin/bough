// `bough project`: read and change project definitions in
// ~/.bough/projects/<slug>/ from a shell, so an agent can add a repo or
// set the checks without hand-editing yaml. Every write goes through
// projectdef.WriteFile, the same validation the web editor uses, so a
// typo fails here instead of at the next session start. In a project
// orb the guest's `bough` relays this command to the host.
package main

import (
	"flag"
	"fmt"
	"io"
	"os"
	"strconv"
	"strings"

	"github.com/andreylukin/bough/internal/projectdef"
	"gopkg.in/yaml.v3"
)

const projectUsage = `usage: bough project <command>
  list                                  every project and its repos
  show <slug> [file]                    print project.yml, Dockerfile, setup.sh, resume.sh (or one)
  create <slug> <repo>...               new project; a repo is a local path (~ ok) or a git remote
  add-repo <slug> <repo> [--branch B] [--name N]
  remove-repo <slug> <name>
  set <slug> <key> <value>              checks.fast, checks.full, base, memory, cpus, env.NAME; "" clears
  write <slug> <file>                   replace a file with stdin (empty stdin deletes a script)
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
	need := func(n int) error {
		if len(args)-1 < n {
			return fmt.Errorf("%s", projectUsage)
		}
		return nil
	}
	switch args[0] {
	case "list":
		ps, err := projectdef.List(home)
		for _, p := range ps {
			names := make([]string, len(p.Def.Repos))
			for i, r := range p.Def.Repos {
				names[i] = r.RepoName()
			}
			fmt.Fprintf(out, "%s\t%s\n", p.Slug, strings.Join(names, ", "))
		}
		return err
	case "show":
		if err := need(1); err != nil {
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
		if err := need(2); err != nil {
			return err
		}
		if _, err := projectdef.Create(home, args[1]); err != nil {
			return err
		}
		// The skeleton's example repo is a placeholder; the given repos replace it.
		return mutate(out, home, args[1], func(d *projectdef.Def) error {
			d.Repos = nil
			for _, r := range args[2:] {
				d.Repos = append(d.Repos, repoArg(r))
			}
			return nil
		})
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
	default:
		name, ok := strings.CutPrefix(key, "env.")
		if !ok || name == "" {
			return fmt.Errorf("unknown key %q (checks.fast, checks.full, base, memory, cpus, env.NAME)", key)
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
	p, err := projectdef.Load(home, slug)
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
