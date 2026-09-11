package vtreal

// rewind-while-ask-pending: turn 1 answers, turn 2 blocks on
// tools.ask, and a double esc is pressed while the ask is pending. The
// first esc declines the ask (a pending ask owns esc), the second
// cancels or arms; once idle a double esc opens the rewind menu and
// rewinding to before turn 2 forks the session. Asserted: the ask card
// and its composer routing are gone, the codemode block resolved (the
// turn records done/cancelled and a later prompt still runs), the fork
// holds turn 1 only, and '1' typed afterwards is composer text, not an
// ask answer.
//
// Both replies after the ask read FORK-REPLY: whether the second esc
// cancels the resumed turn before it consumes its reply is a race, and
// the tape cursor is sequential.

import (
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

func rewindWhileAskPendingStart(t *testing.T) *app {
	t.Helper()
	tape, err := filepath.Abs("testdata/replay/rewind-while-ask-pending.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	a := startCfg(t, 100, 30, askConfig(tape))
	followUpTurn(a, "turn one prompt", 1)
	a.waitFor("T1-ANSWER")
	a.typeText("turn two prompt")
	a.key(uv.KeyEnter, 0)
	a.waitFor("? Pick a color")
	a.settled()
	return a
}

func TestRewindWhileAskPending(t *testing.T) {
	t.Parallel()
	a := rewindWhileAskPendingStart(t)

	t.Run("DoubleEscResolvesAsk", func(t *testing.T) {
		// Two ESC bytes in one read parse as alt+esc; a human double
		// tap is tens of ms apart, so space them like one.
		a.key(uv.KeyEscape, 0)
		time.Sleep(150 * time.Millisecond)
		a.key(uv.KeyEscape, 0)
		if !a.waitDone(2, 20*time.Second) {
			t.Fatalf("turn 2 never recorded done/cancelled (codemode block stuck):\n%s", a.text())
		}
		a.waitUntil(func(s string) bool { return !cancelSpinner.MatchString(s) }, "the spinner to stop")
		s := a.settled()
		if strings.Contains(s, "waiting for you") || strings.Contains(s, askPendingPlaceholder) {
			t.Errorf("ask still pending after double esc:\n%s", s)
		}
		if got := askSteerCancelAnswers(a); len(got) != 1 || got[0] != "(declined)" {
			t.Errorf("ask/answer entries = %q, want one (declined)", got)
		}
		a.check("after double esc")
	})

	t.Run("RewindToTurnOne", func(t *testing.T) {
		a.waitUntil(func(string) bool { return strings.Contains(followUpComposer(a), "say something") },
			"an empty composer")
		time.Sleep(rewindWhileAskPendingEscWait) // let any armed esc expire
		a.key(uv.KeyEscape, 0)
		// The hint may be stale from the ask-time double esc, so it does
		// not prove this esc landed: space the second one as above.
		a.waitFor("press esc again to rewind")
		time.Sleep(150 * time.Millisecond)
		a.key(uv.KeyEscape, 0)
		a.waitFor("(current)")
		a.key(uv.KeyUp, 0) // turn two: back to before it
		a.key(uv.KeyEnter, 0)
		a.waitUntil(func(string) bool { return strings.HasPrefix(followUpComposer(a), "> turn two prompt") },
			"the rewound prompt in the composer")
		s := a.settled()
		if strings.Contains(s, "Pick a color") || !strings.Contains(s, "T1-ANSWER") {
			t.Errorf("transcript after rewind still shows turn 2's ask or lost turn 1:\n%s", s)
		}
		if in := followUpKinds(a, "input"); strings.Join(in, "|") != "turn one prompt" {
			t.Errorf("fork inputs = %q, want turn 2 truncated", in)
		}
		a.check("after rewind")
	})

	t.Run("OneTypesIntoComposer", func(t *testing.T) {
		a.key(uv.KeyEscape, 0)
		a.waitFor("press esc again to clear the draft")
		a.key(uv.KeyEscape, 0)
		a.waitFor("say something")
		a.typeText("1")
		a.waitUntil(func(string) bool { return strings.HasPrefix(followUpComposer(a), "> 1") },
			"'1' in the composer")
		if got := askSteerCancelAnswers(a); len(got) != 1 {
			t.Errorf("'1' was taken as an ask answer: %q", got)
		}
		if strings.Contains(a.settled(), "→ chartreuse") {
			t.Errorf("'1' answered the rewound ask:\n%s", a.text())
		}
	})

	t.Run("SecondPromptWorks", func(t *testing.T) {
		a.key(uv.KeyBackspace, 0)
		a.typeText("fork prompt")
		a.key(uv.KeyEnter, 0)
		followUpWaitDone(a, 2)
		a.waitFor("FORK-REPLY")
		if in := followUpKinds(a, "input"); strings.Join(in, "|") != "turn one prompt|fork prompt" {
			t.Errorf("fork inputs = %q:\n%s", in, a.text())
		}
		a.check("fork turn")
	})
}

// rewindWhileAskPendingEscWait outlasts the ui's 3 s esc arming window.
const rewindWhileAskPendingEscWait = 3100 * time.Millisecond
