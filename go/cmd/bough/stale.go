// A dev install (the binary living inside its own checkout) falls
// behind silently: commits land, sources change, and the symlinked
// binary keeps running yesterday's build. staleNotice catches that at
// launch and in --version, naming the fix.
package main

import (
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"runtime/debug"
	"strings"
	"time"
)

// buildRevision is the full VCS revision compiled into this binary
// ("" when built outside a checkout or without VCS stamping).
func buildRevision() string {
	bi, ok := debug.ReadBuildInfo()
	if !ok {
		return ""
	}
	for _, kv := range bi.Settings {
		if kv.Key == "vcs.revision" {
			return kv.Value
		}
	}
	return ""
}

// staleNotice reports why the running binary is behind its checkout,
// or "" when it is current or is not a dev install (no .git above the
// resolved executable: an installed copy is updated on purpose, not
// checked). Errors are swallowed: this is a hint, never a failure.
func staleNotice(exe string) string {
	root, ok := checkoutAbove(exe)
	if !ok {
		return ""
	}
	head := gitHead(root)
	exeInfo, err := os.Stat(exe)
	if err != nil {
		return ""
	}
	modDir, err := moduleDir(root)
	if err != nil {
		return ""
	}
	return staleness(buildRevision(), head, exeInfo.ModTime(), newestSource(modDir))
}

// staleness is the pure decision: a revision mismatch first, else
// sources newer than the binary. Times at zero are unknown and skip
// that rule.
func staleness(rev, head string, exeTime, srcTime time.Time) string {
	if rev != "" && head != "" && !strings.HasPrefix(head, rev) && !strings.HasPrefix(rev, head) {
		return fmt.Sprintf("this binary was built from %s; the checkout is at %s — run `bough update`", short(rev), short(head))
	}
	if !exeTime.IsZero() && !srcTime.IsZero() && srcTime.After(exeTime.Add(2*time.Second)) {
		return "the checkout's sources are newer than this binary — run `bough update`"
	}
	return ""
}

func short(rev string) string {
	if len(rev) > 8 {
		return rev[:8]
	}
	return rev
}

// checkoutAbove walks up from the executable to the nearest .git.
func checkoutAbove(exe string) (string, bool) {
	for dir := filepath.Dir(exe); ; {
		if hasGit(dir) {
			return dir, true
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			return "", false
		}
		dir = parent
	}
}

// gitHead is the checkout's HEAD revision, "" on any error.
func gitHead(root string) string {
	out, err := exec.Command("git", "-C", root, "rev-parse", "HEAD").Output()
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(out))
}

// newestSource is the latest mtime of any .go file under dir (skipping
// vendored and test-fixture trees); zero when none.
func newestSource(dir string) time.Time {
	var newest time.Time
	filepath.WalkDir(dir, func(p string, d fs.DirEntry, err error) error {
		if err != nil {
			return nil
		}
		if d.IsDir() {
			switch d.Name() {
			case ".git", "node_modules", "testdata", "test-results":
				return filepath.SkipDir
			}
			return nil
		}
		if !strings.HasSuffix(p, ".go") {
			return nil
		}
		if info, err := d.Info(); err == nil && info.ModTime().After(newest) {
			newest = info.ModTime()
		}
		return nil
	})
	return newest
}
