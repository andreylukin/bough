//go:build !windows

package testbin

import (
	"os"
	"syscall"
)

// lock takes an exclusive flock on path; the kernel drops it if the
// holder dies, so a killed build never wedges the next run.
func lock(path string) (func(), error) {
	f, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		return nil, err
	}
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX); err != nil {
		f.Close()
		return nil, err
	}
	return func() { f.Close() }, nil
}
