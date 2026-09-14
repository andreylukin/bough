package ui

import (
	"bytes"
	"testing"
)

// A code block that fails, followed by the model retrying and finishing, is
// a successful run: the wiki ingest recovered from a SyntaxError, updated
// its pages, and still exited 1. An error the turn ends on stays exit 1.
func TestHeadlessRecoveredErrorExitsZero(t *testing.T) {
	var out, errb bytes.Buffer
	oldOut, oldErr := hlOut, hlErr
	hlOut, hlErr = &out, &errb
	defer func() {
		hlOut, hlErr = oldOut, oldErr
		hlErrored.Store(false)
		hlTurnErr.Store(false)
	}()
	hlErrored.Store(false)
	hlTurnErr.Store(false)

	hlPending.Add(1)
	hlPrint(Event{Kind: "code", Text: "console.log(x"})
	hlPrint(Event{Kind: "error", Text: "error: SyntaxError"})
	hlPrint(Event{Kind: "assistant", Text: "retrying"})
	hlPrint(Event{Kind: "code", Text: "console.log(1)"})
	hlPrint(Event{Kind: "result", Text: "1"})
	hlPrint(Event{Kind: "done"})
	if got := ExitCode(); got != 0 {
		t.Fatalf("recovered turn: ExitCode = %d, want 0", got)
	}
	if errb.String() != "[error] error: SyntaxError\n" {
		t.Errorf("stderr = %q: the error must still be reported", errb.String())
	}

	hlPending.Add(1)
	hlPrint(Event{Kind: "assistant", Text: "calling the model"})
	hlPrint(Event{Kind: "error", Text: "provider down"})
	hlPrint(Event{Kind: "done"})
	if got := ExitCode(); got != 1 {
		t.Fatalf("turn that ended on its error: ExitCode = %d, want 1", got)
	}
}
