package vtreal

// Quitting during a provider retry backoff: a fake OpenAI server
// answers 529 "overloaded" (with Retry-After) twice, which puts
// llm.withRetries into its rate-limit budget and a 5 s wait with the
// "retrying in" notice on screen. During that countdown the pane is
// resized and ctrl+c quits. The process must exit within 1 s (the
// backoff sleep honours ctx and never blocks quit), leave the terminal
// restored (alt screen off, kitty keyboard mode popped) and leave no
// half-written assistant entry for the next resume to trip over.
//
// The replay llm cannot stand in here: retries live in the provider's
// HTTP path (plugins/llm/retry.go), which the replay plugin skips.

import (
	"bytes"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
	"github.com/charmbracelet/x/ansi"
	"github.com/charmbracelet/x/vt"
	"github.com/charmbracelet/x/xpty"

	"github.com/andreylukin/bough/plugins/history"
)

// retryBackoffThenResizeAndQuitServer answers the first two calls with
// 529 overloaded and every later one with a short reply.
func retryBackoffThenResizeAndQuitServer(t *testing.T) (*httptest.Server, *atomic.Int32) {
	var calls atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/responses" {
			http.NotFound(w, r)
			return
		}
		if calls.Add(1) <= 2 {
			w.Header().Set("Retry-After", "1")
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(529)
			fmt.Fprint(w, `{"error":{"type":"overloaded_error","message":"overloaded"}}`)
			return
		}
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprint(w, `data: {"type":"response.output_text.delta","delta":"LATEREPLY7"}`+"\n\n"+
			`data: {"type":"response.completed","response":{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"LATEREPLY7"}]}],"usage":{"input_tokens":1,"output_tokens":1}}}`+"\n\ndata: [DONE]\n\n")
	}))
	t.Cleanup(srv.Close)
	return srv, &calls
}

// retryBackoffThenResizeAndQuitRaw is everything the app wrote to the PTY.
type retryBackoffThenResizeAndQuitRaw struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (r *retryBackoffThenResizeAndQuitRaw) Write(p []byte) (int, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.buf.Write(p)
}

func (r *retryBackoffThenResizeAndQuitRaw) String() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.buf.String()
}

// retryBackoffThenResizeAndQuitTerminal is NewTerminal whose app output
// is also teed into raw, so the exit sequences can be asserted as bytes.
func retryBackoffThenResizeAndQuitTerminal(t *testing.T, cols, rows int, raw io.Writer) *Terminal {
	pty, err := xpty.NewPty(cols, rows)
	if err != nil {
		t.Fatal(err)
	}
	term := &Terminal{tb: t, pty: pty, cols: cols, rows: rows, dec: map[ansi.DECMode]ansi.ModeSetting{}, cursorVis: true}
	emu := vt.NewSafeEmulator(cols, rows)
	emu.SetCallbacks(vt.Callbacks{
		AltScreen: func(alt bool) { term.mu.Lock(); term.altScreen = alt; term.mu.Unlock() },
	})
	term.Emu = emu
	setTitle := func(s string) { term.mu.Lock(); term.title = s; term.mu.Unlock() }
	go io.Copy(io.MultiWriter(newTitleFilter(emu, setTitle), raw), pty) //nolint:errcheck // app output → emulator + raw
	go io.Copy(pty, emu)                                                //nolint:errcheck // keys → app
	return term
}

