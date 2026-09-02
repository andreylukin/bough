package ui

import (
	"context"
	"fmt"
	"net"
	"os"

	tea "charm.land/bubbletea/v2"
	"github.com/Gaurav-Gosain/sip"

	"github.com/andreylukin/bough/kernel"
)

// startWeb serves the same model per browser session via sip. All sessions
// share the one global inputs chan and loop event stream (v0).
func startWeb(ctx *kernel.Context, addr string, inputs chan<- string, b *broadcaster) error {
	host, port, err := net.SplitHostPort(addr)
	if err != nil {
		return fmt.Errorf("ui: bad web addr %q: %w", addr, err)
	}
	cfg := sip.DefaultConfig()
	cfg.Host, cfg.Port = host, port

	srvCtx, cancel := context.WithCancel(context.Background())
	ctx.Effect(cancel)

	server := sip.NewServer(cfg)
	go func() {
		err := server.Serve(srvCtx, func(sess sip.Session) (tea.Model, []tea.ProgramOption) {
			events, unsub := b.subscribe()
			go func() {
				<-sess.Context().Done()
				unsub()
			}()
			pty := sess.Pty()
			return newModel(pty.Width, pty.Height, inputs, events), nil
		})
		if err != nil && srvCtx.Err() == nil {
			fmt.Fprintln(os.Stderr, "ui: web:", err)
			interruptSelf()
		}
	}()
	return nil
}
