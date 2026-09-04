package main

import "testing"

func TestWebArgs(t *testing.T) {
	cases := []struct {
		in         []string
		verb, addr string
		bad        bool
	}{
		{nil, "start", "localhost:7681", false},
		{[]string{"start"}, "start", "localhost:7681", false},
		{[]string{"0.0.0.0:8080"}, "start", "0.0.0.0:8080", false},
		{[]string{"9000"}, "start", "localhost:9000", false},
		{[]string{"status"}, "status", "", false},
		{[]string{"stop"}, "stop", "", false},
		{[]string{"--port"}, "", "", true},
		{[]string{"nonsense"}, "", "", true},
		{[]string{"a", "b"}, "", "", true},
	}
	for _, c := range cases {
		verb, addr, err := webArgs(c.in)
		if (err != nil) != c.bad || verb != c.verb || addr != c.addr {
			t.Errorf("webArgs(%v) = %q %q %v, want %q %q bad=%v", c.in, verb, addr, err, c.verb, c.addr, c.bad)
		}
	}
	if got := webURL("0.0.0.0:7681"); got != "http://localhost:7681" {
		t.Errorf("webURL wildcard: %s", got)
	}
	if got := webURL("[::1]:7681"); got != "http://[::1]:7681" {
		t.Errorf("webURL v6: %s", got)
	}
}

// The pidfile carries where the session runs and what it runs on: a
// `bough web` in another directory silently hands you the session
// someone started elsewhere, on that directory's config.
func TestPidfileCarriesDirAndConfig(t *testing.T) {
	pid, addr, dir, config, caps, err := parsePidfile("4242 localhost:7681\t/Users/a/repos/my code\t/Users/a/.bough/bough.yml\tnew-session\n")
	if err != nil {
		t.Fatal(err)
	}
	if pid != 4242 || addr != "localhost:7681" {
		t.Fatalf("pid=%d addr=%q", pid, addr)
	}
	if dir != "/Users/a/repos/my code" {
		t.Fatalf("a space in the path did not survive: %q", dir)
	}
	if config != "/Users/a/.bough/bough.yml" {
		t.Fatalf("config = %q", config)
	}
	// The capability marker: SIGUSR1 kills a bough that predates it,
	// so `bough web` only asks a session that says it understands.
	if !(webSession{caps: caps}).canNewSession() {
		t.Fatalf("caps = %q", caps)
	}
	if (webSession{caps: ""}).canNewSession() {
		t.Fatal("an old pidfile must not be signalled")
	}

	// An old two-field pidfile still parses (no dir, no config).
	pid, addr, dir, config, caps, err = parsePidfile("7 localhost:1\n")
	if err != nil || pid != 7 || addr != "localhost:1" || dir != "" || config != "" || caps != "" {
		t.Fatalf("legacy pidfile: %d %q %q %q %q %v", pid, addr, dir, config, caps, err)
	}
	if _, _, _, _, _, err := parsePidfile("nonsense"); err == nil {
		t.Fatal("a malformed pidfile must error")
	}
}

// where() is what the "already running" line prints.
func TestWebSessionWhere(t *testing.T) {
	if got := (webSession{dir: "/x", config: "/y.yml"}).where(); got != "in /x (config /y.yml)" {
		t.Fatalf("where = %q", got)
	}
	if got := (webSession{}).where(); got != "" {
		t.Fatalf("an old pidfile has nothing to say, got %q", got)
	}
}
