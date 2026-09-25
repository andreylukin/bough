//go:build !windows

package projectdef

import (
	"fmt"
	"os"
	"path/filepath"
	"syscall"
)

// locked runs fn holding the project's lock: an flock on its directory,
// so the writers of project.yml in different processes (each child's
// SetSecret, serve's SetName and editor save) take turns instead of
// each reading, changing and writing the whole file over the others'.
// A lock file inside the directory would be hashed into a Dockerfile
// build's context; the directory itself goes with DeleteProject.
func locked(home, slug string, fn func() error) error {
	f, err := os.Open(filepath.Join(Root(home), slug))
	if err != nil {
		return fmt.Errorf("projectdef: lock %s: %w", slug, err)
	}
	defer f.Close()
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX); err != nil {
		return fmt.Errorf("projectdef: lock %s: %w", slug, err)
	}
	defer syscall.Flock(int(f.Fd()), syscall.LOCK_UN)
	return fn()
}
