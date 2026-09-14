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
	"slices"
	"strings"

	"gopkg.in/yaml.v3"
)

var LockfileNames = []string{"go.sum", "package-lock.json", "bun.lock", "bun.lockb", "yarn.lock", "pnpm-lock.yaml", "Cargo.lock", "poetry.lock", "uv.lock", "requirements.txt", "Gemfile.lock"}

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

// ImageHash fingerprints every input of the snapshot image. Lockfiles are
// read with `git show <ref>:<file>` so uncommitted edits in the user's
// checkout, and worktree changes by agents, never trigger a rebuild.
// A remote repo not cloned yet contributes nothing; orb.Open clones before
// hashing so the tag it builds is complete.
func ImageHash(home string, p Project) (string, error) {
	h := sha256.New()
	field := func(label string, b []byte) {
		fmt.Fprintf(h, "%s\x00%d\x00", label, len(b))
		h.Write(b)
	}
	if p.UsesDockerfile() {
		// The whole project dir is the build context: any file the
		// Dockerfile COPYs (a setup script, a config) is an input, and
		// project.yml is one of them.
		if err := hashTree(p.Dir, field); err != nil {
			return "", fmt.Errorf("projectdef: hash %s: %w", p.Slug, err)
		}
	} else {
		b, err := os.ReadFile(filepath.Join(p.Dir, FileSetup))
		if err != nil && !os.IsNotExist(err) {
			return "", fmt.Errorf("projectdef: hash %s: %w", p.Slug, err)
		}
		field(FileSetup, b)
		y, err := os.ReadFile(filepath.Join(p.Dir, FileYAML))
		if err != nil {
			return "", fmt.Errorf("projectdef: hash %s: %w", p.Slug, err)
		}
		field(FileYAML, withoutSecrets(y))
	}
	base := p.Def.Base
	if base == "" {
		base = BaseTag()
	}
	field("base", []byte(base))
	for _, r := range p.Def.Repos {
		gd := SourceGitDir(home, p.Slug, r)
		if _, err := os.Stat(gd); err != nil {
			continue
		}
		for _, lf := range repoLockfiles(gd, r.BaseRef()) {
			out, err := exec.Command("git", "-C", gd, "show", r.BaseRef()+":"+lf).Output()
			if err != nil {
				continue
			}
			field("lock:"+r.RepoName()+"/"+lf, out)
		}
	}
	return hex.EncodeToString(h.Sum(nil))[:12], nil
}

// Lockfiles writes each repo's lockfiles at its base ref into dir as
// <repo>/<path in repo> and returns the written paths, for CommitSpec.Files.
func Lockfiles(home string, p Project, dir string) ([]string, error) {
	var paths []string
	for _, r := range p.Def.Repos {
		gd := SourceGitDir(home, p.Slug, r)
		for _, lf := range repoLockfiles(gd, r.BaseRef()) {
			out, err := exec.Command("git", "-C", gd, "show", r.BaseRef()+":"+lf).Output()
			if err != nil {
				continue
			}
			dst := filepath.Join(dir, r.RepoName(), filepath.FromSlash(lf))
			if err := os.MkdirAll(filepath.Dir(dst), 0o755); err != nil {
				return nil, fmt.Errorf("projectdef: lockfiles: %w", err)
			}
			if err := os.WriteFile(dst, out, 0o644); err != nil {
				return nil, fmt.Errorf("projectdef: lockfiles: %w", err)
			}
			paths = append(paths, dst)
		}
	}
	return paths, nil
}

// repoLockfiles lists the tracked files at ref named like a lockfile, at
// any depth: a monorepo keeps web/bun.lock, bough keeps go/go.sum. The
// list is sorted by git, so the hash is stable.
func repoLockfiles(gitDir, ref string) []string {
	out, err := exec.Command("git", "-C", gitDir, "ls-tree", "-r", "--name-only", ref).Output()
	if err != nil {
		return nil // no such ref (empty repo, unfetched branch): nothing to hash
	}
	var files []string
	for _, f := range strings.Split(strings.TrimSpace(string(out)), "\n") {
		if f != "" && slices.Contains(LockfileNames, path.Base(f)) {
			files = append(files, f)
		}
	}
	return files
}

// withoutSecrets is project.yml with the secrets key removed, so adding a
// secret never rebuilds the image. It is always re-marshaled, so a file
// with and without secrets hashes alike; an unparseable one hashes as is.
func withoutSecrets(y []byte) []byte {
	d, err := Parse(y)
	if err != nil {
		return y
	}
	// Neither secrets nor identity mounts are part of the image.
	d.Secrets, d.Identity = nil, nil
	b, err := yaml.Marshal(d)
	if err != nil {
		return y
	}
	return b
}

// hashTree feeds every regular file under dir to field, by relative
// path in walk (lexical) order.
func hashTree(dir string, field func(string, []byte)) error {
	return filepath.WalkDir(dir, func(p string, d fs.DirEntry, err error) error {
		if err != nil || !d.Type().IsRegular() {
			return err
		}
		b, err := os.ReadFile(p)
		if err != nil {
			return err
		}
		rel, _ := filepath.Rel(dir, p)
		if rel == FileYAML {
			b = withoutSecrets(b)
		}
		field("ctx:"+filepath.ToSlash(rel), b)
		return nil
	})
}
