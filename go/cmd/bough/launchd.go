// The control room under launchd. On macOS a detached `bough serve`
// dies with the login session and never comes back on its own, and a
// serve started from an ssh shell inherits a locked keychain (its MCP
// tokens unreadable). A LaunchAgent in the gui domain fixes both: it
// starts at login, restarts after a crash or an update, and runs in the
// user's own session. `bough serve` installs it, `bough serve stop`
// removes it, `bough update` and `bough restart` refresh it.
package main

import (
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"runtime"
	"strings"
)

const serveAgentID = "com.bough.server"

// launchdServes reports whether this host runs the control room under
// launchd: macOS, and not a test's fake home.
func launchdServes() bool { return runtime.GOOS == "darwin" && os.Getenv("BOUGH_NO_LAUNCHD") == "" }

func serveAgentPath(home string) string {
	return filepath.Join(home, "Library", "LaunchAgents", serveAgentID+".plist")
}

// serveManaged reports whether the agent is installed for home.
func serveManaged(home string) bool {
	_, err := os.Stat(serveAgentPath(home))
	return err == nil
}

// serveAgentPlist is the agent: `bin serve --run addr` from home, kept
// alive, logging where the detached daemon logged. Only PATH and HOME
// reach it: keys come from ~/.bough/env, never from the shell that
// happened to install it.
func serveAgentPlist(home, bin, addr string, insecure bool, host string) string {
	args := []string{bin, "serve", "--run", addr}
	if insecure {
		args = append(args, "--insecure-bind")
	}
	if host != "" {
		args = append(args, "--host="+host)
	}
	var b strings.Builder
	b.WriteString(`<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>` + serveAgentID + `</string>
  <key>ProgramArguments</key>
  <array>
`)
	for _, a := range args {
		b.WriteString("    <string>" + xmlEscape(a) + "</string>\n")
	}
	b.WriteString(`  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>WorkingDirectory</key><string>` + xmlEscape(home) + `</string>
  <key>StandardOutPath</key><string>` + xmlEscape(filepath.Join(home, ".bough", "serve.log")) + `</string>
  <key>StandardErrorPath</key><string>` + xmlEscape(filepath.Join(home, ".bough", "serve.log")) + `</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>` + xmlEscape(filepath.Dir(bin)) + `:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
    <key>HOME</key><string>` + xmlEscape(home) + `</string>
  </dict>
</dict>
</plist>
`)
	return b.String()
}

func xmlEscape(s string) string {
	return strings.NewReplacer("&", "&amp;", "<", "&lt;", ">", "&gt;").Replace(s)
}

func launchdDomain() string { return fmt.Sprintf("gui/%d", os.Getuid()) }

func launchctl(args ...string) error {
	out, err := exec.Command("launchctl", args...).CombinedOutput()
	if err != nil {
		return fmt.Errorf("launchctl %s: %v: %s", strings.Join(args, " "), err, strings.TrimSpace(string(out)))
	}
	return nil
}

// installServeAgent writes the agent for bin and (re)loads it, which
// starts serve if it is not running and restarts it if it is: after an
// update the job must run the new binary, and launchd only re-reads a
// plist on bootstrap.
func installServeAgent(home, bin, addr string, insecure bool, host string) error {
	path := serveAgentPath(home)
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return fmt.Errorf("serve: %w", err)
	}
	if err := os.MkdirAll(filepath.Join(home, ".bough"), 0o755); err != nil {
		return fmt.Errorf("serve: %w", err)
	}
	if err := os.WriteFile(path, []byte(serveAgentPlist(home, bin, addr, insecure, host)), 0o644); err != nil {
		return fmt.Errorf("serve: write %s: %w", path, err)
	}
	_ = launchctl("bootout", launchdDomain()+"/"+serveAgentID) // not loaded is fine
	if err := launchctl("bootstrap", launchdDomain(), path); err != nil {
		return fmt.Errorf("serve: %w", err)
	}
	return nil
}

// removeServeAgent unloads the agent (launchd stops serve with SIGTERM,
// which it handles like a stop) and deletes the plist, so a stopped
// control room stays stopped through the next login.
func removeServeAgent(home string) error {
	err := launchctl("bootout", launchdDomain()+"/"+serveAgentID)
	if rmErr := os.Remove(serveAgentPath(home)); rmErr != nil && !os.IsNotExist(rmErr) {
		return fmt.Errorf("serve: %w", rmErr)
	}
	return err
}

var wikiInterval = regexp.MustCompile(`<key>StartInterval</key><integer>(\d+)</integer>`)

// refreshWikiAgent re-runs `bough wiki install` on bin when the wiki's
// launchd agent is installed, keeping its interval. The agent names the
// binary that installed it and launchd reads a plist only on load, so
// an update left the ingest on whatever it was loaded with; on the work
// laptop the job sat in "spawn failed" (EX_CONFIG) for hours after a
// rebuild, sessions piling up unread. A failure warns: the update
// itself succeeded.
func refreshWikiAgent(out io.Writer, home, bin string, run func(string, ...string) error) {
	b, err := os.ReadFile(filepath.Join(home, "Library", "LaunchAgents", "com.bough.wiki.plist"))
	if err != nil {
		return // not installed
	}
	args := []string{"wiki", "install"}
	if m := wikiInterval.FindSubmatch(b); m != nil {
		args = append(args, "--every", string(m[1])+"s")
	}
	if err := run(bin, args...); err != nil {
		fmt.Fprintf(out, "bough: wiki agent: %v (rerun `bough wiki install`)\n", err)
		return
	}
	fmt.Fprintf(out, "bough: reloaded wiki ingest agent on %s\n", bin)
}
