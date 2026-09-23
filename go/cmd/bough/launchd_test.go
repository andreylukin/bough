package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestServeAgentPlist(t *testing.T) {
	t.Parallel()
	home := "/Users/a b"
	p := serveAgentPlist(home, "/Users/a b/.local/bin/bough", "127.0.0.1:7684", true, "bough.example")
	for _, want := range []string{
		"<string>com.bough.server</string>",
		"<string>/Users/a b/.local/bin/bough</string>\n    <string>serve</string>\n    <string>--run</string>\n    <string>127.0.0.1:7684</string>\n    <string>--insecure-bind</string>\n    <string>--host=bough.example</string>",
		"<key>KeepAlive</key><true/>",
		"<key>RunAtLoad</key><true/>",
		"<key>WorkingDirectory</key><string>/Users/a b</string>",
		"<string>" + filepath.Join(home, ".bough", "serve.log") + "</string>",
		"<key>PATH</key><string>/Users/a b/.local/bin:",
	} {
		if !strings.Contains(p, want) {
			t.Errorf("plist lacks %q:\n%s", want, p)
		}
	}
	// Only PATH and HOME: a key exported by the installing shell must not ride along.
	if strings.Contains(p, "API_KEY") {
		t.Error("plist carries a key")
	}
	if strings.Contains(serveAgentPlist(home, "/x", "127.0.0.1:1", false, ""), "insecure") {
		t.Error("loopback plist has --insecure-bind")
	}
	if !strings.Contains(serveAgentPlist("/h", "/a&b", "127.0.0.1:1", false, ""), "/a&amp;b") {
		t.Error("bin path is not XML-escaped")
	}
}

func TestServeManaged(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	if serveManaged(home) {
		t.Fatal("managed with no plist")
	}
	if err := os.MkdirAll(filepath.Dir(serveAgentPath(home)), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(serveAgentPath(home), []byte("x"), 0o644); err != nil {
		t.Fatal(err)
	}
	if !serveManaged(home) {
		t.Fatal("not managed with the plist present")
	}
}
