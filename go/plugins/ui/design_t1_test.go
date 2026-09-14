package ui

import (
	"strings"
	"testing"
)

func TestBangExit(t *testing.T) {
	t.Parallel()
	for text, want := range map[string]string{
		"…":                         "running",
		"main.go":                   "exit 0",
		"(no output)":               "exit 0",
		"oops\n! exit status 2":     "exit 2",
		"! cancelled":               "cancelled",
		"x\n! timeout after 1m0s":   "timeout",
		"! exec: \"sh\": not found": "failed",
	} {
		if got := bangExit(text); got != want {
			t.Errorf("bangExit(%q) = %q, want %q", text, got, want)
		}
	}
}

func TestElideMiddleKeepsSegmentsWhole(t *testing.T) {
	t.Parallel()
	short := "~/repos/bough"
	if got := elideMiddle(short, 40); got != short {
		t.Errorf("a short path must stay whole, got %q", got)
	}
	uuid := "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0"
	long := "~/.bough/scratch/sessions/" + uuid
	got := elideMiddle(long, 40)
	if !strings.HasSuffix(got, "/"+uuid) || !strings.Contains(got, "…") {
		t.Errorf("elideMiddle(%q) = %q: want the middle cut and the uuid whole", long, got)
	}
}

func TestShortIDAndPluralWord(t *testing.T) {
	t.Parallel()
	if got := shortID("a1b2c3d4e5f6"); got != "c3d4e5f6" {
		t.Errorf("shortID = %q", got)
	}
	if pluralWord(1, "entry", "entries") != "entry" || pluralWord(2, "entry", "entries") != "entries" {
		t.Error("pluralWord")
	}
	if plural(1, "session") != "1 session" || plural(3, "session") != "3 sessions" {
		t.Error("plural")
	}
}
