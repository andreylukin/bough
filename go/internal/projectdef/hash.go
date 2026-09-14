package projectdef

import (
	"crypto/sha256"
	_ "embed"
	"encoding/hex"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path"
	"path/filepath"
	"regexp"
	"strings"
)

// BaseDockerfile is bough's orb base: the user's everyday CLIs. A project
// with no `base` builds its setup.sh on top of it.
//
//go:embed base.Dockerfile
var BaseDockerfile []byte

// BaseTag names the base image by its Dockerfile, so editing it rebuilds
// the base and, through ImageHash, every project built on it.
func BaseTag() string {
	sum := sha256.Sum256(BaseDockerfile)
	return "bough-orb/base:" + hex.EncodeToString(sum[:])[:12]
}

func ImageTag(slug, hash string) string { return "bough-orb/" + slug + ":" + hash }

// CacheGitDir is the bare clone a Remote repo's worktrees are added from.
func CacheGitDir(home, slug string, r Repo) string {
	return filepath.Join(home, ".bough", "orbs", "cache", slug, r.RepoName()+".git")
}

// SourceGitDir is the git dir lockfiles and worktrees come from: the
// user's checkout for Path repos, the cache clone for Remote repos.
func SourceGitDir(home, slug string, r Repo) string {
	if r.Path != "" {
		return ExpandPath(home, r.Path)
	}
	return CacheGitDir(home, slug, r)
}

// BaseRef is the commit-ish lockfiles and new worktrees start from.
func (r Repo) BaseRef() string {
	if r.Branch == "" {
		return "HEAD"
	}
	return r.Branch
}

// ImageHash fingerprints the build inputs of the snapshot image, and only
// those (docs/orb-speed.md §1): the base; on the setup.sh path the script
// and the lockfiles its steps declare; on the Dockerfile path every project
// dir file except project.yml and resume.sh. Declared lockfiles are read
// with `git show <ref>:<file>` so uncommitted edits in the user's checkout,
// and worktree changes by agents, never trigger a rebuild. A remote repo
// not cloned yet contributes nothing; orb.Open clones before hashing so
// the tag it builds is complete.
func ImageHash(home string, p Project) (string, error) {
	h := sha256.New()
	field := func(label string, b []byte) {
		fmt.Fprintf(h, "%s\x00%d\x00", label, len(b))
		h.Write(b)
	}
	if p.UsesDockerfile() {
		// The project dir is the build context: any file the Dockerfile
		// COPYs is an input. project.yml and resume.sh are only when the
		// Dockerfile names them.
		df, _ := os.ReadFile(filepath.Join(p.Dir, FileDockerfile))
		if err := hashTree(p.Dir, string(df), field); err != nil {
			return "", fmt.Errorf("projectdef: hash %s: %w", p.Slug, err)
		}
	} else {
		b, err := os.ReadFile(filepath.Join(p.Dir, FileSetup))
		if err != nil && !os.IsNotExist(err) {
			return "", fmt.Errorf("projectdef: hash %s: %w", p.Slug, err)
		}
		field(FileSetup, b)
		steps, err := ParseSteps(string(b))
		if err != nil {
			return "", fmt.Errorf("projectdef: hash %s: %w", p.Slug, err)
		}
		for _, s := range steps {
			lfs, err := StepLockfiles(home, p, s.Uses)
			if err != nil {
				return "", fmt.Errorf("projectdef: hash %s: %w", p.Slug, err)
			}
			for _, lf := range lfs {
				field("lock:"+lf.Rel(), lf.Data)
			}
		}
	}
	base := p.Def.Base
	if base == "" {
		base = BaseTag()
	}
	field("base", []byte(base))
	return hex.EncodeToString(h.Sum(nil))[:12], nil
}

// Step is one image layer of a setup.sh build.
type Step struct {
	Name   string   // `# bough:step <name>`; "setup" for a script without markers
	Script string   // the preamble followed by the step's own lines
	Uses   []string // repo-relative files from `# bough:uses`
}

var stepNameRE = regexp.MustCompile(`^[a-z0-9][a-z0-9._-]{0,62}$`)

