//go:build !windows

package servetest

import (
	"os/exec"
	"syscall"
)

// setProcessGroup puts serve in a group of its own, so a hung serve can
// be killed together with the session children it spawned.
func setProcessGroup(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
}

// terminate asks serve to shut down; it ends its sessions itself.
func terminate(cmd *exec.Cmd) { cmd.Process.Signal(syscall.SIGTERM) }

func killGroup(cmd *exec.Cmd) { syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL) }

// groupAlive: signal 0 to the group reaches any member still running.
func groupAlive(cmd *exec.Cmd) bool { return syscall.Kill(-cmd.Process.Pid, 0) == nil }
