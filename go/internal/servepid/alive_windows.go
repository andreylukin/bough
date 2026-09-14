//go:build windows

package servepid

import "syscall"

// Alive reports whether pid exists. FindProcess succeeds for any pid on
// Windows, so the handle is opened and its exit code queried instead.
func Alive(pid int) bool {
	if pid <= 0 {
		return false
	}
	h, err := syscall.OpenProcess(syscall.PROCESS_QUERY_INFORMATION, false, uint32(pid))
	if err != nil {
		return false
	}
	defer syscall.CloseHandle(h)
	var code uint32
	if syscall.GetExitCodeProcess(h, &code) != nil {
		return false
	}
	const stillActive = 259
	return code == stillActive
}
