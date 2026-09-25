package ui

import (
	"bytes"
	"slices"
	"testing"
)

// Not parallel: the headless pump state is process-wide.

// Under serve every answer arrives typed, naming its question, so a
// line routes by what it is, not by what happens to be open when it is
// read: a prompt that lands in the gap before serve arms a question
// steers the turn, and an answer to a question that timed out (serve
// wrote it before it read the timeout) is dropped rather than read as a
// steer or handed to the next question, which may be a secret.
// Found by tests/model/mbt/ask_answer_arm_races_test.go.
func TestHeadlessTypedAnswersRouteByAsk(t *testing.T) {
	for _, tc := range []struct {
		name   string
		open   *hlAskState
		line   string
		route  string
		answer []string
		steer  []string
		kept   bool // the open question is still open after
	}{
		{"prompt with a question open", &hlAskState{id: "ask-1"}, "msg-1", "steer", nil, []string{"msg-1"}, true},
		{"its own answer", &hlAskState{id: "ask-1", options: []string{"red", "blue"}}, `{"answer":"2","ask":"ask-1"}`, "answer", []string{"ask-1=blue"}, nil, false},
		{"a late answer with another question open", &hlAskState{id: "ask-2", secret: true}, `{"answer":"ans-A-1","ask":"ask-1"}`, "drop", nil, nil, true},
		{"a late answer with nothing open", nil, `{"answer":"S3CR3T-1","ask":"ask-2"}`, "drop", nil, nil, false},
		{"a secret's own answer", &hlAskState{id: "ask-2", secret: true}, `{"answer":"S3CR3T-1","ask":"ask-2"}`, "answer", []string{"ask-2=S3CR3T-1"}, nil, false},
	} {
		var out, errb bytes.Buffer
		var steered []string
		ans := &recordAnswers{}
		oldOut, oldErr := hlOut, hlErr
		hlOut, hlErr = &out, &errb
		hlMu.Lock()
		oldAsk, oldAnswer, oldSteer, oldTyped := hlAsk, hlAnswer, hlSteer, hlTyped
		hlAsk, hlAnswer, hlTyped = tc.open, ans, true
		hlSteer = func(s string) bool { steered = append(steered, s); return true }
		hlMu.Unlock()

		route := hlLineIn(tc.line)

		hlMu.Lock()
		kept := hlAsk != nil
		hlAsk, hlAnswer, hlSteer, hlTyped = oldAsk, oldAnswer, oldSteer, oldTyped
		hlMu.Unlock()
		hlOut, hlErr = oldOut, oldErr
		if route != tc.route || !slices.Equal(ans.got, tc.answer) || !slices.Equal(steered, tc.steer) || kept != tc.kept {
			t.Errorf("%s: route %q, answers %q, steers %q, still open %v; want %q, %q, %q, %v",
				tc.name, route, ans.got, steered, kept, tc.route, tc.answer, tc.steer, tc.kept)
		}
	}
}