// retryBackoffThenResizeAndQuitStart is costStart on the raw-teeing
// terminal with a kitty TERM, so the kitty keyboard push is exercised.
func retryBackoffThenResizeAndQuitStart(t *testing.T, home, yml string, raw io.Writer) *app {
	t.Helper()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	const cols, rows = 120, 30
	term := retryBackoffThenResizeAndQuitTerminal(t, cols, rows, raw)
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-kitty", "KITTY_WINDOW_ID=1", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", "OPENAI_API_KEY=test-key",
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: cols, rows: rows, home: home}
	t.Cleanup(func() {
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	a.waitFor("say something")
	return a
}

// retryBackoffThenResizeAndQuitSession is the session file this run
// wrote (not the costold fixture).
func retryBackoffThenResizeAndQuitSession(t *testing.T, home string) string {
	t.Helper()
	paths, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
	for _, p := range paths {
		if filepath.Base(p) != "costold.jsonl" {
			return p
		}
	}
	t.Fatalf("no session file written under %s", home)
	return ""
}

func retryBackoffThenResizeAndQuitTail(s string) string {
	if len(s) > 200 {
		return s[len(s)-200:]
	}
	return s
}

var (
	retryBackoffThenResizeAndQuitPush = regexp.MustCompile(`\x1b\[>\d*u`)
	retryBackoffThenResizeAndQuitPop  = regexp.MustCompile(`\x1b\[<\d*u`)
)

// retryBackoffThenResizeAndQuitTmux is startTmux with yml and a key for
// the fake provider. The x/vt emulator cannot judge a resize (see
// TestResizeKeepsComposerOnScreen), so the resize runs under tmux.
func retryBackoffThenResizeAndQuitTmux(t *testing.T, yml string) (*tmuxApp, string) {
	t.Helper()
	if _, err := exec.LookPath("tmux"); err != nil {
		t.Skip("tmux not installed")
	}
	home, _ := costHome(t)
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	tm := &tmuxApp{t: t, sock: fmt.Sprintf("vtreal-rbq-%d-%d", os.Getpid(), time.Now().UnixNano())}
	shell := fmt.Sprintf("cd %s && HOME=%s TERM=xterm-256color OPENAI_API_KEY=test-key %s -config %s", home, home, bin, cfg)
	tm.run("new-session", "-d", "-x", "120", "-y", "30", shell)
	t.Cleanup(func() {
		_ = exec.Command("tmux", "-L", tm.sock, "kill-server").Run()
		dir := os.Getenv("TMUX_TMPDIR")
		if dir == "" {
			dir = "/tmp"
		}
		_ = os.Remove(filepath.Join(dir, fmt.Sprintf("tmux-%d", os.Getuid()), tm.sock))
	})
	tm.waitFor("say something")
	return tm, home
}

// retryBackoffThenResizeAndQuitGone reports whether the tmux session
// ended, which it does when its only process (bough) exits.
func retryBackoffThenResizeAndQuitGone(tm *tmuxApp) bool {
	return exec.Command("tmux", "-L", tm.sock, "has-session").Run() != nil
}

func TestRetryBackoffThenResizeAndQuit(t *testing.T) {
	t.Parallel()

	// Resize during the countdown (tmux), then quit: the composer
	// follows the resize and ctrl+c exits within 1 s.
	t.Run("ResizeUnderTmux", func(t *testing.T) {
		t.Parallel()
		srv, calls := retryBackoffThenResizeAndQuitServer(t)
		tm, _ := retryBackoffThenResizeAndQuitTmux(t, costConfig(srv.URL, ""))
		tm.keys("hello", "Enter")
		tm.waitFor("retrying in")
		tm.resize(90, 24)
		tm.waitUntil(func(s string) bool {
			ls := strings.Split(s, "\n")
			return composerRow(ls) >= len(ls)-3 && strings.Contains(s, "? keys") && strings.Contains(s, "retrying in")
		}, "composer on the last rows, countdown still shown, after resize to 90x24")
		// The first ctrl+c cancels the turn; then the usual arm + quit.
		tm.keys("C-c")
		tm.waitFor("cancelled")
		tm.keys("C-c")
		tm.waitFor("again to quit")
		tm.keys("C-c")
		pressed := time.Now()
		for !retryBackoffThenResizeAndQuitGone(tm) {
			if time.Since(pressed) > 8*time.Second {
				t.Fatalf("process did not exit during the retry backoff:\n%s", tm.screen())
			}
			time.Sleep(20 * time.Millisecond)
		}
		if d := time.Since(pressed); d > time.Second {
			t.Errorf("exit took %s after ctrl+c, want <= 1s", d)
		}
		if n := calls.Load(); n > 2 {
			t.Errorf("provider called %d times: a retry ran after quit", n)
		}
	})

	t.Run("QuitRestoresTerminal", func(t *testing.T) {
		t.Parallel()
		retryBackoffThenResizeAndQuitRestores(t)
	})
}

// retryBackoffThenResizeAndQuitRestores quits during the countdown on a
// raw-teeing terminal and checks the exit bytes, history and resume.
func retryBackoffThenResizeAndQuitRestores(t *testing.T) {
	srv, calls := retryBackoffThenResizeAndQuitServer(t)
	home, _ := costHome(t)
	raw := &retryBackoffThenResizeAndQuitRaw{}
	a := retryBackoffThenResizeAndQuitStart(t, home, costConfig(srv.URL, ""), raw)
	a.typeText("hello")
	a.key(uv.KeyEnter, 0)
	a.waitFor("retrying in")

	// The first ctrl+c cancels the turn; then the usual arm + quit.
	a.key('c', uv.ModCtrl)
	a.waitFor("cancelled")
	a.key('c', uv.ModCtrl)
	a.waitFor("again to quit")
	a.key('c', uv.ModCtrl)
	pressed := time.Now()
	done := make(chan error, 1)
	go func() { done <- a.term.Wait(a.cmd) }()
	select {
	case <-done:
		if d := time.Since(pressed); d > time.Second {
			t.Errorf("exit took %s after ctrl+c, want <= 1s", d)
		}
	case <-time.After(8 * time.Second):
		t.Fatalf("process did not exit during the retry backoff:\n%s", a.text())
	}
	if n := calls.Load(); n > 2 {
		t.Errorf("provider called %d times: a retry ran after quit", n)
	}

	// Terminal restored: the last alt-screen toggle is off and the
	// last kitty keyboard push is followed by a pop.
	var out string
	for range 100 {
		out = raw.String()
		if strings.LastIndex(out, "\x1b[?1049l") > strings.LastIndex(out, "\x1b[?1049h") {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}
	if strings.LastIndex(out, "\x1b[?1049l") < strings.LastIndex(out, "\x1b[?1049h") {
		t.Errorf("alt screen not left on exit (tail %q)", retryBackoffThenResizeAndQuitTail(out))
	}
	if pushes := retryBackoffThenResizeAndQuitPush.FindAllStringIndex(out, -1); len(pushes) > 0 {
		last := pushes[len(pushes)-1][0]
		if !retryBackoffThenResizeAndQuitPop.MatchString(out[last:]) {
			t.Errorf("kitty keyboard mode pushed %d times, never popped after the last (tail %q)", len(pushes), retryBackoffThenResizeAndQuitTail(out))
		}
	} else {
		t.Logf("no kitty keyboard push seen; pop check vacuous")
	}

	// History: no assistant entry for a reply that never arrived.
	session := retryBackoffThenResizeAndQuitSession(t, home)
	es, err := history.Read(session)
	if err != nil {
		t.Fatal(err)
	}
	var kinds []string
	for _, e := range es {
		kinds = append(kinds, e.Kind)
		if e.Kind == "assistant" {
			t.Errorf("dangling assistant entry after quitting mid-backoff: %v", e.Data)
		}
	}
	t.Logf("history kinds: %v", kinds)

	// Resume the session: it boots clean with no phantom reply, and a
	// new turn (the server is past its 529s) completes.
	a2 := costStart(t, home, costConfig(srv.URL, session))
	a2.check("resumed")
	if s := a2.settled(); strings.Contains(s, "LATEREPLY7") {
		t.Errorf("resumed session shows a reply that never arrived:\n%s", s)
	}
	a2.typeText("again")
	a2.key(uv.KeyEnter, 0)
	a2.waitFor("LATEREPLY7")
	a2.check("resumed turn")
}
