//go:build !windows

package ci

// A check is `sh -c <run>`, and sh forks: killing sh alone on ^C left
// a `go test` or a bundler running in the CI worktree after bough ci
// exited, still holding files the next run rewrites. Same shape as
// plugins/tools, which internal code does not import.

import (
	"os"
	"os/exec"
	"syscall"
)

// inheritLock hands the check the repo's lock fd. An flock belongs to
// the open file, not the process, so it is held until every copy is
// closed: when bough ci itself is SIGKILLed (the bash tool's timeout
// kills its own process group, and the check is in another one), the
// check it started keeps the lock, and the next bough ci waits for it
// instead of moving the worktree under a build that is still writing.
// Matching on a pgid saved in the lock file would do the same, but a
// recycled pgid would get an unrelated process group killed.
func inheritLock(c *exec.Cmd, f *os.File) {
	if f != nil {
		c.ExtraFiles = append(c.ExtraFiles, f)
	}
}

func ownProcessGroup(c *exec.Cmd) {
	c.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
}

// killProcessGroup ends c and everything it started; the negative pid
// addresses the group.
func killProcessGroup(c *exec.Cmd) error {
	return syscall.Kill(-c.Process.Pid, syscall.SIGKILL)
}
