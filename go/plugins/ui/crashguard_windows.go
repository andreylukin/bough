//go:build windows

package ui

import "os/exec"

func guardSysProc(*exec.Cmd) {}
