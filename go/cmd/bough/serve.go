// bough serve [addr]: run the session control API as a detached
// daemon (or report/stop the one already running), mirroring the
// `bough web` lifecycle. The API itself lives in internal/serve; this
// file is only the subcommand and the foreground server.
package main

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"
	"time"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/serve/watch"
	"github.com/andreylukin/bough/internal/serveclient"
)

// defaultServeAddr is the control API's own port: not 7683 (the
// artifacts page server) and not 7681 (the web TUI), so the three can
// run side by side. Loopback by default; anything else needs
// --insecure-bind (see serveAddr).
const defaultServeAddr = "127.0.0.1:7684"

// serveArgs parses `bough serve` arguments, mirroring webArgs: no
// args or an address starts (or attaches to) the daemon; "status" and
// "stop" are their own verbs; the hidden "--run <addr>" is how the
// detached child is told to be the server in the foreground.
// --insecure-bind, anywhere, allows a non-loopback address.
// --host=NAME, anywhere, trusts one more Host header — the name a
// reverse proxy in front of a loopback bind forwards. Pure.
func serveArgs(all []string) (verb, addr string, insecure bool, host string, err error) {
	usage := "(usage: bough serve [--insecure-bind] [--host=NAME] [addr|status|stop])"
	var args []string
	for i := 0; i < len(all); i++ {
		a := all[i]
		switch {
		case a == "--insecure-bind":
			insecure = true
			continue
		case strings.HasPrefix(a, "--host="):
			host = strings.TrimPrefix(a, "--host=")
		case a == "--host" && i+1 < len(all):
			i++
			host = all[i]
		default:
			args = append(args, a)
			continue
		}
		if host == "" {
			return "", "", false, "", fmt.Errorf("serve: --host needs a name %s", usage)
		}
	}
	switch {
	case len(args) == 0:
		return "start", defaultServeAddr, insecure, host, nil
	case args[0] == "--run":
		switch len(args) {
		case 1:
			return "--run", defaultServeAddr, insecure, host, nil
		case 2:
			a, aerr := serveAddr(args[1], insecure)
			if aerr != nil {
				return "", "", false, "", aerr
			}
			return "--run", a, insecure, host, nil
		}
		return "", "", false, "", fmt.Errorf("serve: --run takes at most one address, got %v %s", args[1:], usage)
	case len(args) > 1:
		return "", "", false, "", fmt.Errorf("serve: takes at most one argument, got %v %s", args, usage)
	}
	switch args[0] {
	case "status", "stop":
		return args[0], "", insecure, host, nil
	case "start":
		return "start", defaultServeAddr, insecure, host, nil
	}
	a, err := serveAddr(args[0], insecure)
	if err != nil {
		return "", "", false, "", err
	}
	return "start", a, insecure, host, nil
}

// serveAddr accepts host:port or a bare port; a bare port binds
// loopback, never a wildcard. A non-loopback host (":7684" included)
// is refused unless insecure: the API runs agents and writes hooks,
// and a token is all that would stand between it and the network.
func serveAddr(a string, insecure bool) (string, error) {
	usage := "(usage: bough serve [--insecure-bind] [--host=NAME] [addr|status|stop])"
	if strings.HasPrefix(a, "-") {
		return "", fmt.Errorf("serve: unknown flag %s %s", a, usage)
	}
	if _, _, err := net.SplitHostPort(a); err != nil {
		if p, perr := net.LookupPort("tcp", a); perr == nil && p > 0 {
			return fmt.Sprintf("127.0.0.1:%d", p), nil
		}
		return "", fmt.Errorf("serve: %q is not host:port or a port %s", a, usage)
	}
	if watch.CheckLoopback(a) != nil && !insecure {
		return "", fmt.Errorf("serve: %s is not a loopback address; the API would be reachable from the network — pass --insecure-bind to do it anyway %s", a, usage)
	}
	return a, nil
}

func servePidfile(home string) string {
	return filepath.Join(home, ".bough", "serve.pid")
}

