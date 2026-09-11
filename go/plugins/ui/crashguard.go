package ui

import (
	"io"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"runtime/debug"
	"syscall"
	"time"

	"github.com/charmbracelet/x/term"
)

// A Go panic in any goroutine kills the process without running a
// single defer elsewhere, so nothing in-process can hand the terminal
// back. The crash guard is a tiny child (this binary, re-exec'd) that
// starts before the tui takes the terminal: it remembers the cooked
// tty state and waits on a pipe. A clean exit writes one byte first;
// EOF without it means bough died, and the guard restores the terminal
// and reprints the crash (the runtime drew it on the alt screen).

const guardEnv = "BOUGH_TTY_GUARD"

// Leave alt screen, mouse 1000/1002/1003/1006, bracketed paste, focus
// events; show the cursor.
const restoreSeq = "\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?1004l\x1b[?25h\x1b[?1049l"

var guardW *os.File

// RunCrashGuardIfAsked is the guard child's entry point; main calls it
// first. It returns only in a normal (non-guard) process.
func RunCrashGuardIfAsked() {
	logPath := os.Getenv(guardEnv)
	if logPath == "" {
		return
	}
	signal.Ignore(os.Interrupt, syscall.SIGTERM, syscall.SIGHUP)
	// The tty arrives as fd 3 only: stdio is a pipe and /dev/null, and
	// on unix the guard has no controlling terminal, so nothing in this
	// re-exec'd binary's inits can query the terminal or eat keystrokes.
	tty := os.NewFile(3, "tty")
	fd := tty.Fd()
	st, _ := term.GetState(fd)
	_, _ = os.Stdout.Write([]byte{1}) // cooked state saved: the tui may go raw
	os.Stdout.Close()
	var off int64
	if fi, err := os.Stat(logPath); err == nil {
		off = fi.Size()
	}
	var b [1]byte
	if n, _ := os.Stdin.Read(b[:]); n == 1 {
		os.Exit(0) // clean exit
	}
	if st != nil {
		_ = term.Restore(fd, st)
	}
	_, _ = tty.WriteString(restoreSeq)
	if f, err := os.Open(logPath); err == nil {
		_, _ = f.Seek(off, io.SeekStart)
		_, _ = io.Copy(tty, f)
		f.Close()
	}
	os.Exit(0)
}

// startCrashGuard sends crash output to ~/.bough/bough.log and starts
// the guard child. Best effort: without it bough still runs.
func startCrashGuard() {
	home, err := os.UserHomeDir()
	if err != nil {
		return
	}
	dir := filepath.Join(home, ".bough")
	_ = os.MkdirAll(dir, 0o755)
	logPath := filepath.Join(dir, "bough.log")
	lf, err := os.OpenFile(logPath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return
	}
	_ = debug.SetCrashOutput(lf, debug.CrashOptions{})
	lf.Close()
	exe, err := os.Executable()
	if err != nil {
		return
	}
	r, w, err := os.Pipe()
	if err != nil {
		return
	}
	cmd := exec.Command(exe)
	cmd.Env = append(os.Environ(), guardEnv+"="+logPath)
	cmd.Stdin = r
	cmd.ExtraFiles = []*os.File{os.Stdout}
	rr, rw, err := os.Pipe()
	if err != nil {
		r.Close()
		w.Close()
		return
	}
	cmd.Stdout = rw
	guardSysProc(cmd)
	if err := cmd.Start(); err != nil {
		r.Close()
		w.Close()
		rr.Close()
		rw.Close()
		return
	}
	r.Close()
	rw.Close()
	// Wait until the guard saved the cooked tty, or it would restore raw.
	_ = rr.SetReadDeadline(time.Now().Add(time.Second))
	var b [1]byte
	_, _ = rr.Read(b[:])
	rr.Close()
	go func() { _ = cmd.Wait() }()
	guardW = w
}

// stopCrashGuard tells the guard the terminal was handed back cleanly.
func stopCrashGuard() {
	if guardW != nil {
		_, _ = guardW.Write([]byte{1})
		guardW.Close()
		guardW = nil
	}
}
