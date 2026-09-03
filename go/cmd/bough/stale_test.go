package main

import (
	"strings"
	"testing"
	"time"
)

func TestStaleness(t *testing.T) {
	t.Parallel()
	now := time.Now()
	if got := staleness("abcdef1234567890", "abcdef1234567890", now, now.Add(-time.Hour)); got != "" {
		t.Fatalf("same revision, older sources: want current, got %q", got)
	}
	if got := staleness("abcdef12", "abcdef1234567890", now, now); got != "" {
		t.Fatalf("short revision prefix of head: want current, got %q", got)
	}
	got := staleness("1111111111111111", "2222222222222222", now, now.Add(-time.Hour))
	if !strings.Contains(got, "built from 11111111") || !strings.Contains(got, "checkout is at 22222222") || !strings.Contains(got, "bough update") {
		t.Fatalf("revision mismatch notice = %q", got)
	}
	if got := staleness("abc", "abc", now, now.Add(time.Minute)); !strings.Contains(got, "sources are newer") {
		t.Fatalf("newer sources notice = %q", got)
	}
	if got := staleness("", "", now, now.Add(time.Minute)); !strings.Contains(got, "sources are newer") {
		t.Fatalf("unknown revisions still use mtimes, got %q", got)
	}
	if got := staleness("", "abc", time.Time{}, now); got != "" {
		t.Fatalf("unknown times: want current, got %q", got)
	}
}
