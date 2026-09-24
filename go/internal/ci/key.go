package ci

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"sort"
	"strings"
)

// entry is one line of `git ls-tree -r`.
type entry struct{ mode, oid, path string }

// treeFiles is a tree's flat file list, read once per call and shared by
// every check's key and the config lookup.
type treeFiles struct {
	tree    string
	entries []entry // sorted by path
}

func listTree(ctx context.Context, r Repo, tree string) (*treeFiles, error) {
	out, err := gitRaw(ctx, r.Top, nil, "ls-tree", "-r", "-z", "--full-tree", tree)
	if err != nil {
		return nil, fmt.Errorf("ci: list tree %s: %w", short(tree), err)
	}
	tf := &treeFiles{tree: tree}
	for rec := range bytes.SplitSeq(out, []byte{0}) {
		if len(rec) == 0 {
			continue
		}
		meta, p, ok := strings.Cut(string(rec), "\t")
		f := strings.Fields(meta)
		if !ok || len(f) != 3 {
			return nil, fmt.Errorf("ci: list tree %s: unexpected line %q", short(tree), rec)
		}
		tf.entries = append(tf.entries, entry{mode: f[0], oid: f[2], path: p})
	}
	sort.Slice(tf.entries, func(i, j int) bool { return tf.entries[i].path < tf.entries[j].path })
	return tf, nil
}

func (tf *treeFiles) find(p string) (entry, bool) {
	i := sort.Search(len(tf.entries), func(i int) bool { return tf.entries[i].path >= p })
	if i < len(tf.entries) && tf.entries[i].path == p {
		return tf.entries[i], true
	}
	return entry{}, false
}

// cacheKey is what a check's result is stored under. It covers the
// check's definition (run and dir: an edited command must rerun) and
// then either
//
//   - with inputs: the mode, blob id and path of every file the globs
//     match — mode so an exec-bit flip counts, path so a rename does;
//   - otherwise (go_cache, or no inputs): the tree id. A key over "all
//     tracked files" would hash exactly what the tree id already hashes,
//     so the tree id is that key, for free. A go_cache check therefore
//     reruns on any change; the rerun is cheap because the CI worktree
//     keeps its path and unchanged mtimes, so go's cache hits.
//
// What the key cannot see: the host toolchain, the environment, and
// anything a check fetches. --rerun is the way past a result that is
// stale for one of those reasons.
func cacheKey(c Check, tf *treeFiles) string {
	h := sha256.New()
	dir, _ := cleanDir(c.Dir)
	kind := "tree"
	if len(c.Inputs) > 0 {
		kind = "inputs"
	}
	fmt.Fprintf(h, "bough-ci/v1\x00%s\x00%s\x00%s\x00", c.Run, dir, kind)
	if kind == "tree" {
		h.Write([]byte(tf.tree))
	} else {
		for _, e := range tf.entries {
			for _, g := range c.Inputs {
				if matchGlob(g, e.path) {
					fmt.Fprintf(h, "%s %s\t%s\x00", e.mode, e.oid, e.path)
					break
				}
			}
		}
	}
	return hex.EncodeToString(h.Sum(nil))
}
