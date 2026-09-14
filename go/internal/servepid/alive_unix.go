//go:build !windows

package servepid

import "syscall"

// Alive reports whether pid exists. Signal 0 checks without delivering;
// EPERM means it is there and not ours. pid must be positive — never
// signal 0/negative (process groups).
func Alive(pid int) bool {
	if pid <= 0 {
		return false
	}
	err := syscall.Kill(pid, 0)
	return err == nil || err == syscall.EPERM
}