// runningServe reports the live serve daemon recorded in the pidfile,
// if any; a stale pidfile is removed. Same record shape as the web
// pidfile so the two lifecycles read alike.
func runningServe(home string) (webSession, bool) {
	pf := servePidfile(home)
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

// writeServePidfile records "<pid> <addr>\t<cwd>\t<config>\t<caps>".
// A pidfile naming a live foreign pid is left alone: a second daemon
// that fails to bind must not leave the file naming its own dead pid
// while the first one serves on — and two supervisors would mean two
// writers per session file. The returned cleanup (nil on failure)
// removes the file on clean shutdown.
func writeServePidfile(home, addr string) func() {
	pf := servePidfile(home)
	if b, err := os.ReadFile(pf); err == nil {
		if pid, _, _, _, _, perr := parsePidfile(string(b)); perr == nil && pid != os.Getpid() && alive(pid) {
			fmt.Fprintf(os.Stderr, "bough serve: already running (pid %d); leaving its pidfile alone\n", pid)
			return nil
		}
	}
	if err := os.MkdirAll(filepath.Dir(pf), 0o755); err != nil {
		fmt.Fprintln(os.Stderr, "bough serve: pidfile:", err)
		return nil
	}
	dir, _ := os.Getwd()
	if err := os.WriteFile(pf, fmt.Appendf(nil, "%d %s\t%s\t%s\t%s\n",
		os.Getpid(), addr, dir, webConfig, ""), 0o644); err != nil {
		fmt.Fprintln(os.Stderr, "bough serve: pidfile:", err)
		return nil
	}
	return func() { os.Remove(pf) }
}

// launchServe starts `bin serve --run addr` detached (own session,
// output appended to ~/.bough/serve.log) and returns its pid and log
// path.
func launchServe(home, bin, addr string, insecure bool, host string) (int, string, error) {
	logPath := filepath.Join(home, ".bough", "serve.log")
	if err := os.MkdirAll(filepath.Dir(logPath), 0o755); err != nil {
		return 0, "", fmt.Errorf("serve: log dir: %w", err)
	}
	logF, err := os.OpenFile(logPath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return 0, "", fmt.Errorf("serve: open %s: %w", logPath, err)
	}
	defer logF.Close()
	argv := []string{"serve", "--run", addr}
	if insecure {
		argv = append(argv, "--insecure-bind")
	}
	if host != "" {
		argv = append(argv, "--host="+host)
	}
	cmd := exec.Command(bin, argv...)
	cmd.Stdout = logF
	cmd.Stderr = logF
	detach(cmd)
	if err := cmd.Start(); err != nil {
		return 0, "", fmt.Errorf("serve: launch %s serve --run %s: %w", bin, addr, err)
	}
	return cmd.Process.Pid, logPath, nil
}

func runServe(args []string) {
	verb, addr, insecure, host, err := serveArgs(args)
	if err != nil {
		fatal(err)
	}
	home, err := os.UserHomeDir()
	if err != nil {
		fatal(fmt.Errorf("serve: home dir: %w", err))
	}
	switch verb {
	case "status":
		if w, ok := runningServe(home); ok {
			fmt.Printf("bough serve: running at http://%s (pid %d)\n", w.addr, w.pid)
			if where := w.where(); where != "" {
				fmt.Println("bough serve: started " + where)
			}
		} else {
			fmt.Println("bough serve: not running")
		}
		return
	case "stop":
		w, ok := runningServe(home)
		if !ok {
			fmt.Println("bough serve: not running")
			return
		}
		if err := interrupt(w.pid); err != nil {
			fatal(fmt.Errorf("serve: signal pid %d: %w", w.pid, err))
		}
		// SIGINT so the daemon can kill and reap its children before
		// exiting; a child left running would keep a session lease.
		for i := 0; i < 100 && alive(w.pid); i++ {
			time.Sleep(50 * time.Millisecond)
		}
		os.Remove(servePidfile(home))
		fmt.Printf("bough serve: stopped http://%s (pid %d)\n", w.addr, w.pid)
		return
	case "--run":
		if err := serveForeground(home, addr, insecure, host); err != nil {
			fatal(err)
		}
		return
	}

	// Never a second daemon: two supervisors would each spawn children
	// for the same session, and two writers on one history file is the
	// hazard history.ConcurrentWriter exists for.
	if w, ok := runningServe(home); ok {
		fmt.Printf("bough serve: already running at http://%s (pid %d)\n", w.addr, w.pid)
		if where := w.where(); where != "" {
			fmt.Println("bough serve: started " + where)
		}
		return
	}
	pid, logPath, err := launchServe(home, resolveExe(), addr, insecure, host)
	if err != nil {
		fatal(err)
	}
	// Wait for the port, so a failed start is reported here with its
	// log rather than discovered on the first request.
	for range 100 {
		if c, err := net.DialTimeout("tcp", addr, 200*time.Millisecond); err == nil {
			c.Close()
			// The line a first run reads: where to go and how to stop it,
			// since the daemon outlives the shell that started it.
			fmt.Printf("bough serve: http://%s (pid %d, log %s)\n", addr, pid, logPath)
			fmt.Printf("  open it in a browser; stop it with: bough serve stop\n")
			return
		}
		if !alive(pid) {
			break
		}
		time.Sleep(100 * time.Millisecond)
	}
	fatal(fmt.Errorf("serve: daemon (pid %d) did not open %s; see %s", pid, addr, logPath))
}

// serveForeground is the daemon body: supervisor + API on addr until
// SIGINT/SIGTERM, then drain HTTP and kill every child.
func serveForeground(home, addr string, insecure bool, host string) error {
	remote := watch.CheckLoopback(addr) != nil
	if remote {
		fmt.Fprintf(os.Stderr, "bough serve: WARNING: %s is not loopback; the API is reachable from the network, guarded only by the token in %s\n", addr, serveclient.TokenPath(home))
	}
	token, err := serveclient.LoadToken(home)
	if err != nil {
		return fmt.Errorf("serve: %w", err)
	}
	if done := writeServePidfile(home, addr); done != nil {
		defer done()
	}
	sup, err := serve.NewSupervisor(serve.Options{
		Exe:      resolveExe(),
		HistDir:  sessionsDir(),
		MetaPath: filepath.Join(home, ".bough", "serve", "meta.json"),
		Home:     home,
	})
	if err != nil {
		return fmt.Errorf("serve: supervisor: %w", err)
	}
	defer sup.Close()

	// Listen before serving so a taken port is an error here, not a
	// line in a log nobody reads.
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		return fmt.Errorf("serve: listen %s: %w", addr, err)
	}
	api := serve.NewAPI(sup)
	srv := &http.Server{Addr: addr, Handler: serve.Guard(api, token, remote && insecure, host)}

	// Watchers run for as long as the server does. They execute shell,
	// so a server bound off loopback gets none — say why, rather than
	// leaving someone wondering where their watcher went.
	watchCtx, stopWatch := context.WithCancel(context.Background())
	defer stopWatch()
	if err := api.StartWatchers(watchCtx, addr); err != nil {
		fmt.Fprintln(os.Stderr, "bough serve: watchers off:", err)
	}

	sigs := make(chan os.Signal, 1)
	signal.Notify(sigs, syscall.SIGINT, syscall.SIGTERM)
	defer signal.Stop(sigs)

	errc := make(chan error, 1)
	go func() { errc <- srv.Serve(ln) }()
	fmt.Printf("bough serve: http://%s (pid %d)\n", addr, os.Getpid())

	select {
	case err := <-errc:
		if err != nil && err != http.ErrServerClosed {
			return fmt.Errorf("serve: http %s: %w", addr, err)
		}
	case <-sigs:
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		// Shut the API first: no new requests can adopt a session
		// while the supervisor is killing its children.
		if err := srv.Shutdown(ctx); err != nil {
			fmt.Fprintln(os.Stderr, "bough serve: shutdown:", err)
		}
	}
	// sup.Close (kill + reap every child) runs on the deferred call
	// above, after Shutdown has drained the API.
	return nil
}
