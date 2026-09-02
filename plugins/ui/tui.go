package ui

import (
	"fmt"
	"os"

	tea "charm.land/bubbletea/v2"
)

// runTUI runs the bubbletea program on the real terminal. When it quits,
// the process is interrupted so the launcher unmounts and exits 0.
func runTUI(inputs chan<- string, b *broadcaster) {
	events, unsub := b.subscribe()
	m := newModel(80, 24, inputs, events) // real size arrives via WindowSizeMsg
	_, err := tea.NewProgram(m).Run()
	unsub()
	if err != nil {
		fmt.Fprintln(os.Stderr, "ui: tui:", err)
		os.Exit(1)
	}
	interruptSelf()
}
