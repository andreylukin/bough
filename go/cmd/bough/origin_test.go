package main

import (
	"os"
	"testing"
)

func TestSessionOrigin(t *testing.T) {
	t.Setenv("BOUGH_ORIGIN", "")
	for mode, want := range map[string]string{"tui": "tui", "headless": "headless", "web:127.0.0.1:1": "web"} {
		if got := sessionOrigin(mode); got != want {
			t.Errorf("sessionOrigin(%q) = %q, want %q", mode, got, want)
		}
	}
	t.Setenv("BOUGH_ORIGIN", "web")
	if got := sessionOrigin("headless"); got != "web" {
		t.Errorf("with BOUGH_ORIGIN=web: %q", got)
	}
	if _, set := os.LookupEnv("BOUGH_ORIGIN"); set {
		t.Error("BOUGH_ORIGIN must be cleared so the agent's own bough runs are not the user's")
	}
}
