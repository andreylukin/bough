package projectdef

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
)

var LockfileNames = []string{"go.sum", "package-lock.json", "bun.lock", "bun.lockb", "yarn.lock", "pnpm-lock.yaml", "Cargo.lock", "poetry.lock", "uv.lock", "requirements.txt", "Gemfile.lock"}

// DefaultBase mirrors container.DefaultBase. It is duplicated rather than
// imported so projectdef stays a leaf package the runtime could depend on.
const DefaultBase = "docker.io/library/debian:bookworm"

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
	recipe := FileSetup
	if p.UsesDockerfile() {
		recipe = FileDockerfile
	}
	b, err := os.ReadFile(filepath.Join(p.Dir, recipe))
	if err != nil && !os.IsNotExist(err) {
		return "", fmt.Errorf("projectdef: hash %s: %w", p.Slug, err)
	}
	field(recipe, b)
	y, err := os.ReadFile(filepath.Join(p.Dir, FileYAML))
	if err != nil {
		return "", fmt.Errorf("projectdef: hash %s: %w", p.Slug, err)
	}
	field(FileYAML, y)
	base := p.Def.Base
	if base == "" {
		base = DefaultBase
	}
	field("base", []byte(base))
	for _, r := range p.Def.Repos {
		gd := SourceGitDir(home, p.Slug, r)
		if _, err := os.Stat(gd); err != nil {
			continue
		}
		for _, lf := range LockfileNames {
			out, err := exec.Command("git", "-C", gd, "show", r.BaseRef()+":"+lf).Output()
			if err != nil {
				continue // absent at that ref
			}
			field("lock:"+r.RepoName()+"/"+lf, out)
		}
	}
	return hex.EncodeToString(h.Sum(nil))[:12], nil
}

// Lockfiles writes each repo's lockfiles at its base ref into dir as
// <repo>/<lockfile> and returns the written paths, for CommitSpec.Files.
func Lockfiles(home string, p Project, dir string) ([]string, error) {
	var paths []string
	for _, r := range p.Def.Repos {
		gd := SourceGitDir(home, p.Slug, r)
		for _, lf := range LockfileNames {
			out, err := exec.Command("git", "-C", gd, "show", r.BaseRef()+":"+lf).Output()
			if err != nil {
				continue
			}
			dst := filepath.Join(dir, r.RepoName(), lf)
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
