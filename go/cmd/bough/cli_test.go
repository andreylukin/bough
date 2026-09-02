package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// A first arg that is neither a flag nor a subcommand is an error, so
// a typo never falls through into the TUI.
func TestCommandDispatch(t *testing.T) {
	if name, rest, err := command([]string{"log", "--raw"}); err != nil || name != "log" || len(rest) != 1 {
		t.Fatalf("log: %q %v %v", name, rest, err)
	}
	if name, rest, err := command([]string{"--headless"}); err != nil || name != "" || len(rest) != 1 {
		t.Fatalf("flags only: %q %v %v", name, rest, err)
	}
	if name, _, err := command(nil); err != nil || name != "" {
		t.Fatalf("no args: %q %v", name, err)
	}
	_, _, err := command([]string{"bogus"})
	if err == nil || err.Error() != "unknown command: bogus (try --help)" {
		t.Fatalf("bogus: %v", err)
	}
}

func TestUsageListsFlagsCommandsConfig(t *testing.T) {
	for _, want := range []string{
		"-c, --continue", "-r, --resume [id]", "--set", "--headless", "--web",
		"--version", "--verbose",
		"rows", "sessions", "log", "update", "restart",
		"./bough.yml", "~/.bough/bough.yml", "~/.bough/init.js",
	} {
		if !strings.Contains(usageText, want) {
			t.Errorf("usage lacks %q", want)
		}
	}
}

func TestVersionString(t *testing.T) {
	old := version
	defer func() { version = old }()
	version = "v1.2.3"
	if got := versionString(); got != "v1.2.3" {
		t.Fatalf("ldflags version: %q", got)
	}
	version = ""
	if got := versionString(); got == "" {
		t.Fatal("fallback version is empty")
	}
}

// latestSession skips empty session files (left behind by `bough rows`
// or an aborted launch) and returns the newest one with entries.
func TestLatestSessionSkipsEmpty(t *testing.T) {
	dir := t.TempDir()
	write := func(name, body string) string {
		p := filepath.Join(dir, name)
		if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
		return p
	}
	if _, err := latestSessionIn(dir, "/nowhere"); err == nil {
		t.Fatal("empty dir: want error")
	}
	full := write("2026-01-01T00-00-00.jsonl", `{"seq":1,"at":"2026-01-01T00:00:00Z","kind":"input","data":{"text":"hi"}}`+"\n")
	empty := write("2026-01-02T00-00-00.jsonl", "")
	if err := os.Chtimes(empty, time.Now().Add(time.Hour), time.Now().Add(time.Hour)); err != nil {
		t.Fatal(err)
	}
	got, err := latestSessionIn(dir, "/nowhere")
	if err != nil {
		t.Fatal(err)
	}
	if got != full {
		t.Fatalf("latest = %s, want %s (the non-empty one)", got, full)
	}
}
