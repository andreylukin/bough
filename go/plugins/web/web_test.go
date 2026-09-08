package web

import (
	"io"
	"net/http"
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
