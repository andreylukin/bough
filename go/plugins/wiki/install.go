package wiki

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"time"
)

const agentID = "com.bough.wiki"

func plistPath(p paths) string {
	return filepath.Join(p.home, "Library", "LaunchAgents", agentID+".plist")
}

func logPath(p paths) string { return filepath.Join(p.wiki, "ingest.log") }

// plist is the launchd agent: `bough wiki run` every interval, from the
// GUI session so the keychain (provider keys) is open.
func plist(p paths, exe string, every time.Duration) string {
	return fmt.Sprintf(`<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>%s</string>
  <key>ProgramArguments</key>
  <array>
    <string>%s</string>
    <string>wiki</string>
    <string>run</string>
  </array>
  <key>StartInterval</key><integer>%d</integer>
  <key>RunAtLoad</key><true/>
  <key>WorkingDirectory</key><string>%s</string>
  <key>StandardOutPath</key><string>%s</string>
  <key>StandardErrorPath</key><string>%s</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>%s:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
    <key>HOME</key><string>%s</string>
  </dict>
</dict>
</plist>
`, agentID, exe, int(every.Seconds()), p.wiki, logPath(p), logPath(p), filepath.Dir(exe), p.home)
}

// install sets the wiki up and schedules `bough wiki run`: a launchd
// agent on macOS, a crontab line to add by hand elsewhere.
func install(p paths, exe string, every time.Duration) error {
	if err := ensureWiki(p); err != nil {
		return err
	}
	if err := writeSkill(p); err != nil {
		return err
	}
	if runtime.GOOS != "darwin" {
		fmt.Printf("wiki at %s, skill at %s\nschedule it with this crontab line:\n*/%d * * * * %s wiki run >> %s 2>&1\n",
			p.wiki, p.skill, max(1, int(every.Minutes())), exe, logPath(p))
		return nil
	}
	path := plistPath(p)
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	_ = exec.Command("launchctl", "unload", path).Run()
	if err := os.WriteFile(path, []byte(plist(p, exe, every)), 0o644); err != nil {
		return err
	}
	if out, err := exec.Command("launchctl", "load", path).CombinedOutput(); err != nil {
		return fmt.Errorf("launchctl load: %v: %s", err, strings.TrimSpace(string(out)))
	}
	fmt.Printf("installed %s: `%s wiki run` every %s\nwiki %s · skill %s · log %s\n", agentID, exe, every, p.wiki, p.skill, logPath(p))
	return nil
}

func uninstall(p paths) error {
	path := plistPath(p)
	if _, err := os.Stat(path); err != nil {
		fmt.Println("not installed")
		return nil
	}
	_ = exec.Command("launchctl", "unload", path).Run()
	if err := os.Remove(path); err != nil {
		return err
	}
	fmt.Println("removed", agentID, "(the wiki itself is kept)")
	return nil
}
