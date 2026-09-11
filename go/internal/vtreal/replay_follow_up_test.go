package vtreal

// Follow-up and double-esc (plugins/ui/stop.go escPress, rewind.go):
// esc esc clears a draft (Up brings it back) or, on an empty composer,
// opens the rewind menu; picking a turn forks the session before it
// and puts its prompt back in the composer; alt+enter (follow_up)
// queues a line mid-turn instead of steering. The replay model hands
// out tape replies strictly in order, so which reply lands next shows
// how many times the model was asked.

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

func followUpTape(t *testing.T) string {
	tape, err := filepath.Abs("testdata/replay/follow-up.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	return tape
}

// followUpNewest is the most recently written session file: a rewind
// forks into a new one.
func followUpNewest(a *app) []history.Entry {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var best string
	var at time.Time
	for _, p := range paths {
		if st, err := os.Stat(p); err == nil && !st.ModTime().Before(at) {
			best, at = p, st.ModTime()
		}
	}
	if best == "" {
		return nil
	}
	entries, _ := history.Read(best)
	return entries
}

// followUpWaitDone waits until the newest session holds n finished
// turns (a fork copies its dones into the new file).
func followUpWaitDone(a *app, n int) {
	a.t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		c := 0
		for _, e := range followUpNewest(a) {
			if e.Kind == "done" || e.Kind == "cancelled" {
				c++
			}
		}
		if c >= n {
			return
		}
		time.Sleep(50 * time.Millisecond)
	}
	a.t.Fatalf("turn %d never finished:\n%s", n, a.text())
}

// followUpKinds lists the text of the newest session's entries of kind.
func followUpKinds(a *app, kind string) []string {
	var out []string
	for _, e := range followUpNewest(a) {
		if e.Kind == kind {
			s, _ := e.Data["text"].(string)
			out = append(out, s)
		}
	}
	return out
}

func followUpComposer(a *app) string {
	ls := a.lines()
	if r := composerRow(ls); r >= 0 {
		return ls[r]
	}
	return ""
}

func followUpTurn(a *app, text string, n int) {
	a.t.Helper()
	a.typeText(text)
	a.key(uv.KeyEnter, 0)
	followUpWaitDone(a, n)
}

func TestFollowUpEscClearsDraft(t *testing.T) {
	t.Parallel()
	a := start(t, 100, 30)
	a.typeText("draft one")
	a.waitFor("> draft one")
	a.key(uv.KeyEscape, 0)
	a.waitFor("press esc again to clear the draft")
	if !strings.Contains(followUpComposer(a), "draft one") {
		t.Fatalf("a single esc cleared the draft:\n%s", a.text())
	}
	a.key(uv.KeyEscape, 0)
	a.waitFor("draft cleared")
	a.waitFor("say something")
	if c := followUpComposer(a); strings.Contains(c, "draft one") {
		t.Fatalf("double esc left the draft (%q):\n%s", c, a.text())
	}
	a.key(uv.KeyUp, 0)
	a.waitUntil(func(string) bool { return strings.Contains(followUpComposer(a), "draft one") },
		"Up to bring the cleared draft back")
	a.check("after recall")
}

func TestFollowUpEscEmptyComposerNothingToRewind(t *testing.T) {
	t.Parallel()
	a := startCfg(t, 100, 30, replayConfig(followUpTape(t)))
	a.key(uv.KeyEscape, 0)
	a.waitFor("press esc again to rewind")
	a.key(uv.KeyEscape, 0)
	a.waitFor("nothing to rewind to")
	a.check("no turns")
}

func TestFollowUpRewindSecondTurn(t *testing.T) {
	t.Parallel()
	a := startCfg(t, 100, 30, replayConfig(followUpTape(t)))
	followUpTurn(a, "alpha", 1)
	a.waitFor("REPLY-ALPHA")
	followUpTurn(a, "beta", 2)
	a.waitFor("REPLY-BETA")

	a.key(uv.KeyEscape, 0)
	a.waitFor("press esc again to rewind")
	a.key(uv.KeyEscape, 0)
	a.waitFor("(current)")
	s := a.text()
	if !strings.Contains(s, "alpha") || !strings.Contains(s, "beta") {
		t.Fatalf("rewind menu does not list both turns:\n%s", s)
	}
	a.key(uv.KeyUp, 0) // from "(current)" to "beta"
	a.key(uv.KeyEnter, 0)

	// Back to before "beta": its prompt is in the composer, its reply gone.
	a.waitUntil(func(string) bool { return strings.HasPrefix(followUpComposer(a), "> beta") },
		"the rewound prompt in the composer")
	s = a.settled()
	if strings.Contains(s, "REPLY-BETA") {
		t.Errorf("rewound turn still in the transcript:\n%s", s)
	}
	if !strings.Contains(s, "REPLY-ALPHA") {
		t.Errorf("turn before the rewind point lost:\n%s", s)
	}
	if in := followUpKinds(a, "input"); fmt.Sprint(in) != "[alpha]" {
		t.Errorf("forked session inputs = %q, want [alpha]:\n%s", in, s)
	}
	a.check("after rewind")

	// Resend: the model is asked exactly once more, so the NEXT tape
	// reply lands — not beta's again, not end of tape.
	a.key(uv.KeyEnter, 0)
	followUpWaitDone(a, 2)
	a.waitFor("REPLY-GAMMA")
	s = a.settled()
	if strings.Contains(s, "REPLY-BETA") || strings.Contains(s, "end of tape") {
		t.Errorf("tape out of step after the rewind:\n%s", s)
	}
	if in := followUpKinds(a, "input"); fmt.Sprint(in) != "[alpha beta]" {
		t.Errorf("forked session inputs = %q, want [alpha beta]:\n%s", in, s)
	}
	for _, r := range followUpKinds(a, "assistant") {
		if strings.Contains(r, "REPLY-BETA") {
			t.Errorf("rewound reply resurfaced in the forked session:\n%s", s)
		}
	}
	a.check("after resend")
}

func TestFollowUpAltEnterQueuesMidTurn(t *testing.T) {
	t.Parallel()
	tape := followUpTape(t)
	cfg := strings.Replace(replayConfig(tape),
		fmt.Sprintf("config: {file: %q}\n", tape),
		fmt.Sprintf("config: {file: %q, delay_ms: 400}\n", tape), 1)
	if !strings.Contains(cfg, "delay_ms: 400") {
		t.Fatalf("could not add delay_ms to the replay row:\n%s", cfg)
	}
	a := startCfg(t, 100, 30, cfg)
	a.typeText("alpha")
	a.key(uv.KeyEnter, 0)
	a.waitFor("one two") // alpha's reply is streaming
	a.typeText("beta")
	a.key(uv.KeyEnter, uv.ModAlt)
	a.waitFor("beta (queued)")
	followUpWaitDone(a, 2)
	a.waitFor("REPLY-BETA")
	s := a.settled()
	if strings.Contains(s, "(queued)") || strings.Contains(s, "steer") {
		t.Errorf("follow-up still queued or taken as a steer:\n%s", s)
	}
	if in := followUpKinds(a, "input"); fmt.Sprint(in) != "[alpha beta]" {
		t.Errorf("inputs = %q, want [alpha beta]:\n%s", in, s)
	}
	a.check("after follow-up")
}
