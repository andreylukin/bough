package serve

import (
	"context"
	"fmt"
	"net/http"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

// Change is one file the working tree differs from HEAD in. Add and Del
// are -1 for a binary file, which git counts in no lines.
type Change struct {
	Path string `json:"path"`
	Add  int    `json:"add"`
	Del  int    `json:"del"`
	New  bool   `json:"new,omitempty"` // untracked
}

// Changes is what is uncommitted in dir right now: the evidence a person
// reviews, read live from git rather than from what the agent said it did.
// ok is false outside a repository.
func Changes(ctx context.Context, dir string) (files []Change, ok bool) {
	git := func(args ...string) (string, error) {
		out, err := exec.CommandContext(ctx, "git", append([]string{"-C", dir}, args...)...).Output()
		return string(out), err
	}
	if _, err := git("rev-parse", "--git-dir"); err != nil {
		return nil, false
	}
	out, err := git("diff", "--numstat", "HEAD")
	if err != nil {
		// A repository with no commits yet has no HEAD to diff against.
		out, _ = git("diff", "--numstat", "--cached")
	}
	for _, l := range strings.Split(strings.TrimSpace(out), "\n") {
		f := strings.SplitN(l, "\t", 3)
		if len(f) != 3 {
			continue
		}
		c := Change{Path: f[2], Add: -1, Del: -1}
		if n, err := strconv.Atoi(f[0]); err == nil {
			c.Add = n
		}
		if n, err := strconv.Atoi(f[1]); err == nil {
			c.Del = n
		}
		files = append(files, c)
	}
	untracked, _ := git("ls-files", "--others", "--exclude-standard")
	for _, p := range strings.Split(strings.TrimSpace(untracked), "\n") {
		if p != "" {
			files = append(files, Change{Path: p, New: true})
		}
	}
	return files, true
}

func (a *API) changes(w http.ResponseWriter, r *http.Request) {
	in, ok := a.info(r.PathValue("id"))
	if !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", r.PathValue("id")))
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 5*time.Second)
	defer cancel()
	files, repo := Changes(ctx, in.Cwd)
	if files == nil {
		files = []Change{}
	}
	writeJSON(w, http.StatusOK, map[string]any{"repo": repo, "files": files})
}

// Diff is one file's unified diff against HEAD with 3 lines of context;
// an untracked file is diffed against nothing, so all of it reads as added.
func Diff(ctx context.Context, dir, path string) (string, error) {
	out, err := exec.CommandContext(ctx, "git", "-C", dir, "diff", "-U3", "HEAD", "--", path).Output()
	if err == nil && len(out) == 0 {
		// Untracked: --no-index exits 1 whenever the files differ.
		out, _ = exec.CommandContext(ctx, "git", "-C", dir, "diff", "-U3", "--no-index", "--", "/dev/null", path).Output()
	}
	return string(out), err
}

func (a *API) diff(w http.ResponseWriter, r *http.Request) {
	in, ok := a.info(r.PathValue("id"))
	if !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", r.PathValue("id")))
		return
	}
	path := r.URL.Query().Get("path")
	if path == "" || filepath.IsAbs(path) || strings.HasPrefix(filepath.Clean(path), "..") {
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: diff path %q is not inside the tree", path))
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 5*time.Second)
	defer cancel()
	text, err := Diff(ctx, in.Cwd, path)
	if err != nil {
		writeErr(w, http.StatusConflict, fmt.Errorf("serve: api: diff %s: %w", path, err))
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"diff": text})
}

// killJob stops one of a session's background jobs. The job lives in the
// child, so this asks the child to kill it; the job's own finished entry
// then takes it off the running list.
func (a *API) killJob(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	job, err := strconv.Atoi(r.PathValue("job"))
	if err != nil {
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: job id %q is not a number", r.PathValue("job")))
		return
	}
	if err := a.sup.Send(id, "/jobkill "+strconv.Itoa(job)); err != nil {
		writeErr(w, http.StatusConflict, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}
