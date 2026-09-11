//go:build !windows

package ui

import (
	"os/exec"
	"syscall"
)

// guardSysProc gives the guard its own session: no controlling
// terminal, so it cannot read the tty or take job-control signals.
func guardSysProc(c *exec.Cmd) { c.SysProcAttr = &syscall.SysProcAttr{Setsid: true} }
