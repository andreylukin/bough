package loop

// The end-of-turn rule, checked against the replies that actually
// provoked it. In a week of real sessions 22 replies were refused for
// "ran nothing and did not stop": 7 had announced a step they never
// ran, and 15 were finished answers the user then read twice, once as
// the rejected draft and once reworded inside a fence.

import (
	"context"
	"slices"
	"strings"
	"testing"
)

// restated are real final replies that the old rule refused. Each is a
// complete answer: it must end the turn as written.
var restated = []string{
	"Job 1 finished after 4m0s, exit code 0, output: `DONE`.",
	"Job 2 (`sleep 120; echo X`) is running with a 5m0s limit. I'll be notified when it finishes.",
	"Pushed to `main` — `ab96727c llm: prompt-cache counters for openai and openrouter`.",
	"Started.",
	"Test passed: `go test ./plugins/tools/ -count=1` → `ok github.com/andreylukin/bough/plugins/tools 2.316s`.",
	"Job 1 finished after 2m and printed `A`. Job 2 (`sleep 200; echo B`) is still running, ~80 s left.",
}

// announced are replies that promise the model's own next action. The
// step never ran, so the turn must not end on them.
var announced = []string{
	"The probe didn't reach its dump. Let me make the probe unconditional.",
	"The write from before didn't run (turn was interrupted). Running it now.",
	"I have the full picture now. Before writing: check remaining test file content and `addUsage` call sites:",
	"Now checking the exact wire formats before writing code.",
	"I'll verify the gate is green.",
	"Next, let me read the fixtures.",
}

func TestFinishedAnswersEndTheTurn(t *testing.T) {
	for _, reply := range restated {
		text, stopped, _ := Finish(reply)
		if !stopped {
			t.Errorf("a reply that runs nothing is the answer, refused: %q", reply)
		}
		if text != reply {
			t.Errorf("answer changed:\n got %q\nwant %q", text, reply)
		}
		if announcesWork(reply) {
			t.Errorf("finished answer misread as an announcement: %q", reply)
		}
	}
}

func TestAnnouncedWorkIsRefused(t *testing.T) {
	for _, reply := range announced {
		if !announcesWork(reply) {
			t.Errorf("announcement not caught: %q", reply)
		}
	}
}

// The whole turn, end to end: a plain-prose answer ends it in one call,
// with no "asking again" note under it.
func TestProseAnswerCostsOneCall(t *testing.T) {
	llm := &seqLLM{replies: []string{"Job 1 finished after 4m0s, exit code 0, output: `DONE`."}}
	r := &runner{llm: llm, code: &stubCode{}, hist: &memHistory{}, secs: &Sections{}, stopRetries: 2}
	var kinds, texts []string
	if err := r.Run(context.Background(), "what happened?", collect(&kinds, &texts)); err != nil {
		t.Fatal(err)
	}
	if llm.calls != 1 {
		t.Fatalf("%d calls, want 1: a finished answer is not asked for twice", llm.calls)
	}
	for _, s := range texts {
		if strings.Contains(s, "asking again") {
			t.Fatalf("the user should see no push-back on a finished answer: %q", s)
		}
	}
	if i := slices.Index(texts, "Job 1 finished after 4m0s, exit code 0, output: `DONE`."); i < 0 {
		t.Fatalf("the answer never reached the user: %v", texts)
	}
}

// The rescue the old rule was really earning: an announcement is asked
// again, and the step that follows runs.
func TestAnnouncementIsAskedAgainAndThenRuns(t *testing.T) {
	llm := &seqLLM{replies: []string{
		"The probe didn't reach its dump. Let me make the probe unconditional.",
		"```js\nrun()\n```",
		"Fixed: the probe now dumps unconditionally.",
	}}
	code := &stubCode{}
	r := &runner{llm: llm, code: code, hist: &memHistory{}, secs: &Sections{}, stopRetries: 2}
	var kinds, texts []string
	if err := r.Run(context.Background(), "fix it", collect(&kinds, &texts)); err != nil {
		t.Fatal(err)
	}
	if !slices.Contains(kinds, "code") {
		t.Fatalf("the announced step never ran: %v", kinds)
	}
	if i := slices.Index(texts, "Fixed: the probe now dumps unconditionally."); i < 0 {
		t.Fatalf("the real answer never landed: %v", texts)
	}
}

// The stop fence is optional now, but it still does its job: an
// introduction above it is kept (the model likes to lead into its
// answer) and anything after it is dropped.
func TestStopFenceStillBoundsTheAnswer(t *testing.T) {
	text, stopped, _ := Finish("Here is what I found.\n```stop\nThe answer.\n```\nignore this trailing chatter")
	if !stopped {
		t.Fatal("a stop fence still ends the turn")
	}
	if !strings.Contains(text, "Here is what I found.") || !strings.Contains(text, "The answer.") {
		t.Fatalf("the introduction and the answer both belong, got %q", text)
	}
	if strings.Contains(text, "trailing chatter") {
		t.Fatalf("text after the fence should be dropped, got %q", text)
	}
}
