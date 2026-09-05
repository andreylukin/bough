package loop

// The end-of-turn rule, checked against the replies that actually
// provoked it. In a week of real sessions 22 replies were refused for
// "ran nothing and did not stop": 7 had announced a step they never
// ran, and 15 were finished answers the user then read twice, once as
// the rejected draft and once reworded inside a fence.

import (
	"context"
	"errors"
	"slices"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/schema"
	"github.com/andreylukin/bough/plugins/llm"
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

// A tool call the loop cannot run must not end the turn. Under the old
// contract this was covered by accident; the real reply below ended a
// turn with markup as its answer once the contract was inverted.
func TestMisfencedToolCallIsRefused(t *testing.T) {
	real := "\n<html>\n<body>\n<div class=\"container\">\n<script>\nconsole.log(tools.write(\"/tmp/hunt/pkg/stack_test.go\", `package pkg`))\n</script>\n</div>\n</body>\n</html>\n"
	for name, reply := range map[string]string{
		"html script wrapper": real,
		"wrong fence tag":     "Here it is:\n```javascript\nconsole.log(tools.bash(\"ls\"))\n```",
		"bare tool call":      "I'll list the files.\nconsole.log(tools.bash(\"ls\"))",
		"unfenced tools line": "tools.view(\"a.go\")",
	} {
		if !meantToRunCode(reply) {
			t.Errorf("%s: should be refused, not taken as the answer:\n%s", name, reply)
		}
	}
}

// Prose that merely mentions a tool is an answer, not an attempt.
func TestProseAboutToolsStillAnswers(t *testing.T) {
	for _, reply := range []string{
		"Use tools.bash(cmd) to run a command; it returns the combined output.",
		"The failure came from tools.view(path) being called on a directory.",
		"`tools.spawn` runs a child agent. It is bounded to depth 1.",
	} {
		if meantToRunCode(reply) {
			t.Errorf("discussion of a tool is an answer, not a misfenced call: %q", reply)
		}
	}
}

// A properly fenced block never reaches the check.
func TestFencedBlockIsNotMisfenced(t *testing.T) {
	if meantToRunCode("```js\nconsole.log(tools.bash(\"ls\"))\n```") {
		t.Error("a real js block is not a misfenced call")
	}
}

// A provider that cut the reply off at its output limit has not
// produced an answer, however complete the prose looks. Before this,
// OpenRouter's finish_reason "length" was allowed through silently and
// a reply that stopped mid-sentence was handed over as final.
func TestTruncatedReplyDoesNotEndTheTurn(t *testing.T) {
	partial := "## What decides a turn is over\n\nThe regex that finds a "
	llmStub := &seqLLM{replies: []string{
		llm.MarkTruncated(partial),
		"The turn ends when the reply runs nothing.",
	}}
	r := &runner{llm: llmStub, code: &stubCode{}, hist: &memHistory{}, secs: &Sections{}, stopRetries: 2}
	var kinds, texts []string
	if err := r.Run(context.Background(), "what ends a turn?", collect(&kinds, &texts)); err != nil {
		t.Fatal(err)
	}
	if llmStub.calls != 2 {
		t.Fatalf("a truncated reply should be asked again, %d calls", llmStub.calls)
	}
	if i := slices.Index(texts, "The turn ends when the reply runs nothing."); i < 0 {
		t.Fatalf("the complete answer never landed: %v", texts)
	}
	// The marker is machinery: it belongs in the record, never as the
	// last thing the user reads.
	last := texts[len(texts)-1]
	if strings.Contains(last, "truncated at the output limit") {
		t.Errorf("the final answer should not carry the marker: %q", last)
	}
}

// The marker is only added once, however many times a reply passes
// through it.
func TestMarkTruncatedIsIdempotent(t *testing.T) {
	once := llm.MarkTruncated("half a thought")
	if llm.MarkTruncated(once) != once {
		t.Error("marking twice should not double the marker")
	}
}

// Models fall back to the tool-calling convention they were trained on.
// A real reply from claude-sonnet-5 through OpenRouter was exactly
// this, twice in a row, with no prose and no tools.* call — so the
// other shapes missed it and the turn nearly ended on it.
func TestJSONToolCallIsRefused(t *testing.T) {
	for _, reply := range []string{
		"{\n\"cmd\": \"cd /repo && find . -name '*.go'\"\n}",
		"{\"name\": \"bash\", \"arguments\": {\"command\": \"ls\"}}",
		"  {\n  \"tool\": \"view\", \"path\": \"a.go\"\n}  ",
	} {
		if !meantToRunCode(reply) {
			t.Errorf("a bare JSON object is a misfired tool call, not an answer: %q", reply)
		}
	}
}

// An answer that happens to contain JSON is still an answer.
func TestJSONInsideAnAnswerIsFine(t *testing.T) {
	for _, reply := range []string{
		"The config is:\n\n```json\n{\"model\": \"claude-sonnet-5\"}\n```\n\nThat is all it needs.",
		"It returns {\"ok\": true} on success.",
		"{\"ok\": true} is what it returns, and nothing else changed.",
	} {
		if meantToRunCode(reply) {
			t.Errorf("prose carrying JSON is an answer: %q", reply)
		}
	}
}

// A schema'd turn's answer IS a bare JSON object, so the JSON veto
// must not fire on it — the schema check is what judges those.
func TestSchemaTurnAcceptsBareJSON(t *testing.T) {
	sch := schema.Schema{"type": "object", "properties": map[string]any{
		"files": map[string]any{"type": "array", "items": map[string]any{"type": "string"}},
	}, "required": []any{"files"}}
	llmStub := &seqLLM{replies: []string{"{\"files\": [\"a.go\"]}"}}
	r := &runner{llm: llmStub, code: &stubCode{}, hist: &memHistory{}, secs: &Sections{},
		stopRetries: 2, schema: sch}
	var kinds, texts []string
	if err := r.Run(context.Background(), "list files", collect(&kinds, &texts)); err != nil {
		t.Fatal(err)
	}
	if llmStub.calls != 1 {
		t.Fatalf("a valid structured answer should not be refused, %d calls", llmStub.calls)
	}
}

// The shapes the specific vetoes kept missing one at a time. A turn
// ended on "<br>" and an earlier one on "}" — what a model emits when
// it has lost the thread, and never an answer.
func TestRepliesWithNoContentAreRefused(t *testing.T) {
	for _, reply := range []string{"<br>", "}", "", "   \n\n  ", "```", "---", "<div></div>", "{}", "<html><body></body></html>"} {
		if !saysNothing(reply) {
			t.Errorf("%q carries no content and must not end a turn", reply)
		}
	}
}

// Short is not the same as empty.
func TestShortAnswersAreStillAnswers(t *testing.T) {
	for _, reply := range []string{"Done.", "3 lines.", "Yes — the test passes now.", "42", "ok"} {
		if saysNothing(reply) {
			t.Errorf("%q says something and is a valid answer", reply)
		}
	}
}

// A conversation that outgrew the window ends the turn with a way out,
// not with the provider's token arithmetic alone.
func TestOverflowErrorCarriesTheWayOut(t *testing.T) {
	llmStub := &errLLM{err: errors.New(
		"llm-openrouter: HTTP 400: This endpoint's maximum context length is 200000 tokens. However, you requested 210000 tokens.")}
	r := &runner{llm: llmStub, code: &stubCode{}, hist: &memHistory{}, secs: &Sections{}, stopRetries: 2}
	var kinds, texts []string
	_ = r.Run(context.Background(), "carry on", collect(&kinds, &texts))

	var errText string
	for i, k := range kinds {
		if k == "error" {
			errText = texts[i]
		}
	}
	if errText == "" {
		t.Fatalf("no error reported: %v", kinds)
	}
	for _, want := range []string{"maximum context length", "/model", "/new"} {
		if !strings.Contains(errText, want) {
			t.Errorf("the error should keep the cause and add the way out (%q):\n%s", want, errText)
		}
	}
	// The turn still ends properly rather than hanging.
	if kinds[len(kinds)-1] != "done" {
		t.Errorf("a failed turn still ends with done: %v", kinds)
	}
}

// An ordinary failure is reported as it is.
func TestOtherErrorsAreNotDecorated(t *testing.T) {
	llmStub := &errLLM{err: errors.New("llm-openrouter: HTTP 401: Missing Authentication header")}
	r := &runner{llm: llmStub, code: &stubCode{}, hist: &memHistory{}, secs: &Sections{}, stopRetries: 2}
	var kinds, texts []string
	_ = r.Run(context.Background(), "hi", collect(&kinds, &texts))
	for i, k := range kinds {
		if k == "error" && strings.Contains(texts[i], "/new") {
			t.Errorf("a 401 is not an overflow: %s", texts[i])
		}
	}
}

// errLLM fails every call with the same error.
type errLLM struct{ err error }

func (e errLLM) Complete(context.Context, string, []llm.Message) (string, error) {
	return "", e.err
}
