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
	"syscall"
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
	root, err := findCheckout(exe, os.Getenv("BOUGH_ROOT"), home)
	if err != nil {
		fatal(err)
	}
	modDir, err := moduleDir(root)
	if err != nil {
		fatal(err)
	}
	target, installed := installTarget(exe, root, modDir)

	step("pull", root, "git", "pull", "--ff-only")
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
	return "", fmt.Errorf("cannot find a bough checkout (%s)", strings.Join(tried, "; "))
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
	pid, addr, perr := parsePidfile(string(b))
	if perr != nil || !alive(pid) {
		os.Remove(pf)
		fmt.Fprintln(out, noWebMsg)
		return nil
	}

	fmt.Fprintf(out, "bough: stopping web session (pid %d)…\n", pid)
	if err := syscall.Kill(pid, syscall.SIGINT); err != nil {
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

	logPath := filepath.Join(home, ".bough", "web.log")
	logF, err := os.OpenFile(logPath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return fmt.Errorf("open %s: %w", logPath, err)
	}
	defer logF.Close()
	cmd := exec.Command(bin, "--web", addr)
	cmd.Stdout = logF
	cmd.Stderr = logF
	cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true}
	if err := cmd.Start(); err != nil {
		return fmt.Errorf("relaunch %s --web %s: %w", bin, addr, err)
	}
	fmt.Fprintf(out, "bough: restarted web session on %s (pid %d, log %s)\n", addr, cmd.Process.Pid, logPath)
	return nil
}

func webPidfile(home string) string {
	return filepath.Join(home, ".bough", "web.pid")
}

func parsePidfile(s string) (pid int, addr string, err error) {
	fields := strings.Fields(s)
	if len(fields) != 2 {
		return 0, "", fmt.Errorf("malformed pidfile: %q", s)
	}
	if _, err := fmt.Sscanf(fields[0], "%d", &pid); err != nil || pid <= 0 {
		return 0, "", fmt.Errorf("malformed pidfile pid: %q", fields[0])
	}
	return pid, fields[1], nil
}

// alive is a signal-0 liveness probe. pid must be positive — never
// signal 0/negative (process groups).
func alive(pid int) bool {
	if pid <= 0 {
		return false
	}
	err := syscall.Kill(pid, 0)
	return err == nil || err == syscall.EPERM
}

// writeWebPidfile records "<pid> <addr>" for restart to find.
// Best-effort: a failure warns and the session runs on. The returned
// cleanup (nil on failure) removes the file on clean shutdown.
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
	if err := os.WriteFile(pf, []byte(fmt.Sprintf("%d %s\n", os.Getpid(), addr)), 0o644); err != nil {
		fmt.Fprintln(os.Stderr, "bough: web pidfile:", err)
		return nil
	}
	return func() { os.Remove(pf) }
}
