package history

// Turn checkpoints and session forking. A checkpoint is the working
// tree written as a git tree object through a temporary index (HEAD
// and the real index are never touched), pinned under
// refs/bough/turns/<session>/<seq> so gc keeps it; /undo reads files
// back out of it. A fork copies the ancestors of one turn into a new
// session file — the original is never rewritten.

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"maps"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
	"time"

	"github.com/andreylukin/bough/kernel"
)

// git runs one git command in dir with extra env, returning trimmed
// stdout; stderr rides along in the error.
func git(dir string, env []string, args ...string) (string, error) {
	c := exec.Command("git", args...)
	c.Dir = dir
	c.Env = append(os.Environ(), env...)
	var out, stderr bytes.Buffer
	c.Stdout, c.Stderr = &out, &stderr
	if err := c.Run(); err != nil {
		return "", fmt.Errorf("git %s: %w: %s", args[0], err, strings.TrimSpace(stderr.String()))
	}
	return strings.TrimSpace(out.String()), nil
}

// Snapshot writes the working tree around dir (untracked included,
// ignored excluded) as a git tree object and returns its id. It uses
// a temporary index seeded from the real one — so only changed files
// are hashed — and touches neither the index nor HEAD. An error means
// dir is not in a git repo (or git is missing).
func Snapshot(dir string) (string, error) {
	index, err := git(dir, nil, "rev-parse", "--git-path", "index")
	if err != nil {
		return "", err
	}
	if !filepath.IsAbs(index) {
		index = filepath.Join(dir, index)
	}
	tmp, err := os.CreateTemp("", "bough-index-*")
	if err != nil {
		return "", err
	}
	tmp.Close()
	defer os.Remove(tmp.Name())
	if data, err := os.ReadFile(index); err == nil {
		if err := os.WriteFile(tmp.Name(), data, 0o600); err != nil {
			return "", err
		}
	}
	env := []string{"GIT_INDEX_FILE=" + tmp.Name()}
	if _, err := git(dir, env, "add", "-A"); err != nil {
		return "", err
	}
	return git(dir, env, "write-tree")
}

var refUnsafe = regexp.MustCompile(`[^A-Za-z0-9._-]`)

// TurnRef is the ref a turn's checkpoint is pinned under; the session
// id is sanitized (a ":" from an old RFC3339-stamped session name is
// not ref-safe; a UUIDv7 id needs no sanitizing).
func TurnRef(session string, seq int64) string {
	return fmt.Sprintf("refs/bough/turns/%s/%d", refUnsafe.ReplaceAllString(session, "-"), seq)
}

// PinRef names tree as turn seq's checkpoint of session in the repo
// around dir, so gc keeps the object.
func PinRef(dir, session string, seq int64, tree string) error {
	_, err := git(dir, nil, "update-ref", TurnRef(session, seq), tree)
	return err
}

// Skipped is a path Restore left alone, and why.
type Skipped struct {
	Path, Why string
}

// ignored reports whether rel (toplevel-relative) is gitignored. Such
// a file is never in a checkpoint (Snapshot honours .gitignore), so
// its absence from the tree says nothing about whether it existed.
func ignored(top, rel string) bool {
	c := exec.Command("git", "check-ignore", "-q", "--", rel)
	c.Dir = top
	return c.Run() == nil
}

// Restore puts exactly the listed files back to their content in tree
// (a Snapshot id): a path the tree lacks is deleted. Nothing else in
// the working tree is touched. Paths are as the tools recorded them
// (relative to dir, or absolute); one outside the repo, gitignored,
// or not a regular file in the checkpoint is skipped, never deleted.
// Returns the paths restored and the ones skipped.
func Restore(dir, tree string, files []string) (restored []string, skipped []Skipped, err error) {
	top, err := git(dir, nil, "rev-parse", "--show-toplevel")
	if err != nil {
		return nil, nil, err
	}
	if r, err := filepath.EvalSymlinks(dir); err == nil {
		dir = r
	}
	for _, f := range files {
		abs := f
		if !filepath.IsAbs(abs) {
			abs = filepath.Join(dir, f)
		}
		parent := filepath.Dir(abs)
		if r, err := filepath.EvalSymlinks(parent); err == nil {
			parent = r
		}
		abs = filepath.Join(parent, filepath.Base(abs))
		rel, err := filepath.Rel(top, abs)
		if err != nil || rel == ".." || strings.HasPrefix(rel, "../") {
			skipped = append(skipped, Skipped{f, "outside the repo"})
			continue
		}
		// rel is toplevel-relative and ls-tree scopes a pathspec to
		// its cwd, so run in top (dir may be a subdirectory).
		entry, err := git(top, nil, "ls-tree", "-z", tree, "--", rel)
		if err != nil {
			return restored, skipped, err
		}
		if entry == "" {
			if ignored(top, rel) {
				skipped = append(skipped, Skipped{f, "gitignored"})
				continue
			}
			if err := os.Remove(abs); err != nil && !errors.Is(err, fs.ErrNotExist) {
				return restored, skipped, err
			}
			restored = append(restored, f)
			continue
		}
		// "<mode> blob <hash>\t<path>"; a symlink is a blob too
		// (mode 120000) and must not come back as a regular file.
		fields := strings.Fields(strings.SplitN(entry, "\t", 2)[0])
		if len(fields) != 3 || fields[1] != "blob" || (fields[0] != "100644" && fields[0] != "100755") {
			skipped = append(skipped, Skipped{f, "not a regular file in the checkpoint"})
			continue
		}
		c := exec.Command("git", "cat-file", "blob", fields[2])
		c.Dir = top
		content, err := c.Output()
		if err != nil {
			return restored, skipped, fmt.Errorf("git cat-file %s: %w", rel, err)
		}
		mode := fs.FileMode(0o644)
		if fields[0] == "100755" {
			mode = 0o755
		}
		if err := os.MkdirAll(parent, 0o755); err != nil {
			return restored, skipped, err
		}
		if err := os.WriteFile(abs, content, mode); err != nil {
			return restored, skipped, err
		}
		restored = append(restored, f)
	}
	return restored, skipped, nil
}

