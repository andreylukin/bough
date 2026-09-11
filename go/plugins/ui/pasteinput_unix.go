//go:build unix

package ui

import (
	"time"

	"golang.org/x/sys/unix"
)

// waitReadable reports whether fd has input within d. select, not poll:
// macOS poll does not work on ttys.
func waitReadable(fd uintptr, d time.Duration) bool {
	var set unix.FdSet
	set.Set(int(fd))
	tv := unix.NsecToTimeval(d.Nanoseconds())
	n, err := unix.Select(int(fd)+1, &set, nil, nil, &tv)
	return err == nil && n > 0
}
