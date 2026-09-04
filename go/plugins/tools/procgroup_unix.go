//go:build !windows

package tools

// Process-group handling, the Unix half.
//
// `sh -s` execs or forks the command it reads, so killing sh alone
// leaves a sleep, a server or a build running after the turn was
// cancelled. Both shell paths (tools.bash and a background job) put the
// shell in its own group and kill the group.

import (
	"os/exec"
	"syscall"
)

// ownProcessGroup puts c in a new process group so its whole tree can
// be killed together.
func ownProcessGroup(c *exec.Cmd) {
	c.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
}

// killProcessGroup kills c and everything it started. The negative pid
// addresses the group.
func killProcessGroup(c *exec.Cmd) error {
	return syscall.Kill(-c.Process.Pid, syscall.SIGKILL)
}
