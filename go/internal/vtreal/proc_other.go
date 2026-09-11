//go:build !linux

package vtreal

import "os/exec"

// setDeathSig: no parent-death signal outside Linux; killChildren in
// TestMain is the only net.
func setDeathSig(*exec.Cmd) {}
