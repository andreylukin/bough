package ui

import (
	"testing"
	"time"
)

// Not parallel: the headless pump state is process-wide.

// A line that arrives for a pending ask while the ui row is between
// dispose and remount (a config reload) waits for the remount and
// answers the ask through the new mount's service. It used to drop the
// ask and fall through to hlSubmit, so the answer became a prompt, and
// a secret became an input entry in history.
// Found by tests/model/mbt/ask_across_reload_respawn_test.go (ReloadUi,
// Answer, RemountUi).
func TestHeadlessAnswerInReloadGapWaitsForRemount(t *testing.T) {
	for _, secret := range []bool{false, true} {
		ans := &recordAnswers{}
		inputs := make(chan string, 1)
		hlMu.Lock()
		oldAsk, oldAnswer, oldInputs, oldSteer := hlAsk, hlAnswer, hlInputs, hlSteer
		hlAsk = &hlAskState{id: "ask-1", secret: secret}
		hlAnswer, hlInputs, hlSteer = nil, nil, nil
		hlMu.Unlock()

		done := make(chan struct{})
		go func() { hlLineIn("red"); close(done) }()
		time.Sleep(100 * time.Millisecond)
		hlMu.Lock()
		hlAnswer, hlInputs = ans, inputs
		hlMu.Unlock()
		select {
		case <-done:
		case <-time.After(5 * time.Second):
			t.Fatalf("secret=%v: the line never left the gap", secret)
		}
		hlMu.Lock()
		hlAsk, hlAnswer, hlInputs, hlSteer = oldAsk, oldAnswer, oldInputs, oldSteer
		hlMu.Unlock()
		select {
		case in := <-inputs:
			t.Fatalf("secret=%v: the answer was sent as input %q", secret, in)
		default:
		}
		if len(ans.got) != 1 || ans.got[0] != "ask-1=red" {
			t.Fatalf("secret=%v: answers %q, want [ask-1=red]", secret, ans.got)
		}
	}
}
