package vtreal

// settled() is what nearly every assertion in this package waits on,
// so its contract gets tests of its own against a scripted child
// instead of bough: the child's output timing is then exact.

import (
	"bufio"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"testing"
	"time"
)

// settleChildEnv names, in a re-exec'd test binary, the settleChildren
// entry to run instead of the tests.
const settleChildEnv = "VTREAL_SETTLE_CHILD"

// settleChildren are the scripted children, one per test by its name.
// They were sh scripts once, and each `sleep` between two writes was a
// fork+exec: on the macOS CI runner one of those stalled past settled's
// 120ms quiet window often enough that settled rightly returned the
// half-written screen, and the test blamed settled. A Go child's delays
// are timers, so the only gaps in its output are the ones written here.
var settleChildren = map[string]func(){
	"TestSettledWaitsOutAShortPause": func() {
		fmt.Print("FIRST")
		time.Sleep(80 * time.Millisecond)
		fmt.Print("SECOND")
		time.Sleep(30 * time.Second)
	},
	"TestSettledIgnoresOutputThatChangesNoText": func() {
		fmt.Print("STEADY")
		for {
			fmt.Print("\033[?25l")
			time.Sleep(10 * time.Millisecond)
		}
	},
	"TestSettledWaitsForTheReplyToInput":    replyAfter50ms,
	"TestSettledWaitsForTheReplyToRawInput": replyAfter50ms,
	"TestSettledReturnsAtOnceOnAQuietScreen": func() {
		fmt.Print("STEADY")
		time.Sleep(30 * time.Second)
	},
	"TestSettledSeesPastTickingChrome": func() {
		for i := 0; ; i++ {
			fmt.Printf("\r%c job 1 · %ds STEADY", []rune("⠋⠙⠹⠸")[i%4], i/20)
			time.Sleep(50 * time.Millisecond)
		}
	},
	"TestSettledWaitsOutChangingText": func() {
		for i := range 30 {
			fmt.Printf("\rcount %d", i)
			time.Sleep(50 * time.Millisecond)
		}
		time.Sleep(30 * time.Second)
	},
}

// replyAfter50ms echoes a line back only 50ms after it arrives.
func replyAfter50ms() {
	stty := exec.Command("stty", "-echo")
	stty.Stdin = os.Stdin
	_ = stty.Run() // before READY, so its fork is outside every timed window
	fmt.Print("READY")
	l, _ := bufio.NewReader(os.Stdin).ReadString('\n')
	time.Sleep(50 * time.Millisecond)
	fmt.Printf("GOT:%s", strings.TrimRight(l, "\r\n"))
	time.Sleep(30 * time.Second)
}

// settleApp runs this test's settleChildren entry, in a re-exec of the
// test binary, on a fresh terminal.
func settleApp(t *testing.T) *app {
	t.Helper()
	if settleChildren[t.Name()] == nil {
		t.Fatalf("no settle child for %s", t.Name())
	}
	self, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, 40, 5)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(self)
	cmd.Env = append(os.Environ(), settleChildEnv+"="+t.Name())
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_ = cmd.Process.Kill()
		_, _ = cmd.Process.Wait()
		_ = term.Close()
	})
	return &app{t: t, term: term, cmd: cmd, cols: 40, rows: 5}
}

// On a throttled terminal (64 bytes every 50ms) the screen keeps
// changing until the output has drained: settled must wait for the
// tail, whatever terminal it runs on.
func TestSettledWaitsForASlowDrain(t *testing.T) {
	t.Parallel()
	term, err := slowterminalNew(t, 40, 5, 64, 50*time.Millisecond)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command("sh", "-c", `printf 'HEAD'; i=0; while [ $i -lt 20 ]; do printf '0123456789'; i=$((i+1)); done; printf 'TAIL'; exec sleep 30`)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_ = cmd.Process.Kill()
		_, _ = cmd.Process.Wait()
		_ = term.Close()
	})
	a := &app{t: t, term: term, cmd: cmd, cols: 40, rows: 5}
	a.waitFor("HEAD")
	if s := a.settled(); !strings.Contains(s, "TAIL") {
		t.Fatalf("settled returned before a throttled terminal drained:\n%s", s)
	}
}

