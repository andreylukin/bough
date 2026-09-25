//go:build !windows

package connect

import (
	"os"
	"syscall"
)

// lockFile holds an exclusive flock on path+".lock" until unlock: serve's
// welcome and /connect in a terminal are separate processes writing the
// same env file.
func lockFile(path string) (unlock func(), err error) {
	f, err := os.OpenFile(path+".lock", os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return nil, err
	}
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX); err != nil {
		f.Close()
		return nil, err
	}
	return func() { syscall.Flock(int(f.Fd()), syscall.LOCK_UN); f.Close() }, nil
}
