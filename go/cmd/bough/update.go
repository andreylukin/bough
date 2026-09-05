// bough update: pull the checkout, rebuild this binary, bounce the web
// session. bough restart: bounce it alone. The web session is found via
// $HOME/.bough/web.pid ("<pid> <addr>"), written by --web on startup.
package main

import (
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"
)

// runUpdate is `bough update`: git pull --ff-only in the checkout,
// go build over the current binary, then the restart logic in-process.
func runUpdate(args []string) {
	if len(args) > 0 {
		fatal(fmt.Errorf("update takes no arguments, got %v", args))
	}
	exe := resolveExe()
	home, err := os.UserHomeDir()
	if err != nil {
		fatal(fmt.Errorf("home dir: %w", err))
	}
	// Your own checkout wins: that is your tree and your branch, and
	// update pulls it as it always did. Without one, build the newest
	// commit on main from a clone bough manages (source.go) rather than
	// sending you to the last tagged release.
	root, err := findCheckout(exe, os.Getenv("BOUGH_ROOT"), home)
	fromMain := false
	if err != nil {
		root, err = fetchMain(home)
		if err != nil {
			fatal(err)
		}
		fromMain = true
	}
	modDir, err := moduleDir(root)
	if err != nil {
		fatal(err)
	}
	target, installed := installTarget(exe, root, modDir)

	was := versionString()
	if !fromMain {
		step("pull", root, "git", "pull", "--ff-only")
	}
	if head := gitHead(root); head != "" {
		where := "checkout"
		if fromMain {
			where = "main"
		}
		fmt.Printf("bough: %s at %s (this binary was built from %s)\n", where, short(head), was)
	}
	if installed {
		// Overwrite the installed copy atomically: build next to it,
		// then rename (same dir, so same filesystem).
		tmp := filepath.Join(filepath.Dir(target), fmt.Sprintf(".bough-update-%d", os.Getpid()))
		step("build", modDir, "go", "build", "-o", tmp, "./cmd/bough")
		if err := os.Rename(tmp, target); err != nil {
			os.Remove(tmp)
			fatal(fmt.Errorf("install %s: %w", target, err))
		}
	} else {
		step("build", modDir, "go", "build", "-o", target, "./cmd/bough")
	}
	fmt.Printf("bough: installed %s\n", target)

	if err := restartWeb(home, target, os.Stdout); err != nil {
		fatal(err)
	}
}

// runRestart is `bough restart`: bounce the running --web session (if
// any) onto the current binary.
func runRestart(args []string) {
	if len(args) > 0 {
		fatal(fmt.Errorf("restart takes no arguments, got %v", args))
	}
	home, err := os.UserHomeDir()
	if err != nil {
		fatal(fmt.Errorf("home dir: %w", err))
	}
	if err := restartWeb(home, resolveExe(), os.Stdout); err != nil {
		fatal(err)
	}
}

// resolveExe is the current executable with symlinks resolved (so a
// ~/.local/bin/bough symlink counts as wherever it points).
func resolveExe() string {
	exe, err := os.Executable()
	if err != nil {
		fatal(fmt.Errorf("locate executable: %w", err))
	}
	if r, err := filepath.EvalSymlinks(exe); err == nil {
		exe = r
	}
	return exe
}

// findCheckout locates the bough git checkout: walk up from the
// resolved executable looking for .git, then $BOUGH_ROOT, then
// <home>/repos/bough. Fails naming everything tried.
func findCheckout(exe, boughRoot, home string) (string, error) {
	var tried []string
	for dir := filepath.Dir(exe); ; {
		if hasGit(dir) {
			return dir, nil
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			break
		}
		dir = parent
	}
	tried = append(tried, "no .git above "+exe)
	if boughRoot != "" {
		if hasGit(boughRoot) {
			return boughRoot, nil
		}
		return "", fmt.Errorf("BOUGH_ROOT=%s has no .git", boughRoot)
	}
	tried = append(tried, "$BOUGH_ROOT unset")
	def := filepath.Join(home, "repos", "bough")
	if hasGit(def) {
		return def, nil
	}
	tried = append(tried, def+" has no .git")
	// No checkout is not an error any more: runUpdate builds main from
	// its own clone instead (source.go). The trail is still returned so
	// a caller that wanted a real checkout can say what it looked for.
	return "", fmt.Errorf("no bough checkout found (searched: %s)", strings.Join(tried, "; "))
}

