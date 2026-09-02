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
	home   string
	wmu    sync.Mutex // serializes writes to the PTY (test vs responder)
	out    *safeBuf
	exited chan error
}

func launchTUI(t *testing.T, opts launchOpts) *tuiProc {
	t.Helper()
	home, cwd, _ := sandbox(t, opts)

	args := append([]string{"--config", "bough.yml", "--set", "llm.plugin=llm-echo"}, opts.args...)
	cmd := exec.Command(boughBin, args...)
	cmd.Dir = cwd
	cmd.Env = append(env(home), "TERM=xterm-256color")

	f, err := pty.StartWithSize(cmd, &pty.Winsize{Rows: 30, Cols: 100})
	if err != nil {
		t.Skipf("no pty available: %v", err)
	}
	p := &tuiProc{t: t, f: f, home: home, out: &safeBuf{}, exited: make(chan error, 1)}
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

// waitFor polls the ANSI-stripped PTY output for substr. The deadline
// is generous: under a full parallel `go test ./...` a TUI boot can
// take well over 10s to paint its first frame.
func (p *tuiProc) waitFor(substr string) {
	p.t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		if strings.Contains(stripANSI(p.out.String()), substr) {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	p.t.Fatalf("%q not on PTY after 30s; stripped output:\n%s", substr, stripANSI(p.out.String()))
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

// quit sends the default quit key twice (ctrl+c reaches the app as a
// key — bubbletea has the terminal in raw mode; the first press only
// arms the quit) and asserts a prompt, clean exit.
func (p *tuiProc) quit() {
	p.t.Helper()
	p.write("\x03\x03")
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
	// Status bar: " bough · <model>" left, "? keys" right.
	p.waitFor("bough")
	p.waitFor("? keys")
	p.quit()
}

func TestPTYEchoRoundtrip(t *testing.T) {
	t.Parallel()
	p := launchTUI(t, launchOpts{})
	p.waitFor("bough")
	p.write("hello from the pty\r")
	p.waitFor("echo: hello from the pty")
	p.quit()
	// The exit line names the session and how to get back into it.
	if out := stripANSI(p.out.String()); !strings.Contains(out, "resume with: bough -r ") {
		t.Fatalf("exit line missing after quit; output:\n%s", out)
	}
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

// Bare --resume in TUI mode: the session picker shows before the chat,
// enter resumes the selected session (the launcher's "session-choose"
// reconfigures the history row and Reconcile remounts loop+ui), the
// transcript replays, and the resumed session keeps answering with the
// prior context on file (no new session file appears).
func TestPTYResumePicker(t *testing.T) {
	t.Parallel()
	// Seed one finished session in a fresh sandbox.
	a := launchHeadless(t, launchOpts{})
	a.send("seeded turn")
	a.closeStdin()
	if code := a.waitExit(); code != 0 {
		t.Fatalf("seed exit %d:\n%s", code, a.out.String())
	}
	seeded := sessionFiles(t, a.home)
	if len(seeded) != 1 {
		t.Fatalf("want 1 seeded session, got %v", seeded)
	}
	before := lineCount(t, seeded[0])

	p := launchTUI(t, launchOpts{from: a, args: []string{"-r"}})
	p.waitFor("resume a session") // picker header
	p.waitFor("seeded turn")      // the session's title row
	p.write("\r")                 // pick it

	// Replay: the prior turn renders as transcript blocks.
	p.waitFor("echo: seeded turn")

	// The resumed session is live: a new turn answers and appends.
	p.write("after resume\r")
	p.waitFor("echo: after resume")
	p.quit()

	after := sessionFiles(t, a.home)
	if len(after) != 1 || after[0] != seeded[0] {
		t.Fatalf("picker resume must reuse the same file: %v -> %v", seeded, after)
	}
	if got := lineCount(t, after[0]); got <= before {
		t.Fatalf("resumed file did not grow: %d -> %d lines", before, got)
	}
}

// Bare -r in TUI mode: the session picker appears before chat; enter
// resumes the selected session through the live seam (session-choose ->
// history row Reconcile -> loop+ui remount), the transcript replays,
// and the next turn's model context includes the resumed history.
func TestPTYBareResumePickerResumes(t *testing.T) {
	t.Parallel()
	p := launchTUI(t, launchOpts{
		home: map[string]string{
			".bough/history/oldsession.jsonl": `{"seq":1,"kind":"input","data":{"text":"old question"}}` + "\n" +
				`{"seq":2,"kind":"assistant","data":{"text":"old answer"}}` + "\n" +
				`{"seq":3,"kind":"done","data":{"text":""}}` + "\n",
		},
		cwd:  map[string]string{".bough/init.js": parrotInit},
		args: []string{"-r"},
	})
	p.waitFor("resume a session")
	p.waitFor("old question") // the session's title row
	p.write("\r")             // enter: resume the selected (only) session
	p.waitFor("old answer")   // transcript replayed into the chat view
	p.write("new turn\r")
	// input(old)+assistant(old)+input(new) project to 3 messages.
	p.waitFor("parrot(new turn) after 3 msgs")
	p.quit()

	// The resumed file grew; the fresh boot session (abandoned empty by
	// the pick) left no stray file.
	files := sessionFiles(t, p.home)
	if len(files) != 1 || !strings.HasSuffix(files[0], "oldsession.jsonl") {
		t.Fatalf("want only the resumed session file, got %v", files)
	}
	if got := lineCount(t, files[0]); got <= 3 {
		t.Fatalf("resumed file did not grow: %d lines", got)
	}
}

// Esc on the picker starts a fresh session: no resumed context.
func TestPTYBareResumePickerEscFresh(t *testing.T) {
	t.Parallel()
	p := launchTUI(t, launchOpts{
		home: map[string]string{
			".bough/history/oldsession.jsonl": `{"seq":1,"kind":"input","data":{"text":"old question"}}` + "\n",
		},
		cwd:  map[string]string{".bough/init.js": parrotInit},
		args: []string{"-r"},
	})
	p.waitFor("resume a session")
	p.write("\x1b") // esc: fresh session
	// Wait for the chat view before typing: esc immediately followed by
	// text would coalesce into alt-key sequences on the wire.
	p.waitFor("say something")
	p.write("fresh start\r")
	p.waitFor("parrot(fresh start) after 1 msgs")
	p.quit()
}
