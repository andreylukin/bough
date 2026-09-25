package vtreal

// settled() is what nearly every assertion in this package waits on,
// so its contract gets tests of its own against a scripted child
// instead of bough: the child's output timing is then exact.

import (
	"os/exec"
	"strings"
	"testing"
	"time"
)

// settleApp runs sh -c script on a fresh terminal.
func settleApp(t *testing.T, script string) *app {
	t.Helper()
	term, err := NewTerminal(t, 40, 5)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command("sh", "-c", script)
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
	a := settleApp(t, "printf FIRST; sleep 0.08; printf SECOND; exec sleep 30")
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
	a := settleApp(t, `printf STEADY; while :; do printf '\033[?25l'; sleep 0.01; done`)
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
	// The child echoes a line back only after 50ms.
	a := settleApp(t, `stty -echo; printf READY; read l; sleep 0.05; printf "GOT:%s" "$l"; exec sleep 30`)
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
	a := settleApp(t, `printf STEADY; exec sleep 30`)
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
	a := settleApp(t, `stty -echo; printf READY; read l; sleep 0.05; printf "GOT:%s" "$l"; exec sleep 30`)
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
	a := settleApp(t, `i=0; while :; do for c in ⠋ ⠙ ⠹ ⠸; do printf '\r%s job 1 · %ds STEADY' $c $((i/20)); i=$((i+1)); sleep 0.05; done; done`)
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
	a := settleApp(t, `i=0; while [ $i -lt 30 ]; do printf '\rcount %d' $i; i=$((i+1)); sleep 0.05; done; exec sleep 30`)
	a.waitFor("count 1")
	if s := a.settled(); !strings.Contains(s, "count 29") {
		t.Fatalf("settled returned while the text was still changing:\n%s", s)
	}
}
