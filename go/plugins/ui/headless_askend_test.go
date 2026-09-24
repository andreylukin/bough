package ui

import (
	"bytes"
	"testing"
)

// Not parallel: the headless pump state is process-wide.

// An ask whose call ends with no answer (a timeout) stops taking stdin:
// the next line steers the turn instead of being fed to an ask that is
// gone, where Asker.Answer failed and the line was lost. A call's live
// start and another tool's end leave the ask armed.
func TestHeadlessAskCallEndDisarms(t *testing.T) {
	for _, end := range []Event{
		{Kind: "call", Text: "Which?", Data: map[string]any{"tool": "ask", "id": "c1", "error": "ask: no answer after 10m0s"}},
		{Kind: "call", Text: "TOKEN", Data: map[string]any{"tool": "secret", "id": "c1", "error": "secret: ask: no answer after 10m0s"}},
		{Kind: "result", Text: "Error: ask: no answer after 10m0s"},
	} {
		var out, errb bytes.Buffer
		var steered []string
		ans := &recordAnswers{}
		oldOut, oldErr := hlOut, hlErr
		hlOut, hlErr = &out, &errb
		hlMu.Lock()
		oldAsk, oldAnswer, oldSteer := hlAsk, hlAnswer, hlSteer
		hlAnswer = ans
		hlSteer = func(s string) bool { steered = append(steered, s); return true }
		hlMu.Unlock()
		restore := func() {
			hlOut, hlErr = oldOut, oldErr
			hlMu.Lock()
			hlAsk, hlAnswer, hlSteer = oldAsk, oldAnswer, oldSteer
			hlMu.Unlock()
		}

		hlPrint(Event{Kind: "ask", ID: "ask-1", Text: "Which?"})
		hlPrint(Event{Kind: "call", Text: "Which?", Data: map[string]any{"tool": "ask", "id": "c1", "phase": "start"}})
		hlPrint(Event{Kind: "call", Text: "ls", Data: map[string]any{"tool": "bash", "id": "c0"}})
		hlMu.Lock()
		armed := hlAsk != nil
		hlMu.Unlock()
		if !armed {
			restore()
			t.Fatalf("%s: a live start or another tool's end disarmed the ask", end.Kind)
		}
		hlPrint(end)
		hlLineIn("carry on")
		restore()
		if len(ans.got) != 0 || len(steered) != 1 || steered[0] != "carry on" {
			t.Fatalf("after %s %v: answers %q, steers %q; want the line steered", end.Kind, end.Data["tool"], ans.got, steered)
		}
	}
}
