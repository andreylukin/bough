package vtreal

// A slow terminal: the PTY is drained in small chunks with a pause
// between them (4KB/100ms, an ssh link or a busy tmux), so bough's
// writes back up into the kernel buffer and block. Bough must keep
// reading input (esc still cancels within a bound), the screen must
// converge to what a fast terminal shows, and the process must not
// grow without bound while its output queues.
//
// BOUGH_SLOWTERMINAL_SOAK=1 adds a repeated huge-output soak.

import (
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
	"github.com/charmbracelet/x/ansi"
	"github.com/charmbracelet/x/vt"
	"github.com/charmbracelet/x/xpty"
)

const (
	slowterminalChunk = 4096
	slowterminalEvery = 100 * time.Millisecond
)

// slowterminalNew is NewTerminal with the app→emulator copy throttled
// to chunk bytes per every.
func slowterminalNew(tb testing.TB, cols, rows, chunk int, every time.Duration) (*Terminal, error) {
	pty, err := xpty.NewPty(cols, rows)
	if err != nil {
		return nil, fmt.Errorf("pty: %w", err)
	}
	t := &Terminal{tb: tb, pty: pty, cols: cols, rows: rows, dec: map[ansi.DECMode]ansi.ModeSetting{}, cursorVis: true}
	emu := vt.NewSafeEmulator(cols, rows)
	emu.SetCallbacks(vt.Callbacks{
		Title:     func(s string) { t.mu.Lock(); t.title = s; t.mu.Unlock() },
		AltScreen: func(alt bool) { t.mu.Lock(); t.altScreen = alt; t.mu.Unlock() },
		EnableMode: func(mode ansi.Mode) {
			if m, ok := mode.(ansi.DECMode); ok {
				t.mu.Lock()
				t.dec[m] = ansi.ModeSet
				t.mu.Unlock()
			}
		},
		DisableMode: func(mode ansi.Mode) {
			if m, ok := mode.(ansi.DECMode); ok {
				t.mu.Lock()
				t.dec[m] = ansi.ModeReset
				t.mu.Unlock()
			}
		},
		CursorPosition:   func(_, p uv.Position) { t.mu.Lock(); t.cursor = p; t.mu.Unlock() },
		CursorVisibility: func(v bool) { t.mu.Lock(); t.cursorVis = v; t.mu.Unlock() },
	})
	t.Emu = emu
	setTitle := func(s string) { t.mu.Lock(); t.title = s; t.mu.Unlock() }
	out := newTitleFilter(emu, setTitle)
	go func() {
		buf := make([]byte, chunk)
		for {
			n, err := pty.Read(buf)
			if n > 0 {
				if _, werr := out.Write(buf[:n]); werr != nil {
					return
				}
			}
			if err != nil {
				return
			}
			time.Sleep(every)
		}
	}()
	go io.Copy(pty, emu) //nolint:errcheck // emulator input (keys, replies) → app
	return t, nil
}

