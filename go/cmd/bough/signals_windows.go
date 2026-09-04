//go:build windows

package main

// Windows has no SIGUSR1 and no kill(2); see signals_unix.go for what
// these do elsewhere. bough is not tested on Windows — it compiles, and
// CI builds this target on every push.

import (
	"fmt"
	"os"
	"os/exec"
	"syscall"
)

// notifyFreshSession does nothing: there is no spare user signal to
// carry the request. A detached web session on Windows keeps its
// conversation until it is restarted.
func notifyFreshSession(chan os.Signal) bool { return false }

// interrupt has no gentle form here, so this reports rather than
// pretending: `bough web stop` says what it could not do.
func interrupt(pid int) error {
	p, err := os.FindProcess(pid)
	if err != nil {
		return fmt.Errorf("find pid %d: %w", pid, err)
	}
	return p.Kill()
}

// detach launches without a console, which is the closest Windows has
// to Setsid for a background server.
func detach(c *exec.Cmd) {
	c.SysProcAttr = &syscall.SysProcAttr{CreationFlags: 0x00000008} // DETACHED_PROCESS
}

// alive reports whether pid exists. FindProcess succeeds for any pid on
// Windows, so the handle is opened and its exit code queried instead.
func alive(pid int) bool {
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

// askFreshSession has no signal to send here (see notifyFreshSession).
func askFreshSession(int) error {
	return fmt.Errorf("starting a new session in a running web process is not supported on Windows")
}
