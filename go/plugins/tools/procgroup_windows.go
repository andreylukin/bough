//go:build windows

package tools

// Process-group handling, the Windows half.
//
// Windows has no process groups in the POSIX sense and no signals, so
// the two operations are spelled differently: a new console process
// group at spawn, and taskkill /T to end the tree. Process.Kill would
// end only the shell and orphan whatever it started, which is the very
// thing this exists to prevent.
//
// NOTE: bough is not tested on Windows. It compiles (CI builds this
// target on every push) and tools.bash still runs its script through
// `sh`, which means Git for Windows or another sh on PATH. Reports
// welcome.

import (
	"os/exec"
	"strconv"
	"syscall"
)

func ownProcessGroup(c *exec.Cmd) {
	c.SysProcAttr = &syscall.SysProcAttr{CreationFlags: syscall.CREATE_NEW_PROCESS_GROUP}
}

func killProcessGroup(c *exec.Cmd) error {
	// /T takes the children with it, /F does not ask.
	kill := exec.Command("taskkill", "/T", "/F", "/PID", strconv.Itoa(c.Process.Pid))
	if err := kill.Run(); err != nil {
		return c.Process.Kill() // taskkill missing: at least end the shell
	}
	return nil
}
