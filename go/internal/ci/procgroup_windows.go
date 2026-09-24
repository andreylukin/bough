//go:build windows

package ci

// Windows has no POSIX process groups: a new console group at spawn and
// taskkill /T to end the tree. bough is not tested on Windows; this
// exists so the package builds there.

import (
	"os/exec"
	"strconv"
	"syscall"
)

func ownProcessGroup(c *exec.Cmd) {
	c.SysProcAttr = &syscall.SysProcAttr{CreationFlags: syscall.CREATE_NEW_PROCESS_GROUP}
}

func killProcessGroup(c *exec.Cmd) error {
	kill := exec.Command("taskkill", "/T", "/F", "/PID", strconv.Itoa(c.Process.Pid))
	if err := kill.Run(); err != nil {
		return c.Process.Kill()
	}
	return nil
}