// A pause shorter than the quiet window is not the end of the frame:
// the renderer can stall between painting two halves of one screen.
func TestSettledWaitsOutAShortPause(t *testing.T) {
	t.Parallel()
	a := settleApp(t)
	a.waitFor("FIRST")
	if s := a.settled(); !strings.Contains(s, "FIRSTSECOND") {
		t.Fatalf("settled returned mid-frame, before a write 80ms later:\n%s", s)
	}
}

// Output that never changes the text (here a cursor-hide sequence
// every 10ms, like a status redraw that lands on identical cells) must
// not hold settled until its cap: the screen is what the caller asked
// about.
func TestSettledIgnoresOutputThatChangesNoText(t *testing.T) {
	t.Parallel()
	a := settleApp(t)
	a.waitFor("STEADY")
	began := time.Now()
	s := a.settled()
	if took := time.Since(began); took > 2*time.Second {
		t.Fatalf("settled took %s on a screen whose text was not changing", took)
	}
	if !strings.Contains(s, "STEADY") {
		t.Fatalf("settled screen lost the text:\n%s", s)
	}
}

// Input sent just before settled must be answered on the screen it
// returns: the quiet window starts at the call, not at the last output.
func TestSettledWaitsForTheReplyToInput(t *testing.T) {
	t.Parallel()
	a := settleApp(t) // replyAfter50ms
	a.waitFor("READY")
	time.Sleep(200 * time.Millisecond) // the screen has been quiet for longer than a window
	a.typeText("ping\r")
	if s := a.settled(); !strings.Contains(s, "GOT:ping") {
		t.Fatalf("settled returned before the reply to input sent just before it:\n%s", s)
	}
}

// A screen that has been quiet for a window with nothing sent to it
// since is settled already: waiting out a fresh window from the call
// cost a third of the package's ~1500 settles 120ms each for nothing.
func TestSettledReturnsAtOnceOnAQuietScreen(t *testing.T) {
	t.Parallel()
	a := settleApp(t)
	a.waitFor("STEADY")
	time.Sleep(200 * time.Millisecond) // quiet for longer than a window, no input
	began := time.Now()
	s := a.settled()
	if took := time.Since(began); took > 60*time.Millisecond {
		t.Fatalf("settled took %s on a screen quiet for 200ms with no input since", took)
	}
	if !strings.Contains(s, "STEADY") {
		t.Fatalf("settled screen lost the text:\n%s", s)
	}
}

// Raw bytes written to the pty are input too: the reply to them is
// waited for like the reply to a key.
func TestSettledWaitsForTheReplyToRawInput(t *testing.T) {
	t.Parallel()
	a := settleApp(t)
	a.waitFor("READY")
	time.Sleep(200 * time.Millisecond)
	if _, err := a.term.WriteInput([]byte("ping\r")); err != nil {
		t.Fatal(err)
	}
	if s := a.settled(); !strings.Contains(s, "GOT:ping") {
		t.Fatalf("settled returned before the reply to raw input sent just before it:\n%s", s)
	}
}

// A spinner and an elapsed timer tick on their own for as long as a
// turn or a job runs. A screen where only they move is as settled as
// it gets: settled returns it after a longer calm window instead of
// running to its 3.6 s cap.
func TestSettledSeesPastTickingChrome(t *testing.T) {
	t.Parallel()
	a := settleApp(t)
	a.waitFor("STEADY")
	began := time.Now()
	s := a.settled()
	if took := time.Since(began); took > 2*time.Second {
		t.Fatalf("settled took %s on a screen where only a spinner and a timer moved", took)
	}
	if !strings.Contains(s, "STEADY") {
		t.Fatalf("settled screen lost the text:\n%s", s)
	}
}

// Text that keeps changing is not chrome: a counter that runs for
// 1.5 s holds settled until it stops.
func TestSettledWaitsOutChangingText(t *testing.T) {
	t.Parallel()
	a := settleApp(t)
	a.waitFor("count 1")
	if s := a.settled(); !strings.Contains(s, "count 29") {
		t.Fatalf("settled returned while the text was still changing:\n%s", s)
	}
}
