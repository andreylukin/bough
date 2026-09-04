//go:build !windows

package main

// The two places bough speaks POSIX signals, kept behind a build tag so
// the tree compiles for Windows (which has neither SIGUSR1 nor kill).

import (
	"os"
	"os/exec"
	"os/signal"
	"syscall"
)

// notifyFreshSession asks the runtime to deliver the "start a new
// session" signal on ch. SIGUSR1 is what `bough` sends a detached web
// session so opening the browser again is a new conversation.
// Returns false where the platform has no such signal.
func notifyFreshSession(ch chan os.Signal) bool {
	signal.Notify(ch, syscall.SIGUSR1)
	return true
}

// interrupt asks the process to stop the way ctrl+c would.
func interrupt(pid int) error { return syscall.Kill(pid, syscall.SIGINT) }

// detach puts a launched process in its own session, so `bough web`
// survives the shell that started it.
func detach(c *exec.Cmd) { c.SysProcAttr = &syscall.SysProcAttr{Setsid: true} }

// alive reports whether pid exists. Signal 0 checks without delivering;
// EPERM means it is there and not ours.
func alive(pid int) bool {
	if pid <= 0 {
		return false
	}
	err := syscall.Kill(pid, 0)
	return err == nil || err == syscall.EPERM
}

// askFreshSession tells a running session to start a new one.
func askFreshSession(pid int) error { return syscall.Kill(pid, syscall.SIGUSR1) }
