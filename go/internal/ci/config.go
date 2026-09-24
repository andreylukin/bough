package ci

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"path"
	"path/filepath"
	"regexp"
	"strings"

	"github.com/andreylukin/bough/internal/projectdef"
	"gopkg.in/yaml.v3"
)

// ConfigPath is where a repo defines its checks, relative to its root.
const ConfigPath = ".bough/ci.yml"

// Check is one named check.
type Check struct {
	Run string `yaml:"run" json:"run"` // shell command, run with sh -c
	Dir string `yaml:"dir,omitempty" json:"dir,omitempty"`
	// Inputs are repo-relative globs ("**" spans directories). A check
	// with inputs is keyed on only the files they match, so an edit
	// elsewhere reuses its last result. Each "/"-separated segment is
	// matched on its own, so unlike gitignore "*.go" is the root's .go
	// files only; "**/*.go" is every one.
	Inputs []string `yaml:"inputs,omitempty" json:"inputs,omitempty"`
	// GoCache marks a check keyed on the whole tree that leans on go's
	// own build and test cache for speed. It is spelled out (rather than
	// just omitting inputs) so the choice reads as deliberate.
	GoCache bool `yaml:"go_cache,omitempty" json:"go_cache,omitempty"`
	// Manual checks run only when named with --check.
	Manual bool `yaml:"manual,omitempty" json:"manual,omitempty"`
}

// Config is .bough/ci.yml.
type Config struct {
	Checks map[string]Check `yaml:"checks"`
}

// Check names become directory names under the results dir.
var validName = regexp.MustCompile(`^[A-Za-z0-9._-]+$`)

// Parse reads and validates a ci.yml. Unknown fields are errors: a
// misspelt `input:` would otherwise key the check on the whole tree and
// nothing would say why it keeps rerunning.
func Parse(b []byte) (Config, error) {
	var c Config
	dec := yaml.NewDecoder(bytes.NewReader(b))
	dec.KnownFields(true)
	// An empty file decodes as io.EOF; it falls through to "no checks
	// defined", which says more.
	if err := dec.Decode(&c); err != nil && !errors.Is(err, io.EOF) {
		return Config{}, fmt.Errorf("ci: %s: %w", ConfigPath, err)
	}
	if len(c.Checks) == 0 {
		return Config{}, fmt.Errorf("ci: %s: no checks defined", ConfigPath)
	}
	for name, ch := range c.Checks {
		ins, err := normInputs(name, ch.Inputs)
		if err != nil {
			return Config{}, fmt.Errorf("ci: %s: %w", ConfigPath, err)
		}
		ch.Inputs = ins
		if err := validate(name, ch); err != nil {
			return Config{}, fmt.Errorf("ci: %s: %w", ConfigPath, err)
		}
		c.Checks[name] = ch
	}
	return c, nil
}

// normInputs rewrites globs into the one spelling matchGlob compares
// segment by segment. "./web/**" and "web/" read as obvious, but a "."
// segment or an empty last one never matches a path: the check was
// keyed on nothing and its first result stood forever.
func normInputs(name string, globs []string) ([]string, error) {
	var out []string
	for _, g := range globs {
		orig := g
		if strings.HasSuffix(g, "/") {
			g += "**"
		}
		if g != "" && !strings.HasPrefix(g, "/") {
			g = path.Clean(g)
		}
		if g == "." || g == ".." || strings.HasPrefix(g, "../") {
			return nil, fmt.Errorf("check %q: input %q must name files inside the repository", name, orig)
		}
		out = append(out, g)
	}
	return out, nil
}

func validate(name string, ch Check) error {
	if name == "." || name == ".." || !validName.MatchString(name) {
		return fmt.Errorf("check %q: name must match %s", name, validName)
	}
	if strings.TrimSpace(ch.Run) == "" {
		return fmt.Errorf("check %q: run is empty", name)
	}
	if _, err := cleanDir(ch.Dir); err != nil {
		return fmt.Errorf("check %q: %w", name, err)
	}
	if len(ch.Inputs) > 0 && ch.GoCache {
		return fmt.Errorf("check %q: inputs and go_cache both set: go_cache keys on the whole tree, inputs on a subset; pick one", name)
	}
	for _, g := range ch.Inputs {
		if g == "" || strings.HasPrefix(g, "/") {
			return fmt.Errorf("check %q: input %q must be a repo-relative glob", name, g)
		}
		if _, err := path.Match(strings.ReplaceAll(g, "**", "x"), ""); err != nil {
			return fmt.Errorf("check %q: input %q: %w", name, g, err)
		}
	}
	return nil
}

// cleanDir checks a check's dir is relative and stays in the worktree.
func cleanDir(d string) (string, error) {
	if d == "" {
		return ".", nil
	}
	if path.IsAbs(d) || filepath.IsAbs(d) || strings.Contains(d, `\`) {
		return "", fmt.Errorf("dir %q must be a relative, /-separated path", d)
	}
	c := path.Clean(d)
	if c == ".." || strings.HasPrefix(c, "../") {
		return "", fmt.Errorf("dir %q escapes the repository", d)
	}
	return c, nil
}

// LoadConfig reads the checks for tree. The definition comes from the
// tree itself, not the working directory, so it is part of what is
// checked: --tree on an old checkpoint runs the checks that tree had.
// Without a ci.yml it falls back to the checks.fast/full of the project
// whose repo this is. The string says where the checks came from.
func LoadConfig(ctx context.Context, home string, r Repo, files *treeFiles) (Config, string, error) {
	if e, ok := files.find(ConfigPath); ok {
		b, err := gitRaw(ctx, r.Top, nil, "cat-file", "blob", e.oid)
		if err != nil {
			return Config{}, "", fmt.Errorf("ci: read %s from tree %s: %w", ConfigPath, short(files.tree), err)
		}
		c, err := Parse(b)
		return c, ConfigPath, err
	}
	if c, slug, ok := projectChecks(ctx, home, r); ok {
		return c, "project " + slug + " (project.yml checks)", nil
	}
	return Config{}, "", fmt.Errorf("ci: no %s in tree %s and no project with checks.fast for %s", ConfigPath, short(files.tree), r.Top)
}

// projectChecks finds the project one of whose repos is this repository,
// matched on the git common dir: a host checkout's is its own .git, and
// an orb worktree's is the source repo's, so both resolve. BOUGH_PROJECT
// is no help — the launcher unsets it for the agent's shell.
func projectChecks(ctx context.Context, home string, r Repo) (Config, string, bool) {
	projects, _ := projectdef.List(home) // a broken definition is not this call's problem
	for _, p := range projects {
		ch := p.Def.Checks
		if ch.Fast == "" {
			continue
		}
		for _, repo := range p.Def.Repos {
			src := projectdef.SourceGitDir(home, p.Slug, repo)
			common, err := git(ctx, src, nil, "rev-parse", "--path-format=absolute", "--git-common-dir")
			if err != nil {
				continue
			}
			if real, err := filepath.EvalSymlinks(common); err == nil {
				common = real
			}
			if common != r.CommonDir {
				continue
			}
			c := Config{Checks: map[string]Check{"fast": {Run: ch.Fast}}}
			if ch.Full != "" {
				// full is the slow one by name: only when asked for.
				c.Checks["full"] = Check{Run: ch.Full, Manual: true}
			}
			return c, p.Slug, true
		}
	}
	return Config{}, "", false
}
