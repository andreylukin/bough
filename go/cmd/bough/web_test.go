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
