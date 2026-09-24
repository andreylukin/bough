package main

import (
	"bytes"
	"errors"
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

func TestRefreshWikiAgent(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	var calls []string
	run := func(name string, args ...string) error {
		calls = append(calls, name+" "+strings.Join(args, " "))
		return nil
	}
	var out bytes.Buffer
	refreshWikiAgent(&out, home, "/opt/bough", run)
	if len(calls) != 0 || out.Len() != 0 {
		t.Fatalf("no agent installed: ran %q, said %q", calls, out.String())
	}

	plist := filepath.Join(home, "Library", "LaunchAgents", "com.bough.wiki.plist")
	mkdirs(t, filepath.Dir(plist))
	if err := os.WriteFile(plist, []byte("<key>StartInterval</key><integer>600</integer>"), 0o644); err != nil {
		t.Fatal(err)
	}
	refreshWikiAgent(&out, home, "/opt/bough", run)
	if want := []string{"/opt/bough wiki install --every 600s"}; strings.Join(calls, "|") != strings.Join(want, "|") {
		t.Fatalf("ran %q, want %q", calls, want)
	}
	if !strings.Contains(out.String(), "reloaded wiki ingest agent on /opt/bough") {
		t.Errorf("said %q", out.String())
	}

	out.Reset()
	refreshWikiAgent(&out, home, "/opt/bough", func(string, ...string) error { return errors.New("launchctl load: boom") })
	if !strings.Contains(out.String(), "boom") || !strings.Contains(out.String(), "bough wiki install") {
		t.Errorf("failure said %q, want the error and the fix", out.String())
	}
}
