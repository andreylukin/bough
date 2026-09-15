package orb

import (
	"os"
	"path/filepath"
	"strings"
)

// CheckoutRoot is the git checkout dir sits in, or "". It is the one
// directory a local session started in dir may write. A checkout that
// holds home (a dotfiles repo at ~) is not a project, so it answers "".
// The launcher and serve's first-run setup both ask, so the page that
// says "this folder is writable" and the session that then writes agree.
func CheckoutRoot(dir, home string) string {
	for d := filepath.Clean(dir); ; d = filepath.Dir(d) {
		if _, err := os.Stat(filepath.Join(d, ".git")); err == nil {
			if home != "" {
				if rel, err := filepath.Rel(d, home); err == nil && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
					return ""
				}
			}
			return d
		}
		if filepath.Dir(d) == d {
			return ""
		}
	}
}
