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
