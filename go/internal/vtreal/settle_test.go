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
