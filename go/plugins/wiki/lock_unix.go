//go:build unix

package wiki

import (
	"os"
	"syscall"
)

// tryLock takes an exclusive, non-blocking lock on path: false when
// another ingest holds it. The lock dies with the process, so a crashed
// run never wedges the schedule.
func tryLock(path string) (unlock func(), ok bool) {
	f, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		return func() {}, false
	}
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		f.Close()
		return func() {}, false
	}
	return func() {
		_ = syscall.Flock(int(f.Fd()), syscall.LOCK_UN)
		f.Close()
	}, true
}
