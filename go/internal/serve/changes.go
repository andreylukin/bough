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

	"github.com/andreylukin/bough/plugins/history"
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

// changesTimeout bounds one session's git reads, so a huge tree answers
// with an error instead of holding the request open.
const changesTimeout = 5 * time.Second

// Seams for tests: each request runs its own git, with no shared lock.
var (
	changesOf    = Changes
	sessionEdits = SessionEdits
)

func (a *API) changes(w http.ResponseWriter, r *http.Request) {
	in, ok := a.info(r.PathValue("id"))
	if !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", r.PathValue("id")))
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), changesTimeout)
	defer cancel()
	files, repo := changesOf(ctx, in.Cwd)
	if ctx.Err() != nil {
		writeErr(w, http.StatusGatewayTimeout, fmt.Errorf("serve: api: reading uncommitted changes took longer than %s (a large working tree?)", changesTimeout))
		return
	}
	if files == nil {
		files = []Change{}
	}
	writeJSON(w, http.StatusOK, map[string]any{"repo": repo, "files": files})
}

// Edit is one file the session's own turns changed, measured from the
// checkpoint taken before its first turn to the tree now. Patch is false
// when no checkpoint was recorded: the path is known, its lines are not.
type Edit struct {
	Change
	Patch bool `json:"patch"`
}

// baseline is the checkpoint the session's first recorded turn took
// before the model ran, and the files its turns recorded as changed
// (as recorded: relative to the session cwd, or absolute).
func baseline(entries []history.Entry) (tree string, files []string) {
	seen := map[string]bool{}
	for _, e := range entries {
		if cp, _ := e.Data["checkpoint"].(string); e.Kind == "input" && tree == "" && cp != "" {
			tree = cp
		}
		if e.Kind != "done" {
			continue
		}
		fs, _ := e.Data["files"].([]any)
		for _, f := range fs {
			if p, _ := f.(string); p != "" && !seen[p] {
				seen[p] = true
				files = append(files, p)
			}
		}
	}
	return tree, files
}

// relPath is a recorded path relative to dir, as git --relative prints it.
func relPath(dir, p string) string {
	if filepath.IsAbs(p) {
		if r, err := filepath.Rel(dir, p); err == nil {
			return r
		}
	}
	return filepath.Clean(p)
}

// SessionEdits is what this session's turns changed, never the checkout's
// other dirt: only files a turn recorded, each diffed from the checkpoint
// before the session's first turn, so an edit already in the tree when
// the session started is not counted as the agent's. A file put back as
// it was drops out. ok is false outside a repository.
func SessionEdits(ctx context.Context, dir string, entries []history.Entry) (edits []Edit, ok bool) {
	base, files := baseline(entries)
	return editsBetween(ctx, dir, base, "", files)
}

// turnSpan is one turn's checkpoint (its input's), the next turn's
// checkpoint ("" when it is the last: the tree now), and the files the
// turn recorded. found is false when no input has that seq.
func turnSpan(entries []history.Entry, turn int) (base, end string, files []string, found bool) {
	seen := map[string]bool{}
	for _, e := range entries {
		if e.Kind == "input" {
			if found {
				end, _ = e.Data["checkpoint"].(string)
				break
			}
			if e.Seq == int64(turn) {
				found = true
				base, _ = e.Data["checkpoint"].(string)
			}
			continue
		}
		if !found || e.Kind != "done" {
			continue
		}
		fs, _ := e.Data["files"].([]any)
		for _, f := range fs {
			if p, _ := f.(string); p != "" && !seen[p] {
				seen[p] = true
				files = append(files, p)
			}
		}
	}
	return base, end, files, found
}

// TurnEdits is what one turn (the seq of its input) changed: its files,
// from its checkpoint to the next turn's.
func TurnEdits(ctx context.Context, dir string, entries []history.Entry, turn int) ([]Edit, bool) {
	base, end, files, _ := turnSpan(entries, turn)
	return editsBetween(ctx, dir, base, end, files)
}