func hasGit(dir string) bool {
	_, err := os.Stat(filepath.Join(dir, ".git"))
	return err == nil
}

// moduleDir is where go.mod lives: <root>/go (post-merge layout with
// the Rust repo) or <root> itself.
func moduleDir(root string) (string, error) {
	sub := filepath.Join(root, "go")
	if _, err := os.Stat(filepath.Join(sub, "go.mod")); err == nil {
		return sub, nil
	}
	if _, err := os.Stat(filepath.Join(root, "go.mod")); err == nil {
		return root, nil
	}
	return "", fmt.Errorf("no go.mod in %s or %s", root, sub)
}

// installTarget decides where the build lands: an executable outside
// the checkout's target/ and module dir is an installed copy —
// overwrite it in place (installed=true, via temp+rename); otherwise
// build to <modDir>/bough.
func installTarget(exe, root, modDir string) (string, bool) {
	if within(filepath.Join(root, "target"), exe) || within(modDir, exe) {
		return filepath.Join(modDir, "bough"), false
	}
	return exe, true
}

// within reports whether path is inside dir.
func within(dir, path string) bool {
	rel, err := filepath.Rel(dir, path)
	return err == nil && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator))
}

// step runs one labeled update command, streaming output, failing loud.
func step(name, dir string, argv ...string) {
	fmt.Printf("bough: %s (%s)…\n", name, dir)
	cmd := exec.Command(argv[0], argv[1:]...)
	cmd.Dir = dir
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	if err := cmd.Run(); err != nil {
		fatal(fmt.Errorf("%s: %s: %w", name, strings.Join(argv, " "), err))
	}
}

const noWebMsg = "bough: no running web session; sessions pick up the new binary on next launch"

// restartWeb bounces the web session recorded in <home>/.bough/web.pid:
// SIGINT, wait up to 10s, relaunch bin --web <addr> detached. No
// pidfile, a stale one (dead pid — removed), or an unparseable one
// prints noWebMsg and succeeds. Only a pid this file recorded is ever
// signaled.
func restartWeb(home, bin string, out io.Writer) error {
	pf := webPidfile(home)
	b, err := os.ReadFile(pf)
	if err != nil {
		fmt.Fprintln(out, noWebMsg)
		return nil
	}
	pid, addr, _, _, _, perr := parsePidfile(string(b))
	if perr != nil || !alive(pid) {
		os.Remove(pf)
		fmt.Fprintln(out, noWebMsg)
		return nil
	}

	fmt.Fprintf(out, "bough: stopping web session (pid %d)…\n", pid)
	if err := interrupt(pid); err != nil {
		return fmt.Errorf("signal pid %d: %w", pid, err)
	}
	deadline := time.Now().Add(10 * time.Second)
	for alive(pid) {
		if time.Now().After(deadline) {
			return fmt.Errorf("web session (pid %d) did not exit within 10s", pid)
		}
		time.Sleep(50 * time.Millisecond)
	}
	os.Remove(pf) // best-effort; the exiting process usually removed it

	pid, logPath, err := launchWeb(home, bin, addr)
	if err != nil {
		return err
	}
	fmt.Fprintf(out, "bough: restarted web session on %s (pid %d, log %s)\n", addr, pid, logPath)
	return nil
}

// launchWeb starts `bin --web addr` detached (own session, output to
// ~/.bough/web.log) and returns its pid and log path.
func launchWeb(home, bin, addr string) (int, string, error) {
	logPath := filepath.Join(home, ".bough", "web.log")
	if err := os.MkdirAll(filepath.Dir(logPath), 0o755); err != nil {
		return 0, "", err
	}
	logF, err := os.OpenFile(logPath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return 0, "", fmt.Errorf("open %s: %w", logPath, err)
	}
	defer logF.Close()
	cmd := exec.Command(bin, "--web", addr)
	cmd.Stdout = logF
	cmd.Stderr = logF
	detach(cmd)
	if err := cmd.Start(); err != nil {
		return 0, "", fmt.Errorf("launch %s --web %s: %w", bin, addr, err)
	}
	return cmd.Process.Pid, logPath, nil
}

