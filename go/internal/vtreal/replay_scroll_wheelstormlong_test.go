package vtreal

// Wheel storm over the largest real sessions, on tmux (a real terminal
// that drains output on its own; x/vt's SafeEmulator holds its lock
// while SendMouse blocks on the input pipe, and 2000 no-delay wheel
// events deadlock the harness, not bough). Replay every turn, then
// 2000 raw SGR wheel events in 50-event bursts, then far past both
// bounds. bough must stay alive, paint no panic, answer a key, leak no
// SGR bytes into the composer, and leave mouse reporting off on exit.
//
// Reads real tapes from ~/.bough/history (copied to a temp dir), so it
// runs only with BOUGH_KNOWN_SCROLL=1.

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/replay"
)

func wheelStormTapes(t *testing.T) []string {
	home, _ := os.UserHomeDir()
	paths, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
	sort.Slice(paths, func(i, j int) bool {
		a, _ := os.Stat(paths[i])
		b, _ := os.Stat(paths[j])
		return a.Size() > b.Size()
	})
	if len(paths) > 3 {
		paths = paths[:3]
	}
	var out []string
	for _, p := range paths {
		b, err := os.ReadFile(p)
		if err != nil {
			t.Fatal(err)
		}
		cp := filepath.Join(t.TempDir(), filepath.Base(p))
		if err := os.WriteFile(cp, b, 0o644); err != nil {
			t.Fatal(err)
		}
		out = append(out, cp)
	}
	return out
}

func TestWheelStormLongRealSessions(t *testing.T) {
	if os.Getenv("BOUGH_KNOWN_SCROLL") != "1" {
		t.Skip("set BOUGH_KNOWN_SCROLL=1 (reads ~/.bough/history)")
	}
	if _, err := exec.LookPath("tmux"); err != nil {
		t.Skip("tmux not installed")
	}
	tapes := wheelStormTapes(t)
	if len(tapes) == 0 {
		t.Skip("no history tapes")
	}
	for _, tape := range tapes {
		for _, sz := range [][2]int{{100, 30}, {200, 50}} {
			t.Run(fmt.Sprintf("%s/%dx%d", filepath.Base(tape)[:8], sz[0], sz[1]), func(t *testing.T) {
				wheelStorm(t, tape, sz[0], sz[1])
			})
		}
	}
}

func stormDone(home string) int {
	paths, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
	n := 0
	for _, p := range paths {
		es, _ := history.Read(p)
		for _, e := range es {
			if e.Kind == "done" || e.Kind == "cancelled" {
				n++
			}
		}
	}
	return n
}

func wheelStorm(t *testing.T, tape string, cols, rows int) {
	tp, err := replay.Load(tape)
	if err != nil {
		t.Fatal(err)
	}
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(replayConfig(tape)), 0o644); err != nil {
		t.Fatal(err)
	}
	stderr := filepath.Join(home, "stderr.log")
	tm := &tmuxApp{t: t, sock: fmt.Sprintf("vtstorm-%d-%d", os.Getpid(), time.Now().UnixNano())}
	shell := fmt.Sprintf("cd %s && HOME=%s TERM=xterm-256color BOUGH_WEB_ADDR=127.0.0.1:0 %s -config %s 2>%s; echo BOUGH-EXIT=$?; sleep 600",
		home, home, bin, cfg, stderr)
	tm.run("new-session", "-d", "-x", fmt.Sprint(cols), "-y", fmt.Sprint(rows), shell)
	t.Cleanup(func() { _ = exec.Command("tmux", "-L", tm.sock, "kill-server").Run() })
	tm.waitFor("say something")

	dead := func(where string) {
		t.Helper()
		s := tm.screen()
		errb, _ := os.ReadFile(stderr)
		if strings.Contains(s, "BOUGH-EXIT=") || panicky.MatchString(s) || panicky.Match(errb) {
			t.Fatalf("%s: bough died\nscreen:\n%s\nstderr:\n%s", where, s, errb)
		}
	}
	turns := 0
	for _, in := range tp.Inputs {
		if strings.HasPrefix(in, "/") {
			continue
		}
		tm.keys("-l", strings.ReplaceAll(in, "\n", " "))
		tm.keys("Enter")
		turns++
		deadline := time.Now().Add(90 * time.Second)
		for stormDone(home) < turns && time.Now().Before(deadline) {
			time.Sleep(100 * time.Millisecond)
		}
		if stormDone(home) < turns {
			t.Logf("turn %d never finished (tape drift); storming anyway", turns)
			break
		}
	}
	dead("replayed")
	idle := func() {
		t.Helper()
		// Background-job wakes on the big tape start extra turns; storm
		// and type only once the status bar shows no spinner.
		// A wake turn that outlives the tape is cancelled with esc.
		busy := func(s string) bool {
			return strings.ContainsAny(s, "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏") || strings.Contains(s, "writing code")
		}
		for i := 0; i < 600 && busy(tm.screen()); i++ {
			if i%20 == 19 {
				tm.keys("Escape")
			}
			time.Sleep(200 * time.Millisecond)
		}
		if s := tm.screen(); busy(s) {
			t.Fatalf("never idle:\n%s", s)
		}
	}
	idle()

	x, y := cols/2, rows/3
	burst := func(btn, n int) {
		seq := strings.Repeat(fmt.Sprintf("\x1b[<%d;%d;%dM", btn, x, y), n)
		tm.keys("-l", seq)
	}
	for sent := 0; sent < 2000; sent += 50 {
		btn := 64 // wheel up
		if (sent/50)%2 == 1 {
			btn = 65
		}
		burst(btn, 50)
		burst(66, 1) // wheel left
		burst(67, 1) // wheel right
		tm.keys("-l", fmt.Sprintf("\x1b[<35;%d;%dM", x+1, y+1))
		burst(btn, 1)
	}
	dead("storm")
	for range 60 {
		burst(64, 50)
	}
	dead("past top")
	for range 120 {
		burst(65, 50)
	}
	dead("past bottom")
	idle()
	before := tm.settled()
	tm.keys("-l", "z")
	tm.waitUntil(func(s string) bool { return s != before && strings.Contains(s, "> z") }, "key after storm")
	if s := tm.screen(); strings.Contains(s, "[<") || strings.Contains(s, ";M") {
		t.Errorf("SGR mouse fragment leaked as text:\n%s", s)
	}
	tm.keys("BSpace")
	tm.keys("C-c")
	tm.waitFor("ctrl+c")
	tm.keys("C-c")
	tm.waitFor("BOUGH-EXIT=")
	flags := tm.run("display", "-p", "-t", "0", "#{mouse_any_flag}#{mouse_button_flag}#{mouse_standard_flag}#{mouse_sgr_flag}")
	if strings.Contains(flags, "1") {
		t.Errorf("mouse modes still set after exit: %q", flags)
	}
	if !strings.Contains(tm.screen(), "BOUGH-EXIT=0") {
		errb, _ := os.ReadFile(stderr)
		t.Errorf("non-zero exit:\n%s\nstderr:\n%s", tm.screen(), errb)
	}
}
