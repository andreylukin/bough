package vtreal

// Quitting after a few turns hands the terminal back: under tmux, a
// shell prints a marker, runs bough through three replayed turns, and
// prints a second marker once bough exits. After the quit the pane
// must be off the alt screen with the marker still visible, the cursor
// shown again, and anything bough printed on exit printed once.

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/replay"
)

const (
	altScreenScrollbackAfterQuitBefore = "PRELUDE-MARK-7f3a"
	altScreenScrollbackAfterQuitAfter  = "EXITED-MARK-7f3a"
)

// altScreenScrollbackAfterQuitStart runs bough on the replay tape
// inside a shell that echoes the prelude marker first; returns the
// tmux app and the run's $HOME.
func altScreenScrollbackAfterQuitStart(t *testing.T, tape string, cols, rows int) (*tmuxApp, string) {
	t.Helper()
	if _, err := exec.LookPath("tmux"); err != nil {
		t.Skip("tmux not installed")
	}
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(replayConfig(tape)), 0o644); err != nil {
		t.Fatal(err)
	}
	tm := &tmuxApp{t: t, sock: fmt.Sprintf("vtaltq-%d-%d", os.Getpid(), time.Now().UnixNano())}
	shell := fmt.Sprintf("cd %s && echo %s && HOME=%s TERM=xterm-256color %s -config %s; echo %s; exec sleep 600",
		home, altScreenScrollbackAfterQuitBefore, home, bin, cfg, altScreenScrollbackAfterQuitAfter)
	tm.run("new-session", "-d", "-x", fmt.Sprint(cols), "-y", fmt.Sprint(rows), shell)
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

// altScreenScrollbackAfterQuitDone counts finished turns in the
// session files under home.
func altScreenScrollbackAfterQuitDone(home string) int {
	paths, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
	n := 0
	for _, p := range paths {
		entries, err := history.Read(p)
		if err != nil {
			continue
		}
		for _, e := range entries {
			if e.Kind == "done" || e.Kind == "cancelled" {
				n++
			}
		}
	}
	return n
}

func altScreenScrollbackAfterQuitFmt(tm *tmuxApp, f string) string {
	return strings.TrimSpace(tm.run("display-message", "-p", "-t", "0", f))
}

func TestAltScreenScrollbackAfterQuit(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/basic.jsonl")
	tp, err := replay.Load(tape)
	if err != nil {
		t.Fatal(err)
	}
	quits := map[string]func(tm *tmuxApp){
		"slash-quit": func(tm *tmuxApp) {
			tm.keys("-l", "/quit")
			tm.waitFor("/quit")
			tm.keys("Enter")
		},
		"ctrl-c": func(tm *tmuxApp) {
			// First press may only arm the quit; a second one confirms.
			tm.keys("C-c")
			time.Sleep(300 * time.Millisecond)
			if !strings.Contains(tm.screen(), altScreenScrollbackAfterQuitAfter) {
				tm.keys("C-c")
			}
		},
	}
	for name, quit := range quits {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			tm, home := altScreenScrollbackAfterQuitStart(t, tape, 100, 30)
			if altScreenScrollbackAfterQuitFmt(tm, "#{alternate_on}") != "1" {
				t.Fatalf("bough running but pane not on the alt screen\nscreen:\n%s", tm.screen())
			}
			turns := 0
			for _, in := range tp.Inputs {
				tm.keys("-l", in)
				tm.keys("Enter")
				turns++
				deadline := time.Now().Add(60 * time.Second)
				for altScreenScrollbackAfterQuitDone(home) < turns {
					if time.Now().After(deadline) {
						t.Fatalf("turn %d never finished\nscreen:\n%s", turns, tm.screen())
					}
					time.Sleep(50 * time.Millisecond)
				}
			}
			tm.settled()
			quit(tm)
			tm.waitFor(altScreenScrollbackAfterQuitAfter)
			scr := tm.settled()

			if altScreenScrollbackAfterQuitFmt(tm, "#{alternate_on}") != "0" {
				t.Errorf("pane still on the alt screen after quit\nscreen:\n%s", scr)
			}
			if altScreenScrollbackAfterQuitFmt(tm, "#{cursor_flag}") != "1" {
				t.Errorf("cursor hidden after quit (no ?25h)\nscreen:\n%s", scr)
			}
			// Primary screen restored: the prelude is still there (on
			// screen or in the scrollback), above the exit marker.
			full := tm.run("capture-pane", "-p", "-S", "-", "-t", "0")
			pre := strings.Index(full, altScreenScrollbackAfterQuitBefore)
			post := strings.Index(full, altScreenScrollbackAfterQuitAfter)
			if pre < 0 || post < pre {
				t.Errorf("prelude marker lost or out of order after quit\npane+scrollback:\n%s", full)
			}
			// No TUI frame leaked onto the primary screen.
			for _, leak := range []string{"say something", "? keys", "list the files here"} {
				if strings.Contains(full, leak) {
					t.Errorf("TUI text %q left on the primary screen\npane+scrollback:\n%s", leak, full)
				}
			}
			// Whatever bough printed on exit (an optional summary line)
			// shows up once.
			between := strings.Split(full[pre+len(altScreenScrollbackAfterQuitBefore):post], "\n")
			seen := map[string]bool{}
			for _, l := range between {
				l = strings.TrimSpace(l)
				if l == "" {
					continue
				}
				if seen[l] {
					t.Errorf("exit line %q printed more than once\npane+scrollback:\n%s", l, full)
				}
				seen[l] = true
			}
		})
	}
}
