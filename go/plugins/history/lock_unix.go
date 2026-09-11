//go:build unix

package history

import (
	"os"
	"syscall"
)

// lockFile takes an exclusive advisory lock on f, blocking until held.
// A failed lock degrades to no lock rather than dropping the append.
func lockFile(f *os.File) (unlock func()) {
	fd := int(f.Fd())
	if syscall.Flock(fd, syscall.LOCK_EX) != nil {
		return func() {}
	}
	return func() { _ = syscall.Flock(fd, syscall.LOCK_UN) }
}
