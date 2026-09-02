package ui

import (
	"sync/atomic"

	tea "charm.land/bubbletea/v2"

	"github.com/andreylukin/bough/kernel"
)

// NewTestModel constructs the real transcript model wired directly to a
// kernel Context, for in-process TUI-integration tests (internal/uitest).
// It reads the same optional services a live mount does (theme, keymap,
// history) via buildCfg, subscribes to "loop/event", and sends composer
// input into the mounted "inputs" channel — no tui goroutine, no
// process-level live wiring, so parallel tests each get their own model.
func NewTestModel(ctx *kernel.Context, width, height int) (tea.Model, error) {
	inputs, err := kernel.Get[chan string](ctx, "inputs")
	if err != nil {
		return nil, err
	}
	cfg, err := buildCfg(ctx, nil)
	if err != nil {
		return nil, err
	}
	ptr := &atomic.Pointer[uiCfg]{}
	ptr.Store(cfg)
	events := make(chan Event, 256)
	ctx.On("loop/event", func(payload any) {
		select {
		case events <- eventOf(payload):
		default: // a stuck test must not block the loop goroutine
		}
	})
	send := func(line string) {
		// A torn-down loop closes inputs; a straggling send must not
		// panic the test binary.
		defer func() { _ = recover() }()
		inputs <- line
	}
	return newModel(width, height, send, events, ptr), nil
}