// Checkpoints is the "checkpoints" service: the loop snapshots the
// working tree through it before each turn (Snapshot) and, once the
// turn's input entry has its seq, pins the tree (Pin). Both are quiet
// no-ops outside a git repo — one verbose note, never an error.
type Checkpoints struct {
	session string
}

// Snapshot snapshots the working tree around the process cwd; "" when
// there is none to take.
func (c *Checkpoints) Snapshot() string {
	cwd, err := os.Getwd()
	if err != nil {
		return ""
	}
	tree, err := Snapshot(cwd)
	if err != nil {
		kernel.Logf("bough: history: no checkpoint: %v\n", err)
		return ""
	}
	return tree
}

// Pin names tree as turn seq's checkpoint.
func (c *Checkpoints) Pin(seq int64, tree string) {
	cwd, err := os.Getwd()
	if err != nil {
		return
	}
	if err := PinRef(cwd, c.session, seq, tree); err != nil {
		kernel.Logf("bough: history: pin checkpoint: %v\n", err)
	}
}

// Ancestors is the chain of entries ending at seq, root first: each
// entry's ParentOf, walked back (a seq missing from a corrupt line is
// stepped over).
func Ancestors(entries []Entry, seq int64) []Entry {
	bySeq := make(map[int64]Entry, len(entries))
	for _, e := range entries {
		bySeq[e.Seq] = e
	}
	var out []Entry
	for cur := seq; cur > 0; {
		e, ok := bySeq[cur]
		if !ok {
			cur--
			continue
		}
		out = append(out, e)
		cur = ParentOf(e)
	}
	slices.Reverse(out)
	return out
}

// Fork writes a new session file at dst holding the ancestors of the
// turn whose input entry is seq in src — up to and including that
// turn's done entry — with the origin recorded on the meta entry as
// {forked_from: src, at_seq: seq}. src is never rewritten; dst must
// not exist. A seq that is not an input, or a turn with no done yet,
// is an error.
func Fork(src string, seq int64, dst string) error {
	entries, err := readEntries(src)
	if err != nil {
		return fmt.Errorf("fork: %w", err)
	}
	var done int64
	for i, e := range entries {
		if e.Seq != seq || e.Kind != "input" {
			continue
		}
		for _, d := range entries[i+1:] {
			if d.Kind == "done" {
				done = d.Seq
				break
			}
		}
		if done == 0 {
			return fmt.Errorf("fork: turn %d is not finished", seq)
		}
	}
	if done == 0 {
		return fmt.Errorf("fork: no turn %d (/tree lists them)", seq)
	}
	anc := Ancestors(entries, done)
	origin := map[string]any{"forked_from": src, "at_seq": seq}
	if anc[0].Kind == "meta" {
		data := make(map[string]any, len(anc[0].Data)+2)
		maps.Copy(data, anc[0].Data)
		maps.Copy(data, origin)
		anc[0].Data = data
	} else {
		last := anc[len(anc)-1]
		anc = append(anc, Entry{Seq: last.Seq + 1, At: time.Now(), Kind: "meta", Data: origin, Parent: last.Seq})
	}
	f, err := os.OpenFile(dst, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o644)
	if err != nil {
		return fmt.Errorf("fork: %w", err)
	}
	for _, e := range anc {
		line, err := json.Marshal(e)
		if err == nil {
			_, err = f.Write(append(line, '\n'))
		}
		if err != nil {
			f.Close()
			return fmt.Errorf("fork: %w", err)
		}
	}
	return f.Close()
}
