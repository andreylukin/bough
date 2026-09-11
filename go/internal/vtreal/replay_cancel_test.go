package vtreal

// Cancellation on a real terminal: esc during a streamed reply, the
// double-esc that clears a draft, and the two-press quit that has to
// hand the terminal back (alt screen off, mouse reporting off).
//
// The tape (testdata/replay/cancel.jsonl) is built so the cancel is
// observable: the first reply streams ALPHASTART, two hundred filler
// words and only then ALPHAEND, and the SECOND reply is BETAREPLY. If
// esc really aborted the turn, ALPHAEND never lands; if the aborted
// reply was really consumed, the next input renders BETAREPLY rather
// than replaying ALPHA.

import (
	"fmt"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
	"github.com/charmbracelet/x/ansi"
)

// cancelSpinner is the running-turn chip in the status bar: a MiniDot
// frame followed by the elapsed time ("⠹ 3s").
var cancelSpinner = regexp.MustCompile(`[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏] \d+[sm]`)

// cancelConfig is replayConfig with a streaming delay, so a reply is
// still arriving when the test presses esc.
func cancelConfig(tape string, delayMS int) string {
	cfg := replayConfig(tape)
	out := strings.Replace(cfg,
		fmt.Sprintf("config: {file: %q}", tape),
		fmt.Sprintf("config: {file: %q, delay_ms: %d}", tape, delayMS),
		1)
	if out == cfg {
		panic("cancelConfig: replayConfig's llm row changed shape; delay_ms not applied")
	}
	return out
}

func cancelTape(t *testing.T) string {
	t.Helper()
	p, err := filepath.Abs("testdata/replay/cancel.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	return p
}

// Esc mid-reply stops the turn: a cancelled marker lands, the rest of
// the reply never does, the spinner goes away and the composer comes
// back — and the next input starts a fresh turn on the NEXT tape reply.
func TestCancelEscMidReply(t *testing.T) {
	t.Parallel()
	a := startCfg(t, 100, 30, cancelConfig(cancelTape(t), 120))

	a.typeText("start the long one")
	a.key(uv.KeyEnter, 0)
	a.waitFor("ALPHASTART")
	if s := a.text(); !cancelSpinner.MatchString(s) {
		t.Fatalf("no spinner while the reply streams:\n%s", s)
	}

	a.key(uv.KeyEsc, 0)
	a.waitFor("■ cancelled")
	a.waitUntil(func(s string) bool { return !cancelSpinner.MatchString(s) },
		"the spinner to stop after the cancel")

	s := a.settled()
	if strings.Contains(s, "ALPHAEND") {
		t.Fatalf("the reply kept streaming after esc:\n%s", s)
	}
	ls := strings.Split(s, "\n")
	r := composerRow(ls)
	if r < 0 || r < len(ls)-3 {
		t.Fatalf("composer not restored at the bottom (row %d of %d):\n%s", r, len(ls), s)
	}
	if !cancelComposerEmpty(ls[r]) {
		t.Fatalf("composer is not empty after the cancel (%q):\n%s", ls[r], s)
	}
	a.check("after cancel")

	// A fresh turn: the aborted reply was consumed, so this one gets
	// the tape's second reply.
	a.typeText("second")
	a.key(uv.KeyEnter, 0)
	a.waitFor("BETAREPLY")
	a.waitUntil(func(s string) bool { return !cancelSpinner.MatchString(s) },
		"the spinner to stop after the second turn")
	s = a.settled()
	if strings.Contains(s, "ALPHAEND") {
		t.Fatalf("the cancelled reply was re-served on the next turn:\n%s", s)
	}
	a.check("after the next turn")
}

// Idle with a draft: the first esc only arms, the second clears.
func TestCancelDoubleEscClearsDraft(t *testing.T) {
	t.Parallel()
	a := start(t, 100, 30)

	a.typeText("a draft worth keeping")
	a.waitFor("> a draft worth keeping")

	a.key(uv.KeyEsc, 0)
	a.waitFor("press esc again to clear the draft")
	if s := a.settled(); !strings.Contains(s, "> a draft worth keeping") {
		t.Fatalf("one esc already cleared the draft:\n%s", s)
	}

	a.key(uv.KeyEsc, 0)
	a.waitFor("draft cleared")
	s := a.settled()
	if strings.Contains(s, "a draft worth keeping") {
		t.Fatalf("draft still on screen after the second esc:\n%s", s)
	}
	ls := strings.Split(s, "\n")
	r := composerRow(ls)
	if r < 0 || !cancelComposerEmpty(ls[r]) {
		t.Fatalf("composer not empty after the second esc (row %d):\n%s", r, s)
	}
}

// Two ctrl+c hand the terminal back: alt screen off and mouse
// reporting off, or the user's shell is left clicking into nothing.
func TestCancelCtrlCRestoresTerminal(t *testing.T) {
	t.Parallel()
	a := start(t, 100, 30)

	snap := a.term.Snapshot()
	if !snap.AltScreen {
		t.Fatalf("not in the alt screen before the quit:\n%s", a.text())
	}

	a.key('c', uv.ModCtrl)
	a.waitFor("ctrl+c")
	a.key('c', uv.ModCtrl)

	done := make(chan error, 1)
	go func() { done <- a.term.Wait(a.cmd) }()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("exit: %v\nscreen:\n%s", err, a.text())
		}
	case <-time.After(15 * time.Second):
		t.Fatalf("process did not exit after two ctrl+c:\n%s", a.text())
	}

	// The process is gone; the emulator may still be draining the
	// bytes that restore the terminal.
	var left Snapshot
	ok := false
	for range 100 {
		left = a.term.Snapshot()
		if !left.AltScreen && !cancelMouseOn(left) {
			ok = true
			break
		}
		time.Sleep(50 * time.Millisecond)
	}
	if !ok {
		t.Fatalf("terminal not restored after exit (alt=%v dec=%v):\n%s",
			left.AltScreen, left.DEC, a.text())
	}
}

// cancelComposerEmpty reports whether the composer row holds no draft:
// an empty composer draws its placeholder.
func cancelComposerEmpty(row string) bool {
	return strings.TrimSpace(row) == ">" || strings.HasPrefix(row, "> say something")
}

// cancelMouseOn reports whether any mouse reporting mode is still set.
func cancelMouseOn(s Snapshot) bool {
	for _, mode := range []ansi.DECMode{
		ansi.ButtonEventMouseMode, ansi.SgrExtMouseMode,
		ansi.NormalMouseMode, ansi.AnyEventMouseMode,
	} {
		if st, ok := s.DEC[mode]; ok && st.IsSet() {
			return true
		}
	}
	return false
}
