package ui

import (
	"fmt"
	"os"
	"sync"

	tea "charm.land/bubbletea/v2"
)

// The terminal can only be owned once per process, but hot reload can
// remount the ui row; the bubbletea program is a process singleton
// wired to the live broadcaster/inputs (see live.go), so a remount
// re-points its config and channels instead of restarting it.
var tuiOnce sync.Once

// runTUI starts the bubbletea program on the real terminal (first
// mount only). When it quits, the process is interrupted so the
// launcher unmounts and exits 0.
func runTUI() {
	tuiOnce.Do(func() {
		events, _ := liveB.subscribe() // process-lifetime subscription
		go func() {
			m := newModel(80, 24, sendLive, events, &liveCfg) // real size arrives via WindowSizeMsg
			_, err := tea.NewProgram(m).Run()
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
