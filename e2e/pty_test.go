//go:build !windows

// Native-TTY sanity: the real binary in TUI mode on a PTY. Assertions
// run on ANSI-stripped output; the terminal-restore check reads the raw
// escape stream.
package e2e

import (
	"os"
	"os/exec"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/creack/pty"
)

// tuiProc is one bough TUI running on its own PTY.
type tuiProc struct {
	t      *testing.T
	f      *os.File
	wmu    sync.Mutex // serializes writes to the PTY (test vs responder)
	out    *safeBuf
	exited chan error
}

func launchTUI(t *testing.T, opts launchOpts) *tuiProc {
	t.Helper()
	home, cwd, _ := sandbox(t, opts)

	cmd := exec.Command(boughBin, "--config", "bough.yml", "--set", "llm.plugin=llm-echo")
	cmd.Dir = cwd
	cmd.Env = append(env(home), "TERM=xterm-256color")

	f, err := pty.StartWithSize(cmd, &pty.Winsize{Rows: 30, Cols: 100})
	if err != nil {
		t.Skipf("no pty available: %v", err)
	}
	p := &tuiProc{t: t, f: f, out: &safeBuf{}, exited: make(chan error, 1)}
	go func() {
		// The reader doubles as a minimal terminal: bubbletea v2 blocks
		// its first render on cursor-position (and DA1) replies that a
		// real terminal would send, so answer them.
		buf := make([]byte, 32*1024)
		for {
			n, err := f.Read(buf)
			if n > 0 {
				chunk := string(buf[:n])
				if strings.Contains(chunk, "\x1b[6n") {
					p.reply("\x1b[1;1R")
				}
				if strings.Contains(chunk, "\x1b[c") || strings.Contains(chunk, "\x1b[0c") {
					p.reply("\x1b[?62c")
				}
				p.out.Write(buf[:n])
			}
			if err != nil {
				return // EOF/EIO when the child exits
			}
		}
	}()
	go func() { p.exited <- cmd.Wait() }()
	t.Cleanup(func() {
		select {
		case <-p.exited:
		default:
			cmd.Process.Kill()
			<-p.exited
		}
		f.Close()
	})
	return p
}

// waitFor polls the ANSI-stripped PTY output for substr.
func (p *tuiProc) waitFor(substr string) {
	p.t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		if strings.Contains(stripANSI(p.out.String()), substr) {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	p.t.Fatalf("%q not on PTY after 10s; stripped output:\n%s", substr, stripANSI(p.out.String()))
}

// reply writes a terminal response; write errors after exit are fine.
func (p *tuiProc) reply(s string) {
	p.wmu.Lock()
	defer p.wmu.Unlock()
	p.f.WriteString(s)
}

func (p *tuiProc) write(s string) {
	p.t.Helper()
	p.wmu.Lock()
	defer p.wmu.Unlock()
	if _, err := p.f.Write([]byte(s)); err != nil {
		p.t.Fatalf("pty write %q: %v", s, err)
	}
}

// quit sends the default quit key (ctrl+c reaches the app as a key —
// bubbletea has the terminal in raw mode) and asserts a prompt, clean exit.
func (p *tuiProc) quit() {
	p.t.Helper()
	p.write("\x03")
	select {
	case err := <-p.exited:
		p.exited <- err
		if err != nil {
			p.t.Fatalf("TUI exited with error: %v\nstripped output:\n%s", err, stripANSI(p.out.String()))
		}
	case <-time.After(5 * time.Second):
		p.t.Fatalf("TUI did not exit within 5s of quit key; stripped output:\n%s", stripANSI(p.out.String()))
	}
}

func TestPTYBootsToStatusBar(t *testing.T) {
	t.Parallel()
	p := launchTUI(t, launchOpts{})
	// Status bar: " bough · <provider> · N rows" plus the history entry count.
	p.waitFor("bough")
	p.waitFor("llm-echo")
	p.waitFor("rows")
	p.quit()
}

func TestPTYEchoRoundtrip(t *testing.T) {
	t.Parallel()
	p := launchTUI(t, launchOpts{})
	p.waitFor("bough")
	p.write("hello from the pty\r")
	p.waitFor("echo: hello from the pty")
	p.quit()
}

func TestPTYQuitRestoresTerminal(t *testing.T) {
	t.Parallel()
	p := launchTUI(t, launchOpts{})
	p.waitFor("bough")
	p.quit()
	// Give the reader goroutine a beat to drain the final escape flush.
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if strings.Contains(p.out.String(), "\x1b[?1049l") {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}
	raw := p.out.String()
	if !strings.Contains(raw, "\x1b[?1049h") {
		t.Fatalf("alt-screen enter sequence never seen; raw output:\n%q", raw)
	}
	if !strings.Contains(raw, "\x1b[?1049l") {
		t.Fatalf("terminal not restored: leave-alt-screen sequence missing; raw output:\n%q", raw)
	}
}
