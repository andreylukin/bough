//go:build !windows

package ci

import (
	"errors"
	"os"
	"syscall"
)

// lock takes an exclusive flock on path. The kernel drops it when the
// holder dies, so a killed `bough ci` never wedges the next one the way
// a stale O_EXCL lockfile would. With wait false it never blocks and
// held reports whether the lock was taken.
func lock(path string, wait bool) (unlock func(), held bool, err error) {
	f, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		return nil, false, err
	}
	how := syscall.LOCK_EX
	if !wait {
		how |= syscall.LOCK_NB
	}
	for {
		err = syscall.Flock(int(f.Fd()), how)
		if !errors.Is(err, syscall.EINTR) {
			break
		}
	}
	if err != nil {
		f.Close()
		if !wait && errors.Is(err, syscall.EWOULDBLOCK) {
			return nil, false, nil
		}
		return nil, false, err
	}
	return func() { f.Close() }, true, nil
}
