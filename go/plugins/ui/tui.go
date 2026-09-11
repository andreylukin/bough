package ui

import (
	"fmt"
	"os"
	"sync"
	"time"

	tea "charm.land/bubbletea/v2"
)

// The terminal can only be owned once per process, but hot reload can
// remount the ui row; the bubbletea program is a process singleton
// wired to the live broadcaster/inputs (see live.go), so a remount
// re-points its config and channels instead of restarting it.
var tuiOnce sync.Once

// tuiProg and tuiDone let StopTUI quit the program and wait for Run
// to restore the terminal; nil/unset when no tui runs.
var (
	tuiProg *tea.Program
	tuiDone = make(chan struct{})
)

// runTUI starts the bubbletea program on the real terminal (first
// mount only). When it quits, the process is interrupted so the
// launcher unmounts and exits 0.
func runTUI() {
	tuiOnce.Do(func() {
		events, _ := liveB.subscribe() // process-lifetime subscription
		go func() {
			m := newModel(80, 24, sendLive, events, &liveCfg) // real size arrives via WindowSizeMsg
			// main owns the signals (SIGINT/SIGTERM/SIGHUP): bubbletea's
			// own handler would tear the ui down past the unmount.
			p := tea.NewProgram(m, tea.WithoutSignalHandler())
			tuiMu.Lock()
			tuiProg = p
			tuiMu.Unlock()
			_, err := p.Run()
			close(tuiDone)
			if err != nil {
				fmt.Fprintln(os.Stderr, "ui: tui:", err)
				os.Exit(1)
			}
			if line := exitLine(liveCfg.Load().hist); line != "" {
				fmt.Fprintln(os.Stderr, line)
			}
			interruptSelf()
		}()
	})
}

var tuiMu sync.Mutex

// StopTUI quits the running tui and waits (bounded) for it to hand the
// terminal back. A no-op when no tui runs.
func StopTUI() {
	tuiMu.Lock()
	p := tuiProg
	tuiMu.Unlock()
	if p == nil {
		return
	}
	p.Quit()
	select {
	case <-tuiDone:
	case <-time.After(3 * time.Second):
	}
}
