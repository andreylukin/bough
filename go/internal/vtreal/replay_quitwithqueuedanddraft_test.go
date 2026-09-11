package vtreal

// Quit with a queued follow-up and a typed draft: alpha streams, beta
// is queued with alt+enter, a draft is typed, then the quit key is
// pressed until bough exits (the first press cancels the turn, idle
// presses arm and quit — plugins/ui/stop.go quitPress). The process
// must exit 0 and leave the alt screen; on --resume the queued line
// has run at most once and is never re-sent by the resume itself, and
// the unsent draft was never submitted. Drafts are not persisted
// across processes by design (composer.go keeps dropped drafts for the
// session only), so "recoverable" here means not silently sent.

import (
	"fmt"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const quitWithQueuedAndDraftDraft = "unsentdraftxyz"

func quitWithQueuedAndDraftCount(xs []string, s string) int {
	n := 0
	for _, x := range xs {
		if x == s {
			n++
		}
	}
	return n
}

func quitWithQueuedAndDraftInputs(t *testing.T, session string) []string {
	t.Helper()
	var out []string
	for _, e := range crashResumeIntegrityLines(t, session) {
		if e.Kind == "input" {
			s, _ := e.Data["text"].(string)
			out = append(out, s)
		}
	}
	return out
}

func TestQuitWithQueuedAndDraft(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	tape := followUpTape(t)
	a := crashResumeIntegrityStart(t, home, crashResumeIntegrityConfig(tape, 400))
	a.typeText("alpha")
	a.key(uv.KeyEnter, 0)
	a.waitFor("one two") // alpha is streaming
	a.typeText("beta")
	a.key(uv.KeyEnter, uv.ModAlt)
	a.waitFor("beta (queued)")
	a.typeText(quitWithQueuedAndDraftDraft)
	a.waitFor(quitWithQueuedAndDraftDraft)

	done := make(chan error, 1)
	go func() { done <- a.term.Wait(a.cmd) }()
	var exitErr error
	exited := false
	for i := 0; i < 30 && !exited; i++ {
		a.key('c', uv.ModCtrl)
		select {
		case exitErr = <-done:
			exited = true
		case <-time.After(400 * time.Millisecond):
		}
	}
	if !exited {
		select {
		case exitErr = <-done:
			exited = true
		case <-time.After(8 * time.Second):
		}
	}
	if !exited {
		t.Fatalf("bough did not exit under repeated ctrl+c:\n%s", a.text())
	}
	last := a.text()

	t.Run("clean_exit", func(t *testing.T) {
		if exitErr != nil {
			t.Errorf("exit: %v\n%s", exitErr, last)
		}
		left := false
		for i := 0; i < 100 && !left; i++ {
			left = !a.term.Snapshot().AltScreen
			if !left {
				time.Sleep(50 * time.Millisecond)
			}
		}
		if !left {
			t.Errorf("still in the alt screen after exit")
		}
	})

	session := crashResumeIntegritySession(t, home)
	before := quitWithQueuedAndDraftInputs(t, session)

	t.Run("on_disk_before_resume", func(t *testing.T) {
		if n := quitWithQueuedAndDraftCount(before, "alpha"); n != 1 {
			t.Errorf("alpha recorded %d times: %q", n, before)
		}
		if n := quitWithQueuedAndDraftCount(before, "beta"); n > 1 {
			t.Errorf("queued beta sent %d times: %q", n, before)
		}
		for _, s := range before {
			if strings.Contains(s, quitWithQueuedAndDraftDraft) {
				t.Errorf("the unsent draft was submitted: %q", before)
			}
		}
	})

	id := strings.TrimSuffix(filepath.Base(session), ".jsonl")
	b := crashResumeIntegrityStart(t, home, crashResumeIntegrityConfig(tape, 0), "--resume", id)
	b.waitFor("resumed ")
	time.Sleep(1500 * time.Millisecond) // a stray re-send would start by now
	s := b.settled()
	b.check("resumed after quit with queued + draft")

	t.Run("resume_sends_nothing", func(t *testing.T) {
		after := quitWithQueuedAndDraftInputs(t, session)
		if fmt.Sprint(after) != fmt.Sprint(before) {
			t.Errorf("resume changed the inputs: before %q after %q\n%s", before, after, s)
		}
	})

	t.Run("resumed_screen_matches_log", func(t *testing.T) {
		if strings.Contains(s, "(queued)") {
			t.Errorf("resumed transcript still shows a queued line:\n%s", s)
		}
		want := quitWithQueuedAndDraftCount(before, "beta")
		if got := strings.Count(s, "❯ beta"); got != want {
			t.Errorf("resumed screen shows beta %d times, log has %d:\n%s", got, want, s)
		}
		if strings.Contains(s, "❯ "+quitWithQueuedAndDraftDraft) {
			t.Errorf("the draft shows as a sent prompt:\n%s", s)
		}
	})
}
