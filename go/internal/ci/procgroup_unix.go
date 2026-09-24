//go:build !windows

package ci

// A check is `sh -c <run>`, and sh forks: killing sh alone on ^C left
// a `go test` or a bundler running in the CI worktree after bough ci
// exited, still holding files the next run rewrites. Same shape as
// plugins/tools, which internal code does not import.

import (
	"os/exec"
	"syscall"
)

func ownProcessGroup(c *exec.Cmd) {
	c.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
}

// killProcessGroup ends c and everything it started; the negative pid
// addresses the group.
func killProcessGroup(c *exec.Cmd) error {
	return syscall.Kill(-c.Process.Pid, syscall.SIGKILL)
}