// slowterminalStart is startCfg on a throttled terminal (slow) or the
// usual one.
func slowterminalStart(t *testing.T, cols, rows int, yml string, slow bool) *app {
	t.Helper()
	if !slow {
		return startCfg(t, cols, rows, yml)
	}
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := slowterminalNew(t, cols, rows, slowterminalChunk, slowterminalEvery)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=",
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

// slowterminalLongTape writes a one-turn tape whose reply is words
// long, starting LONGSTART and ending LONGEND.
func slowterminalLongTape(t *testing.T, words int) string {
	t.Helper()
	var sb strings.Builder
	sb.WriteString("LONGSTART ")
	for i := range words {
		fmt.Fprintf(&sb, "w%04d ", i)
		if i%12 == 11 {
			sb.WriteString("\n")
		}
	}
	sb.WriteString("LONGEND\n\n```stop\nLONGEND\n```")
	q := strconv.Quote(sb.String())
	lines := []string{
		`{"seq": 1, "at": "2026-09-11T10:00:00Z", "kind": "meta", "data": {"cwd": "/tmp/demo"}}`,
		`{"seq": 2, "at": "2026-09-11T10:00:01Z", "kind": "input", "data": {"text": "long please"}}`,
		`{"seq": 3, "at": "2026-09-11T10:00:02Z", "kind": "assistant", "data": {"text": ` + q + `}}`,
		`{"seq": 4, "at": "2026-09-11T10:00:03Z", "kind": "done", "data": {"text": ""}}`,
	}
	p := filepath.Join(t.TempDir(), "long.jsonl")
	if err := os.WriteFile(p, []byte(strings.Join(lines, "\n")+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// slowterminalClock matches what differs between two runs of the same
// tape: elapsed and timing chips.
var slowterminalClock = regexp.MustCompile(`\d+(\.\d+)?(ms|µs|s|m)\b`)

// slowterminalNorm drops the status bar (the slow run leaves a sticky
// "draft cleared" notice there) and timing chips.
func slowterminalNorm(s string) string {
	var keep []string
	for _, l := range strings.Split(s, "\n") {
		if !strings.Contains(l, "? keys") {
			keep = append(keep, l)
		}
	}
	return slowterminalClock.ReplaceAllString(strings.Join(keep, "\n"), "T")
}

// slowterminalRSS is the bough process's resident set in KB.
func slowterminalRSS(t *testing.T, a *app) int {
	t.Helper()
	out, err := exec.Command("ps", "-o", "rss=", "-p", strconv.Itoa(a.cmd.Process.Pid)).Output()
	if err != nil {
		t.Fatalf("ps: %v", err)
	}
	kb, err := strconv.Atoi(strings.TrimSpace(string(out)))
	if err != nil {
		t.Fatalf("ps rss %q: %v", out, err)
	}
	return kb
}

// slowterminalConverge waits until the normalized screen equals want.
func (a *app) slowterminalConverge(want string, within time.Duration) {
	a.t.Helper()
	deadline := time.Now().Add(within)
	var got string
	for time.Now().Before(deadline) {
		if got = slowterminalNorm(a.settled()); got == want {
			return
		}
	}
	a.t.Fatalf("slow screen never converged to the fast one\nwant:\n%s\ngot:\n%s", want, got)
}

// Esc during a streamed reply on a slow terminal: the cancel reaches
// the loop (history records it) within a bound even while bough's
// output is backed up, and the screen shows it once drained.
func TestSlowTerminalEscCancelsStreaming(t *testing.T) {
	t.Parallel()
	a := slowterminalStart(t, 100, 30, cancelConfig(cancelTape(t), 20), true)
	a.typeText("start the long one")
	a.key(uv.KeyEnter, 0)
	a.waitFor("ALPHASTART")
	t0 := time.Now()
	a.key(uv.KeyEsc, 0)
	if !a.waitDone(1, 3*time.Second) {
		t.Fatalf("esc did not cancel within 3s on a slow terminal:\n%s", a.text())
	}
	t.Logf("cancel recorded after %v", time.Since(t0))
	a.waitFor("■ cancelled")
	s := a.settled()
	if strings.Contains(s, "ALPHAEND") {
		t.Fatalf("the reply kept streaming after esc:\n%s", s)
	}
	a.check("after cancel")
}

// A long streamed reply and a huge block result end on the same
// screen on a slow terminal as on a fast one, input typed while the
// output is backed up still lands, and the slow run's memory stays
// near the fast run's.
func TestSlowTerminalConverges(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name  string
		tape  func(t *testing.T) string
		input string
		delay int
		last  string // on screen once the turn has landed
	}{
		{"LongStream", func(t *testing.T) string { return slowterminalLongTape(t, 3000) }, "long please", 1, "LONGEND"},
		{"HugeOutput", func(t *testing.T) string { return hugeOutputTape(t, hugeOutputLines(5000)) }, "make output", 0, "All printed."},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			tape := c.tape(t)
			run := func(slow bool) *app {
				a := slowterminalStart(t, 100, 30, cancelConfig(tape, c.delay), slow)
				a.typeText(c.input)
				a.key(uv.KeyEnter, 0)
				if slow {
					time.Sleep(200 * time.Millisecond)
					a.typeText("typed during")
				}
				if !a.waitDone(1, 60*time.Second) {
					t.Fatalf("slow=%v: turn never finished:\n%s", slow, a.text())
				}
				a.waitFor(c.last)
				if slow {
					a.waitFor("> typed during")
					a.key(uv.KeyEsc, 0)
					a.waitFor("press esc again to clear the draft")
					a.key(uv.KeyEsc, 0)
					a.waitFor("draft cleared")
				}
				return a
			}
			fa := run(false)
			fast, fastRSS := slowterminalNorm(fa.settled()), slowterminalRSS(t, fa)
			sa := run(true)
			sa.slowterminalConverge(fast, 30*time.Second)
			sa.check("slow converged")
			slowRSS := slowterminalRSS(t, sa)
			t.Logf("rss fast=%dKB slow=%dKB", fastRSS, slowRSS)
			if slowRSS > 2*fastRSS+100*1024 {
				t.Fatalf("slow terminal rss %dKB vs fast %dKB: output queue grew unbounded", slowRSS, fastRSS)
			}
		})
	}
}

// Soak: repeated huge output on a slow terminal; memory must plateau.
func TestSlowTerminalSoak(t *testing.T) {
	if os.Getenv("BOUGH_SLOWTERMINAL_SOAK") == "" {
		t.Skip("set BOUGH_SLOWTERMINAL_SOAK=1 for the slow-terminal soak")
	}
	one, err := os.ReadFile(hugeOutputTape(t, hugeOutputLines(3000)))
	if err != nil {
		t.Fatal(err)
	}
	meta, body, _ := strings.Cut(string(one), "\n")
	const turns = 8
	p := filepath.Join(t.TempDir(), "soak.jsonl")
	if err := os.WriteFile(p, []byte(meta+"\n"+strings.Repeat(body, turns)), 0o644); err != nil {
		t.Fatal(err)
	}
	a := slowterminalStart(t, 100, 30, replayConfig(p), true)
	var rss []int
	for i := range turns {
		a.typeText("make output")
		a.key(uv.KeyEnter, 0)
		if !a.waitDone(i+1, 60*time.Second) {
			t.Fatalf("turn %d never finished:\n%s", i+1, a.text())
		}
		a.check(fmt.Sprintf("turn %d", i+1))
		rss = append(rss, slowterminalRSS(t, a))
	}
	t.Logf("rss per turn (KB): %v", rss)
	if rss[turns-1] > 2*rss[1]+100*1024 {
		t.Fatalf("rss kept growing across turns: %v", rss)
	}
}
