//go:build !windows

package ui

import (
	"os"
	"os/signal"
	"syscall"

	tea "charm.land/bubbletea/v2"
)

// watchResume re-takes the terminal after an external stop/continue
// (a shell's ^Z then fg, or kill -STOP/-CONT). bubbletea restores only
// after its own tea.Suspend; while bough was stopped the shell reset the
// alt screen, mouse and bracketed paste, so re-send them and repaint.
func watchResume(p *tea.Program) {
	ch := make(chan os.Signal, 1)
	signal.Notify(ch, syscall.SIGCONT)
	go func() {
		for range ch {
			_ = p.ReleaseTerminal()
			_ = p.RestoreTerminal()
			p.Send(tea.ClearScreen())
		}
	}()
}
