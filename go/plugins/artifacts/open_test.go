package artifacts

import (
	"errors"
	"strings"
	"testing"
)

// A browser that will not open leaves the URL in the result, and
// /artifacts open reopens the latest page after Publish already did.
func TestOpenFailureAndReopen(t *testing.T) {
	s := newStore(t)
	s.open = func(string) error { return errors.New("no display") }
	got, err := s.Publish("p", sample)
	url := s.web.URL() + "/artifacts/s1/p"
	if err != nil || !strings.HasPrefix(got, url+"\n") || !strings.Contains(got, "no display") {
		t.Fatalf("publish with a failing browser: %q %v", got, err)
	}
	if out := s.openLatest(); !strings.Contains(out, "no display") || !strings.HasSuffix(out, url) {
		t.Fatalf("openLatest failing: %q", out)
	}
	n := 0
	s.open = func(string) error { n++; return nil }
	if out := s.openLatest(); out != "opened "+url || n != 1 {
		t.Fatalf("reopen: %q, %d opens", out, n)
	}
	// A new process (a fresh Store over the same files) opens again on
	// its first publish of the name.
	s2 := &Store{root: s.root, session: "s1", web: s.web, opened: map[string]bool{}, seen: map[string]int{}, errSeen: map[string]string{}, open: s.open}
	if _, err := s2.Publish("p", sample); err != nil || n != 2 {
		t.Fatalf("publish after restart: %v, %d opens", err, n)
	}
}
