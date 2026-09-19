package orb

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"sort"
	"strings"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
)

type Build struct {
	Tag   string `json:"tag"`
	Hash  string `json:"hash"`
	State string `json:"state"` // "", "building", "ok", "failed"
	// omitzero, not omitempty: omitempty does nothing to a struct, so a
	// project that has never been built shipped "0001-01-01T00:00:00Z"
	// and the UI aged it to six figures of days instead of saying Never.
	StartedAt time.Time `json:"startedAt,omitzero"`
	EndedAt   time.Time `json:"endedAt,omitzero"`
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

// FailedBuild reports that the last build of this hash failed: a failed
// build can leave its tag behind, and that tag is not a usable image.
func FailedBuild(home, slug, hash string) bool {
	b, _ := ReadBuild(home, slug)
	return b.State == "failed" && b.Hash == hash
}

// EnsureImage builds the project's snapshot image when its tag is missing.
// Builds are serialised per project by build.lock; a waiter re-hashes
// under the lock (the definition may have changed while it waited), so N
// sessions opened during one build cause one build. A waiter with a log
// tails the running build's build.log into it.
func EnsureImage(ctx context.Context, rt container.Runtime, home string, p projectdef.Project, log io.Writer) (string, error) {
	hash, err := projectdef.ImageHash(home, p)
	if err != nil {
		return "", fmt.Errorf("orb: image %s: %w", p.Slug, err)
	}
	tag := projectdef.ImageTag(p.Slug, hash)
	if ok, err := rt.ImageExists(ctx, tag); err != nil {
		return "", fmt.Errorf("orb: image %s: %w", tag, err)
	} else if ok && !FailedBuild(home, p.Slug, hash) {
		return tag, nil
	}
	dir := imagesDir(home, p.Slug)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return "", fmt.Errorf("orb: image %s: %w", tag, err)
	}
	stopTail := func() {}
	if b, _ := ReadBuild(home, p.Slug); log != nil && b.State == "building" {
		stopTail = tailFile(ImageLogPath(home, p.Slug), log)
	}
	unlock, err := lockFile(filepath.Join(dir, "build.lock"))
	stopTail()
	if err != nil {
		return "", fmt.Errorf("orb: image %s: lock: %w", tag, err)
	}
	defer unlock()
	// Re-hash and re-check under the lock: a waiter whose peer just built
	// must not truncate that peer's build.log or rebuild, and one whose
	// definition was edited meanwhile builds the new hash, not the stale one.
	if fresh, err := projectdef.Load(home, p.Slug); err == nil && fresh.Dir == p.Dir {
		p = fresh
	}
	if hash, err = projectdef.ImageHash(home, p); err != nil {
		return "", fmt.Errorf("orb: image %s: %w", p.Slug, err)
	}
	tag = projectdef.ImageTag(p.Slug, hash)
	if ok, err := rt.ImageExists(ctx, tag); err != nil {
		return "", fmt.Errorf("orb: image %s: %w", tag, err)
	} else if ok && !FailedBuild(home, p.Slug, hash) {
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
	var out tailBuffer
	berr := build(ctx, rt, home, p, tag, io.MultiWriter(w, &out))
	if berr != nil {
		berr = withLogLines(berr, out.String(), ImageLogPath(home, p.Slug))
	}
	b.EndedAt = time.Now().UTC()
	b.State = "ok"
	if berr != nil {
		b.State, b.Error = "failed", berr.Error()
		fmt.Fprintf(w, "\nbuild failed: %v\n", errors.Unwrap(berr))
	}
	if err := writeJSON(buildJSON, b); err != nil && berr == nil {
		berr = err
	}
	if berr != nil {
		return "", fmt.Errorf("orb: image %s: %w", tag, berr)
	}
	if err := pruneImages(ctx, rt, p.Slug, tag); err != nil {
		fmt.Fprintf(w, "prune old images: %v\n", err)
	}
	return tag, nil
}

// pruneImages removes the project's tags other than keep that no
// container, stopped or running, uses.
func pruneImages(ctx context.Context, rt container.Runtime, slug, keep string) error {
	if slug == "base" {
		return nil // bough-orb/base:* is bough's own base image
	}
	tags, err := rt.Images(ctx)
	if err != nil {
		return err
	}
	used, err := rt.ContainerImages(ctx)
	if err != nil {
		return err
	}
	var errs []error
	for _, t := range tags {
		if t != keep && strings.HasPrefix(t, "bough-orb/"+slug+":") && !slices.Contains(used, t) {
			errs = append(errs, rt.RemoveImage(ctx, t))
		}
	}
	return errors.Join(errs...)
}

// tailFile copies path's content to w as it grows, from the start, until
// the returned stop is called; stop copies what is left and returns once
// the copier has finished.
func tailFile(path string, w io.Writer) func() {
	done, stopped := make(chan struct{}), make(chan struct{})
	go func() {
		defer close(stopped)
		var off int64
		copyNew := func() {
			f, err := os.Open(path)
			if err != nil {
				return
			}
			defer f.Close()
			if fi, err := f.Stat(); err == nil && fi.Size() < off {
				off = 0 // a new build truncated it
			}
			n, _ := io.Copy(w, io.NewSectionReader(f, off, 1<<62))
			off += n
		}
		for {
			select {
			case <-done:
				copyNew()
				return
			case <-time.After(200 * time.Millisecond):
				copyNew()
			}
		}
	}()
	return func() {
		close(done)
		<-stopped
	}
}

func build(ctx context.Context, rt container.Runtime, home string, p projectdef.Project, tag string, w io.Writer) error {
	if p.UsesDockerfile() {
		return rt.Build(ctx, container.BuildSpec{Dir: p.Dir, Dockerfile: filepath.Join(p.Dir, projectdef.FileDockerfile), Tag: tag}, w)
	}
	// No setup.sh either: the image is the base with one empty layer. A
	// project that is only a place to work (no repos, no build) must still
	// have an image to run in; ImageHash already hashes the missing file.
	text, err := os.ReadFile(filepath.Join(p.Dir, projectdef.FileSetup))
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	steps, err := projectdef.ParseSteps(string(text))
	if err != nil {
		return err
	}
	tmp, err := os.MkdirTemp("", "bough-orb-lock-*")
	if err != nil {
		return err
	}
	defer os.RemoveAll(tmp)
	spec := container.CommitSpec{FilesRoot: tmp, Tag: tag}
	for _, s := range steps {
		lfs, err := projectdef.StepLockfiles(home, p, s.Uses)
		if err != nil {
			return err
		}
		st := container.Step{Name: s.Name, Script: []byte(s.Script)}
		for _, lf := range lfs {
			dst := filepath.Join(tmp, filepath.FromSlash(lf.Rel()))
			if err := os.MkdirAll(filepath.Dir(dst), 0o755); err != nil {
				return err
			}
			if err := os.WriteFile(dst, lf.Data, 0o644); err != nil {
				return err
			}
			st.Files = append(st.Files, dst)
		}
		spec.Steps = append(spec.Steps, st)
	}
	if spec.Base = p.Def.Base; spec.Base == "" {
		if spec.Base, err = EnsureBase(ctx, rt, w); err != nil {
			return err
		}
	}
	return rt.Commit(ctx, spec, w)
}

// EnsureBase builds bough's embedded base image when its tag is missing.
func EnsureBase(ctx context.Context, rt container.Runtime, log io.Writer) (string, error) {
	tag := projectdef.BaseTag()
	if ok, err := rt.ImageExists(ctx, tag); err != nil {
		return "", fmt.Errorf("orb: base image %s: %w", tag, err)
	} else if ok {
		return tag, nil
	}
	dir, err := os.MkdirTemp("", "bough-orb-base-*")
	if err != nil {
		return "", fmt.Errorf("orb: base image %s: %w", tag, err)
	}
	defer os.RemoveAll(dir)
	df := filepath.Join(dir, "Dockerfile")
	if err := os.WriteFile(df, projectdef.BaseDockerfile, 0o644); err != nil {
		return "", fmt.Errorf("orb: base image %s: %w", tag, err)
	}
	if err := rt.Build(ctx, container.BuildSpec{Dir: dir, Dockerfile: df, Tag: tag}, log); err != nil {
		return "", fmt.Errorf("orb: base image %s: %w", tag, err)
	}
	return tag, nil
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
