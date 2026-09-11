package vtreal

import (
	"os/exec"
	"syscall"
)

// setDeathSig kills the child when the test binary dies, even by
// SIGKILL or go test's timeout panic.
func setDeathSig(cmd *exec.Cmd) {
	if cmd.SysProcAttr == nil {
		cmd.SysProcAttr = &syscall.SysProcAttr{}
	}
	cmd.SysProcAttr.Pdeathsig = syscall.SIGKILL
}
