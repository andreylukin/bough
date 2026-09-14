package orb

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
)

type Build struct {
	Tag       string    `json:"tag"`
	Hash      string    `json:"hash"`
	State     string    `json:"state"` // "", "building", "ok", "failed"
	StartedAt time.Time `json:"startedAt"`
	EndedAt   time.Time `json:"endedAt,omitempty"`
	Error     string    `json:"error,omitempty"`
}

func imagesDir(home, slug string) string { return filepath.Join(orbsRoot(home), "images", slug) }

func ImageLogPath(home, slug string) string { return filepath.Join(imagesDir(home, slug), "build.log") }

func ReadBuild(home, slug string) (Build, error) {
	var b Build
	if err := readJSON(filepath.Join(imagesDir(home, slug), "build.json"), &b); err != nil {
		return Build{}, fmt.Errorf("orb: read build %s: %w", slug, err)
	}
	return b, nil
}

// EnsureImage builds the project's snapshot image when its tag is missing.
func EnsureImage(ctx context.Context, rt container.Runtime, home string, p projectdef.Project, log io.Writer) (string, error) {
	hash, err := projectdef.ImageHash(home, p)
	if err != nil {
		return "", fmt.Errorf("orb: image %s: %w", p.Slug, err)
	}
	tag := projectdef.ImageTag(p.Slug, hash)
	if ok, err := rt.ImageExists(ctx, tag); err != nil {
		return "", fmt.Errorf("orb: image %s: %w", tag, err)
	} else if ok {
		return tag, nil
	}
	dir := imagesDir(home, p.Slug)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return "", fmt.Errorf("orb: image %s: %w", tag, err)
	}
	unlock, err := lockFile(filepath.Join(dir, "build.lock"))
	if err != nil {
		return "", fmt.Errorf("orb: image %s: lock: %w", tag, err)
	}
	defer unlock()
	// Re-check under the lock: a waiter whose peer just built must not
	// truncate that peer's build.log or rebuild.
	if ok, err := rt.ImageExists(ctx, tag); err != nil {
		return "", fmt.Errorf("orb: image %s: %w", tag, err)
	} else if ok {
		return tag, nil
	}

	logf, err := os.Create(ImageLogPath(home, p.Slug))
	if err != nil {
		return "", fmt.Errorf("orb: image %s: %w", tag, err)
	}
	defer logf.Close()
	var w io.Writer = logf
	if log != nil {
		w = io.MultiWriter(logf, log)
	}
	b := Build{Tag: tag, Hash: hash, State: "building", StartedAt: time.Now().UTC()}
	buildJSON := filepath.Join(dir, "build.json")
	if err := writeJSON(buildJSON, b); err != nil {
		return "", fmt.Errorf("orb: image %s: %w", tag, err)
	}
	berr := build(ctx, rt, home, p, tag, w)
	b.EndedAt = time.Now().UTC()
	b.State = "ok"
	if berr != nil {
		b.State, b.Error = "failed", berr.Error()
		fmt.Fprintf(w, "\nbuild failed: %v\n", berr)
	}
	if err := writeJSON(buildJSON, b); err != nil && berr == nil {
		berr = err
	}
	if berr != nil {
		return "", fmt.Errorf("orb: image %s: %w", tag, berr)
	}
	return tag, nil
}

func build(ctx context.Context, rt container.Runtime, home string, p projectdef.Project, tag string, w io.Writer) error {
	if p.UsesDockerfile() {
		return rt.Build(ctx, container.BuildSpec{Dir: p.Dir, Dockerfile: filepath.Join(p.Dir, projectdef.FileDockerfile), Tag: tag}, w)
	}
	script := filepath.Join(p.Dir, projectdef.FileSetup)
	if _, err := os.Stat(script); err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return fmt.Errorf("project %s has neither %s nor %s", p.Slug, projectdef.FileDockerfile, projectdef.FileSetup)
		}
		return err
	}
	tmp, err := os.MkdirTemp("", "bough-orb-lock-*")
	if err != nil {
		return err
	}
	defer os.RemoveAll(tmp)
	files, err := projectdef.Lockfiles(home, p, tmp)
	if err != nil {
		return err
	}
	base := p.Def.Base
	if base == "" {
		base = container.DefaultBase
	}
	return rt.Commit(ctx, container.CommitSpec{Base: base, Script: script, Files: files, Env: envList(p.Def.Env), Tag: tag}, w)
}

// envList sorts so the generated image recipe is deterministic.
func envList(m map[string]string) []string {
	out := make([]string, 0, len(m))
	for k, v := range m {
		out = append(out, k+"="+v)
	}
	sort.Strings(out)
	return out
}