// webSession is the live detached session: where it runs and what it
// runs on, not just where to point a browser.
type webSession struct {
	pid    int
	addr   string
	dir    string // the cwd it was started in — which bough.yml it found
	config string
	caps   string // what the running binary understands, comma separated
}

// canNewSession reports whether the running session handles SIGUSR1 as
// "start a new session". A bough that predates it takes SIGUSR1's
// default disposition instead, which is to DIE — so an old session is
// left alone and the user is told to restart it.
func (w webSession) canNewSession() bool {
	for _, c := range strings.Split(w.caps, ",") {
		if c == capNewSession {
			return true
		}
	}
	return false
}

// where renders the session's directory and config for a person
// deciding whether it is the session they meant.
func (w webSession) where() string {
	if w.dir == "" {
		return ""
	}
	s := "in " + w.dir
	if w.config != "" {
		s += " (config " + w.config + ")"
	}
	return s
}

// runningWeb reports the live web session recorded in the pidfile, if
// any; a stale pidfile is removed.
func runningWeb(home string) (webSession, bool) {
	pf := webPidfile(home)
	b, err := os.ReadFile(pf)
	if err != nil {
		return webSession{}, false
	}
	pid, addr, dir, config, caps, perr := parsePidfile(string(b))
	if perr != nil || !alive(pid) {
		os.Remove(pf)
		return webSession{}, false
	}
	return webSession{pid: pid, addr: addr, dir: dir, config: config, caps: caps}, true
}

func webPidfile(home string) string {
	return filepath.Join(home, ".bough", "web.pid")
}

// parsePidfile reads "<pid> <addr>[\t<cwd>\t<config>]" — tabs, because
// a path may contain spaces. The trailing two
// are what a detached session is actually attached to: without them
// `bough web` in one directory silently hands you the session someone
// started in another, running that directory's bough.yml.
func parsePidfile(s string) (pid int, addr, dir, config, caps string, err error) {
	parts := strings.Split(strings.TrimRight(s, "\n"), "\t")
	head := strings.Fields(parts[0])
	if len(head) != 2 {
		return 0, "", "", "", "", fmt.Errorf("malformed pidfile: %q", s)
	}
	if _, err := fmt.Sscanf(head[0], "%d", &pid); err != nil || pid <= 0 {
		return 0, "", "", "", "", fmt.Errorf("malformed pidfile pid: %q", head[0])
	}
	if len(parts) > 1 {
		dir = parts[1]
	}
	if len(parts) > 2 {
		config = parts[2]
	}
	if len(parts) > 3 {
		caps = parts[3]
	}
	return pid, head[1], dir, config, caps, nil
}

// alive is a signal-0 liveness probe. pid must be positive — never
// signal 0/negative (process groups).
// writeWebPidfile records "<pid> <addr>" for restart to find.
// Best-effort: a failure warns and the session runs on. The returned
// cleanup (nil on failure) removes the file on clean shutdown.
// webConfig is the config path the web session resolved, recorded in
// the pidfile so `bough web` can say which one is in force.
var webConfig = "(embedded)"

// capNewSession marks a running session that understands SIGUSR1.
const capNewSession = "new-session"

func writeWebPidfile(addr string) func() {
	home, err := os.UserHomeDir()
	if err != nil {
		fmt.Fprintln(os.Stderr, "bough: web pidfile:", err)
		return nil
	}
	pf := webPidfile(home)
	if err := os.MkdirAll(filepath.Dir(pf), 0o755); err != nil {
		fmt.Fprintln(os.Stderr, "bough: web pidfile:", err)
		return nil
	}
	dir, _ := os.Getwd()
	if err := os.WriteFile(pf, fmt.Appendf(nil, "%d %s\t%s\t%s\t%s\n",
		os.Getpid(), addr, dir, webConfig, capNewSession), 0o644); err != nil {
		fmt.Fprintln(os.Stderr, "bough: web pidfile:", err)
		return nil
	}
	return func() { os.Remove(pf) }
}