// editsBetween diffs files from the base checkpoint to end (the tree now when "").
func editsBetween(ctx context.Context, dir, base, end string, files []string) (edits []Edit, ok bool) {
	// git -C "" is the server's own cwd: a session with no recorded cwd has no repository.
	if dir == "" || exec.CommandContext(ctx, "git", "-C", dir, "rev-parse", "--git-dir").Run() != nil {
		return nil, false
	}
	if len(files) == 0 {
		return nil, true
	}
	now := end
	if base != "" && now == "" {
		now, _ = history.SnapshotContext(ctx, dir)
	}
	if now == "" {
		for _, f := range files {
			edits = append(edits, Edit{Change: Change{Path: relPath(dir, f), Add: -1, Del: -1}})
		}
		return edits, true
	}
	args := []string{"-C", dir, "diff", "--numstat", "--relative", base, now, "--"}
	for _, f := range files {
		if r := relPath(dir, f); r != ".." && !strings.HasPrefix(r, "../") {
			args = append(args, r)
		}
	}
	if args[len(args)-1] == "--" {
		return nil, true
	}
	out, err := exec.CommandContext(ctx, "git", args...).Output()
	if err != nil {
		return nil, true
	}
	for _, l := range strings.Split(strings.TrimSpace(string(out)), "\n") {
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
		c.New = exec.CommandContext(ctx, "git", "-C", dir, "cat-file", "-e", base+":./"+c.Path).Run() != nil
		edits = append(edits, Edit{Change: c, Patch: true})
	}
	return edits, true
}

// SessionDiff is one file's patch from the session's first checkpoint to now.
func SessionDiff(ctx context.Context, dir string, entries []history.Entry, path string) (string, error) {
	if dir == "" {
		return "", fmt.Errorf("no working directory was recorded for this session")
	}
	base, _ := baseline(entries)
	return diffBetween(ctx, dir, base, "", path)
}

// TurnDiff is one file's patch across one turn (the seq of its input).
func TurnDiff(ctx context.Context, dir string, entries []history.Entry, turn int, path string) (string, error) {
	if dir == "" {
		return "", fmt.Errorf("no working directory was recorded for this session")
	}
	base, end, _, found := turnSpan(entries, turn)
	if !found {
		return "", fmt.Errorf("no turn %d in this session", turn)
	}
	return diffBetween(ctx, dir, base, end, path)
}

func diffBetween(ctx context.Context, dir, base, end, path string) (string, error) {
	if base == "" {
		return "", fmt.Errorf("no checkpoint was recorded for this session")
	}
	now := end
	if now == "" {
		var err error
		if now, err = history.Snapshot(dir); err != nil {
			return "", err
		}
	}
	out, err := exec.CommandContext(ctx, "git", "-C", dir, "diff", "-U3", "--relative", base, now, "--", path).Output()
	return string(out), err
}

func (a *API) edits(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	in, ok := a.info(id)
	if !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return
	}
	entries, err := a.sup.Entries(id)
	if err != nil {
		writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: read session %q: %w", id, err))
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), changesTimeout)
	defer cancel()
	var files []Edit
	var repo bool
	if turn, err := strconv.Atoi(r.URL.Query().Get("turn")); err == nil {
		files, repo = TurnEdits(ctx, in.Cwd, entries, turn)
	} else {
		files, repo = sessionEdits(ctx, in.Cwd, entries)
	}
	if ctx.Err() != nil {
		writeErr(w, http.StatusGatewayTimeout, fmt.Errorf("serve: api: reading this session's edits took longer than %s (a large working tree?)", changesTimeout))
		return
	}
	if files == nil {
		files = []Edit{}
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
	var text string
	var err error
	if scope := r.URL.Query().Get("scope"); scope == "session" || scope == "turn" {
		entries, rerr := a.sup.Entries(in.ID)
		if rerr != nil {
			writeErr(w, http.StatusInternalServerError, rerr)
			return
		}
		if turn, terr := strconv.Atoi(r.URL.Query().Get("turn")); scope == "turn" && terr == nil {
			text, err = TurnDiff(ctx, in.Cwd, entries, turn, path)
		} else {
			text, err = SessionDiff(ctx, in.Cwd, entries, path)
		}
	} else {
		text, err = Diff(ctx, in.Cwd, path)
	}
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
