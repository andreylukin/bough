package attention

// The board as a web page: one HTML file served by this row on its
// own address (sip owns the TUI server's routes), backed by two JSON
// endpoints the page polls. /current-work opens it in the browser.

import (
	_ "embed"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"os"
	"os/exec"
	"runtime"
	"strconv"
	"sync"
	"time"
)

//go:embed board.html
var boardHTML []byte

// A process serves the page once; TUI sessions that mount the same
// row after the web session hold the address find it taken and stay
// quiet — the page is the same for all of them.
var (
	webMu   sync.Mutex
	webAddr string
)

// serveWeb binds addr and serves the board; later calls are no-ops.
func (s *Service) serveWeb(addr string) {
	webMu.Lock()
	defer webMu.Unlock()
	if webAddr != "" {
		return
	}
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		// Another bough (the web session) is serving it; that is fine.
		return
	}
	webAddr = addr
	mux := http.NewServeMux()
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/" {
			http.NotFound(w, r)
			return
		}
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		_, _ = w.Write(boardHTML)
	})
	mux.HandleFunc("/api/board", func(w http.ResponseWriter, r *http.Request) {
		b := s.Board()
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(struct {
			Board
			Now time.Time `json:"now"`
		}{b, time.Now()})
	})
	mux.HandleFunc("/api/flow", func(w http.ResponseWriter, r *http.Request) {
		days, _ := strconv.Atoi(r.URL.Query().Get("days"))
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(s.Flow(days))
	})
	mux.HandleFunc("/api/brief", func(w http.ResponseWriter, r *http.Request) {
		q := r.URL.Query()
		text, pending := s.Brief(q.Get("kind"), q.Get("key"))
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]any{"text": text, "pending": pending, "available": s.briefs != nil})
	})
	mux.HandleFunc("/api/headline", func(w http.ResponseWriter, r *http.Request) {
		text, pending := s.Headline()
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]any{"text": text, "pending": pending, "available": s.briefs != nil})
	})
	mux.HandleFunc("/api/detail", func(w http.ResponseWriter, r *http.Request) {
		q := r.URL.Query()
		lines := s.Detail(q.Get("kind"), q.Get("key"))
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(lines)
	})
	if s.hub != nil {
		s.hub.routes(mux)
	}
	srv := &http.Server{Handler: mux, ReadHeaderTimeout: 5 * time.Second}
	go func() {
		if err := srv.Serve(ln); err != nil && err != http.ErrServerClosed {
			fmt.Fprintln(os.Stderr, "attention: web:", err)
		}
	}()
}

// URL is where the page is served, "" when this process serves none
// and the row has no web address.
func (s *Service) URL() string {
	if s.web == "" {
		return ""
	}
	host, port, err := net.SplitHostPort(s.web)
	if err != nil {
		return ""
	}
	if host == "" {
		host = "localhost"
	}
	return "http://" + net.JoinHostPort(host, port)
}

// openBrowser hands the page to the desktop.
func openBrowser(url string) error {
	cmd := "xdg-open"
	if runtime.GOOS == "darwin" {
		cmd = "open"
	}
	return exec.Command(cmd, url).Start()
}