// ParseSteps splits setup.sh on `# bough:step <name>` markers. Text before
// the first marker (shebang, set -e) is the preamble, prepended to every
// step; no markers is one step. `# bough:uses <file>...` inside a step
// declares repo files the step reads, as /bough-setup/lock/<repo>/<file>.
func ParseSteps(text string) ([]Step, error) {
	var preamble strings.Builder
	var steps []Step
	var body strings.Builder
	flush := func() {
		if len(steps) > 0 {
			steps[len(steps)-1].Script = preamble.String() + body.String()
		}
		body.Reset()
	}
	for _, line := range strings.SplitAfter(text, "\n") {
		t := strings.TrimSpace(line)
		if name, ok := strings.CutPrefix(t, "# bough:step"); ok {
			name = strings.TrimSpace(name)
			if !stepNameRE.MatchString(name) {
				return nil, fmt.Errorf("%s: bad step name %q (want [a-z0-9][a-z0-9._-]*)", FileSetup, name)
			}
			for _, s := range steps {
				if s.Name == name {
					return nil, fmt.Errorf("%s: duplicate step %q", FileSetup, name)
				}
			}
			flush()
			steps = append(steps, Step{Name: name})
			body.WriteString(line)
			continue
		}
		if rest, ok := strings.CutPrefix(t, "# bough:uses"); ok {
			if len(steps) == 0 {
				return nil, fmt.Errorf("%s: bough:uses before the first bough:step", FileSetup)
			}
			files := strings.Fields(rest)
			if len(files) == 0 {
				return nil, fmt.Errorf("%s: step %s: bough:uses names no file", FileSetup, steps[len(steps)-1].Name)
			}
			for _, f := range files {
				if path.IsAbs(f) || path.Clean(f) != f || f == "." || f == ".." || strings.HasPrefix(f, "../") {
					return nil, fmt.Errorf("%s: step %s: bough:uses %q: want a clean repo-relative path", FileSetup, steps[len(steps)-1].Name, f)
				}
			}
			steps[len(steps)-1].Uses = append(steps[len(steps)-1].Uses, files...)
		}
		if len(steps) == 0 {
			preamble.WriteString(line)
		} else {
			body.WriteString(line)
		}
	}
	if len(steps) == 0 {
		return []Step{{Name: "setup", Script: text}}, nil
	}
	flush()
	return steps, nil
}

// Lockfile is one declared file at its repo's base ref.
type Lockfile struct {
	Repo string // RepoName
	Path string // repo-relative, slash-separated
	Data []byte
}

// Rel is the file's place under /bough-setup/lock/.
func (l Lockfile) Rel() string { return l.Repo + "/" + l.Path }

// StepLockfiles resolves a step's `bough:uses` files in every repo that
// tracks them at its base ref. A file no repo tracks is an error, unless
// a remote repo is not cloned yet (it may be there).
func StepLockfiles(home string, p Project, uses []string) ([]Lockfile, error) {
	var out []Lockfile
	for _, f := range uses {
		found, unknown := false, false
		for _, r := range p.Def.Repos {
			gd := SourceGitDir(home, p.Slug, r)
			if _, err := os.Stat(gd); err != nil {
				unknown = true
				continue
			}
			b, err := exec.Command("git", "-C", gd, "show", r.BaseRef()+":"+f).Output()
			if err != nil {
				continue
			}
			found = true
			out = append(out, Lockfile{Repo: r.RepoName(), Path: f, Data: b})
		}
		if !found && !unknown {
			return nil, fmt.Errorf("%s: bough:uses %s: no repo tracks it at its base ref", FileSetup, f)
		}
	}
	return out, nil
}

// hashTree feeds every regular file under dir to field, by relative
// path in walk (lexical) order, skipping project.yml and resume.sh unless
// the Dockerfile text mentions them (a COPY would bake them in).
func hashTree(dir, dockerfile string, field func(string, []byte)) error {
	return filepath.WalkDir(dir, func(p string, d fs.DirEntry, err error) error {
		if err != nil || !d.Type().IsRegular() {
			return err
		}
		rel, _ := filepath.Rel(dir, p)
		if (rel == FileYAML || rel == FileResume) && !strings.Contains(dockerfile, rel) {
			return nil
		}
		b, err := os.ReadFile(p)
		if err != nil {
			return err
		}
		field("ctx:"+filepath.ToSlash(rel), b)
		return nil
	})
}
