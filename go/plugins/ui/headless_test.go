package ui

import (
	"bytes"
	"testing"
)

// Headless stream contract: "[assistant]"/"[done]" on stdout,
// "[error]" on stderr, and any error makes the exit code 1.
func TestHeadlessErrorsToStderrAndExitCode(t *testing.T) {
	var out, errb bytes.Buffer
	oldOut, oldErr := hlOut, hlErr
	hlOut, hlErr = &out, &errb
	defer func() {
		hlOut, hlErr = oldOut, oldErr
		hlErrored.Store(false)
	}()
	hlErrored.Store(false)

	hlPending.Add(1)
	hlPrint(Event{Kind: "assistant", Text: "hi"})
	hlPrint(Event{Kind: "done"})
	if got := ExitCode(); got != 0 {
		t.Fatalf("clean turn: ExitCode = %d, want 0", got)
	}

	hlPending.Add(1)
	hlPrint(Event{Kind: "error", Text: "boom"})
	hlPrint(Event{Kind: "done"})

	if s := out.String(); s != "[assistant] hi\n[done] \n[done] \n" {
		t.Fatalf("stdout = %q", s)
	}
	if s := errb.String(); s != "[error] boom\n" {
		t.Fatalf("stderr = %q", s)
	}
	if got := ExitCode(); got != 1 {
		t.Fatalf("errored turn: ExitCode = %d, want 1", got)
	}
}

// A line that arrives between "[assistant]" and "[done]" steers the
// running turn: it prints as "[steer]", the turn's own done pays for
// it, and the pending count ends at zero — the drain neither returns
// early nor goes negative.
func TestHeadlessSteerMidTurnKeepsPendingBalanced(t *testing.T) {
	var out bytes.Buffer
	oldOut := hlOut
	hlOut = &out
	hlMu.Lock()
	oldSteer := hlSteer
	hlSteer = func(string) bool { return true } // a turn runs: the loop takes it
	hlMu.Unlock()
	defer func() {
		hlOut = oldOut
		hlMu.Lock()
		hlSteer = oldSteer
		hlMu.Unlock()
	}()

	hlPending.Store(1) // the turn hlSubmit("a") started
	hlPrint(Event{Kind: "assistant", Text: "reply to a"})
	if !hlSteerLine("b") {
		t.Fatal("mid-turn line should steer")
	}
	hlPrint(Event{Kind: "steer", Text: "b"})
	hlPrint(Event{Kind: "assistant", Text: "reply to b"})
	hlPrint(Event{Kind: "done"})
	if n := hlPending.Load(); n != 0 {
		t.Fatalf("hlPending = %d after the one done, want 0", n)
	}
	want := "[assistant] reply to a\n[steer] b\n[assistant] reply to b\n[done] \n"
	if s := out.String(); s != want {
		t.Fatalf("stdout = %q, want %q", s, want)
	}
}
