// Package web is the local page server: one address on this machine
// where rows that have a page to show (the attention board, the
// artifacts an agent publishes) mount their routes. One process binds
// the address; the sessions started after it find the port taken and
// stay quiet — the pages are the same for all of them, because every
// row that mounts here reads its state from disk, not from the process.
//
// Row config: addr (default localhost:7683, or $BOUGH_WEB_ADDR when
// set: the test suites set 127.0.0.1:0 so the hundreds of processes
// they boot never take the user's port and serve their stale pages).
package web

import (
	"fmt"
	"net"
	"net/http"
	"os"
	"os/exec"
	"runtime"
	"sync"
	"time"

	"github.com/andreylukin/bough/kernel"
)

const defaultAddr = "localhost:7683"

// Service is the server: a mux and the address it is (or would be)
// served on.
type Service struct {
	conf string // the address as configured
	addr string // the address as bound (":0" resolved)
	// served is true when this process holds the listener.
	served bool

	rmu    sync.Mutex
	routes map[string]http.Handler
	mux    *http.ServeMux // rebuilt when routes change; nil = stale
}

// Handle mounts h at pattern (net/http mux syntax). Routes added by
// this process are served only when it holds the listener, which is
// the reason mounted pages must read from disk. A row unmounts in its
// Effect with Unhandle, so a remount does not double up.
func (s *Service) Handle(pattern string, h http.Handler) {
	s.rmu.Lock()
	defer s.rmu.Unlock()
	s.routes[pattern] = h
	s.mux = nil
}

// Unhandle removes a mounted route.
func (s *Service) Unhandle(pattern string) {
	s.rmu.Lock()
	defer s.rmu.Unlock()
	delete(s.routes, pattern)
	s.mux = nil
}

// ServeHTTP dispatches to the mounted routes.
func (s *Service) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	s.rmu.Lock()
	if s.mux == nil {
		s.mux = http.NewServeMux()
		for p, h := range s.routes {
			s.mux.Handle(p, h)
		}
	}
	mux := s.mux
	s.rmu.Unlock()
	mux.ServeHTTP(w, r)
}

// URL is the server's address for the browser.
func (s *Service) URL() string {
	host, port, err := net.SplitHostPort(s.addr)
	if err != nil {
		return "http://" + s.addr
	}
	if host == "" || host == "0.0.0.0" || host == "::" {
		host = "localhost"
	}
	return "http://" + net.JoinHostPort(host, port)
}

// Serving reports whether this process holds the listener.
func (s *Service) Serving() bool { return s.served }

// Open hands a page under the server to the desktop browser.
func Open(url string) error {
	cmd := "xdg-open"
	if runtime.GOOS == "darwin" {
		cmd = "open"
	}
	return exec.Command(cmd, url).Start()
}

// One listener per process: a remount reuses it.
var (
	mu     sync.Mutex
	shared *Service
)

// New returns the process's server for addr, binding it on first use.
// A bind failure is not an error: a peer bough is serving the same
// pages, and this process's URL still points at them.
func New(addr string) *Service {
	mu.Lock()
	defer mu.Unlock()
	if shared != nil && shared.conf == addr {
		return shared
	}
	s := &Service{conf: addr, addr: addr, routes: map[string]http.Handler{}}
	ln, err := net.Listen("tcp", addr)
	if err == nil {
		s.served = true
		if _, port, _ := net.SplitHostPort(addr); port == "0" {
			// A test's ":0" resolves to the port it got.
			s.addr = ln.Addr().String()
		}
		srv := &http.Server{Handler: s, ReadHeaderTimeout: 5 * time.Second}
		go func() {
			if err := srv.Serve(ln); err != nil && err != http.ErrServerClosed {
				fmt.Fprintln(os.Stderr, "web:", err)
			}
		}()
	}
	shared = s
	return s
}

type plugin struct{}

func init() {
	kernel.Register("web", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "web" }
func (plugin) Inject() []string { return nil }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	addr := defaultAddr
	if a := os.Getenv("BOUGH_WEB_ADDR"); a != "" {
		addr = a
	}
	for k, v := range cfg {
		if k != "addr" {
			return fmt.Errorf("web: unknown config key %q", k)
		}
		a, ok := v.(string)
		if !ok || a == "" {
			return fmt.Errorf("web: addr must be host:port, got %v", v)
		}
		addr = a
	}
	ctx.Provide("web", New(addr))
	return nil
}
