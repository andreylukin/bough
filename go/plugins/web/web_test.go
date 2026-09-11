package web

import (
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// Rows mount routes on the shared mux and the page is served at URL().
func TestMountedRouteIsServed(t *testing.T) {
	s := New("127.0.0.1:0")
	if !s.Serving() {
		t.Fatal("should hold the listener")
	}
	s.Handle("/hello", http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { io.WriteString(w, "hi") }))
	r, err := http.Get(s.URL() + "/hello")
	if err != nil {
		t.Fatal(err)
	}
	defer r.Body.Close()
	b, _ := io.ReadAll(r.Body)
	if string(b) != "hi" {
		t.Fatalf("body = %q", b)
	}
	if !strings.HasPrefix(s.URL(), "http://127.0.0.1:") || strings.HasSuffix(s.URL(), ":0") {
		t.Fatalf("URL = %q", s.URL())
	}
	// A remount with the same config reuses the listener; a row that
	// unmounts and mounts again does not panic the mux.
	if again := New("127.0.0.1:0"); again != s {
		t.Fatal("remount should reuse the server")
	}
	s.Unhandle("/hello")
	s.Handle("/hello", http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { io.WriteString(w, "again") }))
	r, _ = http.Get(s.URL() + "/hello")
	b, _ = io.ReadAll(r.Body)
	r.Body.Close()
	if string(b) != "again" {
		t.Fatalf("after remount = %q", b)
	}
}

// When a peer holds the port, this process still knows the URL.
func TestTakenPortStillHasURL(t *testing.T) {
	first := New("127.0.0.1:0")
	shared = nil
	second := New(first.addr)
	if second.Serving() {
		t.Fatal("port was taken")
	}
	if second.URL() != first.URL() {
		t.Fatalf("URL %q vs %q", second.URL(), first.URL())
	}
}

func peer(t *testing.T, h http.Handler) string {
	t.Helper()
	p := httptest.NewServer(h)
	t.Cleanup(p.Close)
	return p.Listener.Addr().String()
}

func identity(id Identity) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == identityPath {
			json.NewEncoder(w).Encode(id)
			return
		}
		http.NotFound(w, r)
	})
}

// A same-build peer with our $HOME keeps the port; we share it quietly.
func TestSameBuildPeerIsShared(t *testing.T) {
	addr := peer(t, identity(self()))
	shared = nil
	s := New(addr)
	if s.Serving() || s.Notice() != "" || s.URL() != "http://"+addr {
		t.Fatalf("serving %v notice %q url %q", s.Serving(), s.Notice(), s.URL())
	}
}

// A stale build, another $HOME, or a non-bough listener: serve our own
// port, and the URL and the notice follow it.
func TestUntrustedPeerGetsOwnPort(t *testing.T) {
	me := self()
	for name, h := range map[string]http.Handler{
		"other build": identity(Identity{Build: "old", Home: me.Home, PID: 1}),
		"other home":  identity(Identity{Build: me.Build, Home: "/tmp/x", PID: 1}),
		"not bough":   http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { io.WriteString(w, "hello") }),
	} {
		addr := peer(t, h)
		shared = nil
		s := New(addr)
		if !s.Serving() || s.URL() == "http://"+addr || !strings.Contains(s.Notice(), s.URL()) {
			t.Fatalf("%s: serving %v url %q notice %q", name, s.Serving(), s.URL(), s.Notice())
		}
		s.Handle("/x", http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { io.WriteString(w, "mine") }))
		r, err := http.Get(s.URL() + "/x")
		if err != nil {
			t.Fatal(err)
		}
		b, _ := io.ReadAll(r.Body)
		r.Body.Close()
		if string(b) != "mine" {
			t.Fatalf("%s: body %q", name, b)
		}
	}
}
