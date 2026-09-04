// bough web [addr]: start the browser UI as a detached session (or
// report the one already running) and open it. bough web status /
// stop read and end that session via the same pidfile restart uses.
package main

import (
	"fmt"
	"net"
	"os"
	"os/exec"
	"runtime"
	"strings"
	"syscall"
	"time"
)

const defaultWebAddr = "localhost:7681"

// webArgs parses `bough web` arguments: no args or an address starts
// (or attaches to) a session; "status" and "stop" are their own verbs.
// Pure.
func webArgs(args []string) (verb, addr string, err error) {
	switch {
	case len(args) == 0:
		return "start", defaultWebAddr, nil
	case len(args) > 1:
		return "", "", fmt.Errorf("web takes at most one argument, got %v", args)
	}
	switch args[0] {
	case "status", "stop":
		return args[0], "", nil
	case "start":
		return "start", defaultWebAddr, nil
	}
	a := args[0]
	if strings.HasPrefix(a, "-") {
		return "", "", fmt.Errorf("web: unknown flag %s (usage: bough web [addr|status|stop])", a)
	}
	if _, _, err := net.SplitHostPort(a); err != nil {
		if p, perr := net.LookupPort("tcp", a); perr == nil && p > 0 {
			return "start", fmt.Sprintf("localhost:%d", p), nil // a bare port
		}
		return "", "", fmt.Errorf("web: %q is not host:port or a port", a)
	}
	return "start", a, nil
}

// webURL is what to open for a listen address: a wildcard host becomes
// localhost.
func webURL(addr string) string {
	host, port, err := net.SplitHostPort(addr)
	if err != nil {
		return "http://" + addr
	}
	if host == "" || host == "0.0.0.0" || host == "::" {
		host = "localhost"
	}
	return "http://" + net.JoinHostPort(host, port)
}

func runWeb(args []string) {
	verb, addr, err := webArgs(args)
	if err != nil {
		fatal(err)
	}
	home, err := os.UserHomeDir()
	if err != nil {
		fatal(fmt.Errorf("home dir: %w", err))
	}
	switch verb {
	case "status":
		if w, ok := runningWeb(home); ok {
			fmt.Printf("bough web: running at %s (pid %d)\n", webURL(w.addr), w.pid)
			if where := w.where(); where != "" {
				fmt.Println("bough web: started " + where)
			}
		} else {
			fmt.Println("bough web: not running")
		}
		return
	case "stop":
		w, ok := runningWeb(home)
		if !ok {
			fmt.Println("bough web: not running")
			return
		}
		if err := syscall.Kill(w.pid, syscall.SIGINT); err != nil {
			fatal(fmt.Errorf("signal pid %d: %w", w.pid, err))
		}
		for i := 0; i < 100 && alive(w.pid); i++ {
			time.Sleep(50 * time.Millisecond)
		}
		os.Remove(webPidfile(home))
		fmt.Printf("bough web: stopped %s (pid %d)\n", webURL(w.addr), w.pid)
		return
	}

	// The web session is detached: its stderr goes to a log nobody
	// reads, so a stale binary — which is how an llm-small row ends up
	// silently replacing the agent's model — has to be said here.
	if n := staleNotice(resolveExe()); n != "" {
		fmt.Fprintln(os.Stderr, "bough web: "+n)
	}
	if w, ok := runningWeb(home); ok {
		// Running `bough` again is a new session; opening the browser
		// again should be too. The detached process is asked for one
		// rather than restarted, so nothing in flight is lost — but
		// only when it is new enough to understand the request, since
		// SIGUSR1 kills a bough that is not.
		switch {
		case w.canNewSession():
			if err := syscall.Kill(w.pid, syscall.SIGUSR1); err != nil {
				fmt.Fprintf(os.Stderr, "bough web: could not ask for a new session: %v\n", err)
			}
			fmt.Printf("bough web: %s (pid %d) — new session\n", webURL(w.addr), w.pid)
		default:
			fmt.Printf("bough web: already running at %s (pid %d)\n", webURL(w.addr), w.pid)
			fmt.Println("bough web: that session predates `bough web` starting a new one; `bough web stop` then `bough web`")
		}
		// The trap: a session started elsewhere runs THAT directory's
		// bough.yml, so opening it from here shows someone else's
		// model and no cost, with nothing to say why.
		if here, err := os.Getwd(); err == nil && w.dir != "" && w.dir != here {
			fmt.Printf("bough web: it was started in %s, so it is running that directory's config, not this one\n", w.dir)
			fmt.Println("bough web: `bough web stop` then `bough web` here to use this directory's")
		} else if where := w.where(); where != "" {
			fmt.Println("bough web: started " + where)
		}
		openBrowser(webURL(w.addr))
		return
	}
	pid, logPath, err := launchWeb(home, resolveExe(), addr)
	if err != nil {
		fatal(err)
	}
	// Wait for the port, so the browser opens on a page and a failed
	// start is reported here with its log, not discovered later.
	for range 100 {
		if c, err := net.DialTimeout("tcp", addr, 200*time.Millisecond); err == nil {
			c.Close()
			fmt.Printf("bough web: %s (pid %d, log %s)\n", webURL(addr), pid, logPath)
			openBrowser(webURL(addr))
			return
		}
		if !alive(pid) {
			break
		}
		time.Sleep(100 * time.Millisecond)
	}
	fatal(fmt.Errorf("web session (pid %d) did not open %s; see %s", pid, addr, logPath))
}

// openBrowser is best-effort: macOS `open`, else xdg-open; failure is
// silent because the URL was already printed.
func openBrowser(url string) {
	if os.Getenv("BOUGH_NO_OPEN") != "" {
		return
	}
	cmd := "xdg-open"
	if runtime.GOOS == "darwin" {
		cmd = "open"
	}
	_ = exec.Command(cmd, url).Start()
}
