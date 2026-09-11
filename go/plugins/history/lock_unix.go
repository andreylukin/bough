//go:build unix

package history

import (
	"os"
	"syscall"
	"time"
)

// lockWait bounds how long Append waits for another holder of the lock:
// Append runs on the turn's path, so a lock held forever (a stuck
// process, a stray flock) must not hang the session.
var lockWait = 2 * time.Second

// lockFile takes an exclusive advisory lock on f, waiting at most
// lockWait. A failed or timed-out lock degrades to no lock rather than
// dropping the append.
func lockFile(f *os.File) (unlock func()) {
	fd := int(f.Fd())
	deadline := time.Now().Add(lockWait)
	for {
		err := syscall.Flock(fd, syscall.LOCK_EX|syscall.LOCK_NB)
		if err == nil {
			return func() { _ = syscall.Flock(fd, syscall.LOCK_UN) }
		}
		if err != syscall.EWOULDBLOCK && err != syscall.EINTR || time.Now().After(deadline) {
			return func() {}
		}
		time.Sleep(10 * time.Millisecond)
	}
}
