package servetest

import "os/exec"

// Windows has no SIGTERM or process groups here: a kill is all there is.
func setProcessGroup(cmd *exec.Cmd) {}

func terminate(cmd *exec.Cmd) { cmd.Process.Kill() }

func killGroup(cmd *exec.Cmd) { cmd.Process.Kill() }
