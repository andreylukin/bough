package vtreal

// A prompt and two submits arriving in ONE PTY write (a key-repeat or
// a fat-fingered double tap over a slow link): the first enter sends
// the line, the second meets an empty composer and must do nothing —
// no empty turn, no duplicate, no empty queued follow-up. The history
// file's "input" entries are what the tape saw.

import (
	"fmt"
	"strings"
	"testing"
	"time"
)

func doubleSubmitEnterBurstWrite(a *app, s string) {
	a.t.Helper()
	if _, err := a.term.pty.Write([]byte(s)); err != nil {
		a.t.Fatal(err)
	}
}

func TestDoubleSubmitEnterBurst(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct{ name, bytes string }{
		{"enter-enter", "alpha\r\r"},
		{"enter-alt-enter", "alpha\r\x1b\r"},
		{"enter-enter-enter", "alpha\r\r\r"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			a := startCfg(t, 100, 30, replayConfig(followUpTape(t)))
			doubleSubmitEnterBurstWrite(a, tc.bytes)
			if !a.waitDone(1, 30*time.Second) {
				t.Fatalf("turn never finished:\n%s", a.settled())
			}
			a.waitFor("REPLY-ALPHA")
			time.Sleep(1500 * time.Millisecond) // let a stray second turn show up
			s := a.settled()
			if in := fmt.Sprint(followUpKinds(a, "input")); in != "[alpha]" {
				t.Errorf("inputs = %s, want [alpha]:\n%s", in, s)
			}
			if n := a.doneCount(); n != 1 {
				t.Errorf("%d turns ended, want 1:\n%s", n, s)
			}
			if strings.Contains(s, "(queued)") || strings.Contains(s, "REPLY-BETA") {
				t.Errorf("a second submit leaked through:\n%s", s)
			}
			if c := strings.TrimSpace(strings.TrimPrefix(followUpComposer(a), ">")); c != "" && c != "say something" {
				t.Errorf("composer = %q, want empty:\n%s", c, s)
			}
			a.check("after burst " + tc.name)
		})
	}
}
