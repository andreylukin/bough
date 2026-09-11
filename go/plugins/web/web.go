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
	"bytes"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"os"
	"os/exec"
	"runtime"
	"runtime/debug"
	"strings"
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
	// peer is true when a same-build bough with our $HOME holds addr.
	peer bool
	// notice says why pages are not on the configured address ("" =
	// they are, or a same-build peer serves them).
	notice string

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

// Reachable reports whether this process's URLs load: it serves them,
// or a same-build bough with our $HOME holding the port does.
func (s *Service) Reachable() bool { return s.served || s.peer }

// Notice is a one-line user-facing warning when the pages moved off
// the configured address; "" otherwise.
func (s *Service) Notice() string { return s.notice }

// identityPath is where a serving bough says who it is, so a process
// that finds the port taken can tell a same-build peer (share it) from
// a stale build, another $HOME, or some other program (serve its own).
const identityPath = "/_bough/identity"

// Identity is what the holder of the listener serves at identityPath.
type Identity struct {
	Build string `json:"build"`
	Home  string `json:"home"`
	PID   int    `json:"pid"`
}

// self is this process's identity; a var so tests can pose as another build.
var self = func() Identity {
	home, _ := os.UserHomeDir()
	return Identity{Build: buildID(), Home: home, PID: os.Getpid()}
}

// buildID names the binary: its VCS revision plus the executable's
// path and mtime, so two builds of one dirty tree still differ.
func buildID() string {
	id := ""
	if bi, ok := debug.ReadBuildInfo(); ok {
		for _, kv := range bi.Settings {
			if kv.Key == "vcs.revision" || kv.Key == "vcs.modified" {
				id += kv.Value + " "
			}
		}
	}
	if exe, err := os.Executable(); err == nil {
		if fi, err := os.Stat(exe); err == nil {
			id += fmt.Sprintf("%s@%d", exe, fi.ModTime().UnixNano())
		}
	}
	return id
}

// probe asks whatever holds addr who it is: "" = a same-build peer
// with our $HOME; otherwise the reason not to trust it.
func probe(addr string) string {
	c := &http.Client{Timeout: 2 * time.Second}
	r, err := c.Get((&Service{addr: addr}).URL() + identityPath)
	if err != nil {
		return "is not answering http"
	}
	defer r.Body.Close()
	var id Identity
	if r.StatusCode != http.StatusOK || json.NewDecoder(r.Body).Decode(&id) != nil || id.Build == "" {
		return "is not bough (or a bough too old to say)"
	}
	me := self()
	switch {
	case id.Build != me.Build:
		return fmt.Sprintf("is a different bough build (pid %d)", id.PID)
	case id.Home != me.Home:
		return fmt.Sprintf("is a bough with another HOME %s (pid %d)", id.Home, id.PID)
	}
	return ""
}

// Open hands a page under the server to the desktop browser.
func Open(url string) error {
	cmd := "xdg-open"
	if runtime.GOOS == "darwin" {
		cmd = "open"
	}
	c := exec.Command(cmd, url)
	var stderr bytes.Buffer
	c.Stderr = &stderr
	if err := c.Start(); err != nil {
		return err
	}
	// open/xdg-open hand off and exit at once; a failure (no handler,
	// no display) is a non-zero exit. One that lingers is not waited on.
	done := make(chan error, 1)
	go func() { done <- c.Wait() }()
	select {
	case err := <-done:
		if err != nil {
			return fmt.Errorf("%s: %v %s", cmd, err, strings.TrimSpace(stderr.String()))
		}
	case <-time.After(2 * time.Second):
	}
	return nil
}

// One listener per process: a remount reuses it.
var (
	mu     sync.Mutex
	shared *Service
)

// New returns the process's server for addr, binding it on first use.
// A bind failure is not an error when a same-build bough with the same
// $HOME holds the port: it serves the same pages and this process's
// URL points at them. Anything else there (a stale build, a test's
// temp $HOME, another program) would serve the wrong pages or none, so
// this process binds a free port instead and says so in Notice.
func New(addr string) *Service {
	mu.Lock()
	defer mu.Unlock()
	if shared != nil && shared.conf == addr {
		return shared
	}
	s := &Service{conf: addr, addr: addr, routes: map[string]http.Handler{}}
	s.routes[identityPath] = http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(self())
	})
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		if why := probe(addr); why == "" {
			s.peer = true
		} else {
			host, _, _ := net.SplitHostPort(addr)
			if l, err2 := net.Listen("tcp", net.JoinHostPort(host, "0")); err2 == nil {
				ln, err = l, nil
				old := s.URL()
				s.addr = ln.Addr().String()
				s.notice = fmt.Sprintf("web: %s %s, so pages are served at %s instead", old, why, s.URL())
			}
		}
	}
	if err == nil {
		s.served = true
		if _, port, _ := net.SplitHostPort(s.addr); port == "0" {
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
