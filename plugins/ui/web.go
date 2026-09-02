package ui

import (
	"context"
	"fmt"
	"net"
	"os"
	"sync"

	tea "charm.land/bubbletea/v2"
	"github.com/Gaurav-Gosain/sip"
)

// The sip server is a process singleton (a remount must not re-bind
// the addr — the old hazard); sessions read the live broadcaster and
// send through the live inputs, so they survive ui-row remounts. All
// sessions share the one loop and event stream (v0).
var (
	webMu   sync.Mutex
	webAddr string // "" until the server is started
)

// startWeb starts the shared sip server on first call; later calls
// (remounts) are no-ops, loudly so if the addr changed.
func startWeb(addr string) error {
	webMu.Lock()
	defer webMu.Unlock()
	if webAddr != "" {
		if addr != webAddr {
			fmt.Fprintf(os.Stderr, "ui: web: addr change %q -> %q needs a restart; still serving %q\n", webAddr, addr, webAddr)
		}
		return nil
	}

	host, port, err := net.SplitHostPort(addr)
	if err != nil {
		return fmt.Errorf("ui: bad web addr %q: %w", addr, err)
	}
	cfg := sip.DefaultConfig()
	cfg.Host, cfg.Port = host, port
	webAddr = addr

	server := sip.NewServer(cfg)
	go func() {
		err := server.Serve(context.Background(), func(sess sip.Session) (tea.Model, []tea.ProgramOption) {
			events, unsub := liveB.subscribe()
			go func() {
				<-sess.Context().Done()
				unsub()
			}()
			pty := sess.Pty()
			return newModel(pty.Width, pty.Height, sendLive, events, &liveCfg), nil
		})
		if err != nil {
			fmt.Fprintln(os.Stderr, "ui: web:", err)
			interruptSelf()
		}
	}()
	return nil
}
