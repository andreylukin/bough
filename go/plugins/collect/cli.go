package collect

import (
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/graph"
)

const (
	usage    = "usage: bough collect [github|linear|slack|notion]… | install [--every 10m] | uninstall"
	agentID  = "com.bough.collect"
	logName  = "collect.log"
	timeLine = "2006-01-02 15:04:05"
)

// Commands implements kernel.Commander: `bough collect …`.
func (plugin) Commands() []kernel.Command {
	return []kernel.Command{{
		Name:    "collect",
		Usage:   "[source…] | install [--every 10m] | uninstall",
		Summary: "pull my PRs, tickets, threads and pages into the memory graph; install runs it under launchd",
		Run:     runCLI,
	}}
}

func runCLI(cfg map[string]any, args []string) error {
	c, err := parseConfig(cfg)
	if err != nil {
		return err
	}
	if len(args) > 0 {
		switch args[0] {
		case "install":
			return install(c, args[1:])
		case "uninstall":
			return uninstall()
		case "-h", "--help", "help":
			fmt.Println(usage)
			return nil
		}
	}
	sources := args
	if len(sources) == 0 {
		sources = []string{"github", "linear", "slack", "notion"}
	}
	for _, s := range sources {
		switch s {
		case "github", "linear", "slack", "notion":
		default:
			return fmt.Errorf("unknown source %q\n%s", s, usage)
		}
	}
	if err := os.MkdirAll(filepath.Dir(graphPath()), 0o755); err != nil {
		return err
	}
	st, err := graph.Open(graphPath())
	if err != nil {
		return err
	}
	defer st.Close()
	st.SetEmbedder(graph.EmbedderFromEnv())
	run, err := NewRun(st, c.Me)
	if err != nil {
		return err
	}
	run.Days = c.Days
	run.Log = func(format string, a ...any) { fmt.Fprintf(os.Stderr, format+"\n", a...) }
	failed := 0
	for _, s := range sources {
		var rep Report
		switch s {
		case "github":
			if !c.Github {
				rep = Report{Source: s, Err: errors.New("off (no gh on PATH, or github: false)")}
			} else {
				rep = run.Github()
			}
		case "linear":
			rep = serverOrOff(s, c.Linear, func() Report { return run.Linear(c.Linear) })
		case "slack":
			rep = serverOrOff(s, c.Slack, func() Report { return run.Slack(c.Slack, c.Queries) })
		case "notion":
			rep = serverOrOff(s, c.Notion, func() Report { return run.Notion(c.Notion) })
		}
		if rep.Err != nil {
			failed++
		}
		fmt.Printf("%s %s\n", time.Now().Format(timeLine), rep)
	}
	if failed == len(sources) {
		return fmt.Errorf("every source failed")
	}
	if w, err := st.WorldOf(run.Me); err == nil && !w.Empty() {
		fmt.Println(w.Render())
	}
	return nil
}

func serverOrOff(source, server string, f func() Report) Report {
	if server == "" {
		return Report{Source: source, Err: errors.New("off")}
	}
	return f()
}

// install writes a launchd agent that runs `bough collect` every
// interval from the GUI session (so the keychain is open) and loads it.
func install(c Config, args []string) error {
	every := c.Every
	for i := 0; i < len(args); i++ {
		if args[i] == "--every" && i+1 < len(args) {
			d, err := time.ParseDuration(args[i+1])
			if err != nil || d < time.Minute {
				return fmt.Errorf("--every: a duration of at least 1m")
			}
			every = d
			i++
		}
	}
	bin, err := os.Executable()
	if err != nil {
		return err
	}
	if p, err := filepath.EvalSymlinks(bin); err == nil {
		bin = p
	}
	logPath := filepath.Join(home(), ".bough", logName)
	plist := fmt.Sprintf(`<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>%s</string>
  <key>ProgramArguments</key>
  <array>
    <string>%s</string>
    <string>collect</string>
  </array>
  <key>StartInterval</key><integer>%d</integer>
  <key>RunAtLoad</key><true/>
  <key>WorkingDirectory</key><string>%s</string>
  <key>StandardOutPath</key><string>%s</string>
  <key>StandardErrorPath</key><string>%s</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
    <key>HOME</key><string>%s</string>
  </dict>
</dict>
</plist>
`, agentID, bin, int(every.Seconds()), home(), logPath, logPath, home())
	path := plistPath()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	_ = exec.Command("launchctl", "unload", path).Run()
	if err := os.WriteFile(path, []byte(plist), 0o644); err != nil {
		return err
	}
	if out, err := exec.Command("launchctl", "load", path).CombinedOutput(); err != nil {
		return fmt.Errorf("launchctl load: %v: %s", err, strings.TrimSpace(string(out)))
	}
	fmt.Printf("installed %s: %s collect every %s, log %s\n", agentID, bin, every, logPath)
	return nil
}

func uninstall() error {
	path := plistPath()
	if _, err := os.Stat(path); err != nil {
		fmt.Println("not installed")
		return nil
	}
	_ = exec.Command("launchctl", "unload", path).Run()
	if err := os.Remove(path); err != nil {
		return err
	}
	fmt.Println("removed", agentID)
	return nil
}

func plistPath() string {
	return filepath.Join(home(), "Library", "LaunchAgents", agentID+".plist")
}
