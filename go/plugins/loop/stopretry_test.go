package loop

import (
	"strings"
	"testing"
)

// The re-ask rules, against what a month of real sessions showed: the
// model re-ran the block that had just failed, verbatim, 49 times, and
// 21 push-backs for "announced work it did not do" recovered nothing.

// A block identical to the one that just failed is not run again: the
// model is told why, and the third attempt is told to report instead.
func TestIdenticalFailedBlockIsRefused(t *testing.T) {
	bad := "```js\nthrow new Error('boom')\n```"
	r, llm := mcphooksinitjsRunner(t, nil, bad, bad, bad, "```stop\nit failed\n```")
	mcphooksinitjsRun(t, r, "go")
	_, results := histPairs(r)
	if len(results) != 3 {
		t.Fatalf("want 3 results (one run, two refusals), got %d: %q", len(results), results)
	}
	if !strings.Contains(results[0], "boom") {
		t.Fatalf("first attempt did not run: %q", results[0])
	}
	if !strings.HasPrefix(results[1], "[not run]") || !strings.Contains(results[1], "boom") {
		t.Fatalf("second attempt was not refused with the reason: %q", results[1])
	}
	if !strings.Contains(results[2], "third attempt") {
		t.Fatalf("third attempt was not told to report: %q", results[2])
	}
	// The model's fourth call was fed the refusals, not a fourth run.
	if len(llm.calls) != 4 {
		t.Fatalf("model calls = %d, want 4", len(llm.calls))
	}
}

// A changed block after a failure runs: the breaker is for repeats, not retries.
func TestChangedBlockAfterFailureRuns(t *testing.T) {
	r, _ := mcphooksinitjsRunner(t, nil, "```js\nthrow new Error('boom')\n```", "```js\nconsole.log('fixed')\n```")
	mcphooksinitjsRun(t, r, "go")
	_, results := histPairs(r)
	if len(results) != 2 || !strings.Contains(results[1], "fixed") {
		t.Fatalf("changed block did not run: %q", results)
	}
}

// A push-back after a failed block carries the turn's receipt: what
// passed (leave it) and what failed (the one thing to do).
func TestFailedStopNudgeCarriesReceipt(t *testing.T) {
	r, llm := mcphooksinitjsRunner(t, nil,
		"```js\nconsole.log(tools.bash ? 'x' : 'ok one')\n```",
		"```js\nthrow new Error('boom')\n```",
		"done, all good.", // stops on the failure: pushed back once with the receipt
		"```stop\nit failed\n```")
	mcphooksinitjsRun(t, r, "go")
	var nudge string
	for _, e := range r.hist.Entries() {
		if e.Kind == "nudge" {
			nudge, _ = e.Data["text"].(string)
		}
	}
	if !strings.HasPrefix(nudge, stoppedOnErrorNote) {
		t.Fatalf("no failed-stop nudge: %q", nudge)
	}
	for _, want := range []string{"do not re-run what passed", "- passed: console.log(tools.bash", "- FAILED: throw new Error('boom')", "boom"} {
		if !strings.Contains(nudge, want) {
			t.Errorf("receipt lacks %q:\n%s", want, nudge)
		}
	}
	if strings.Contains(stoppedOnErrorNote, "run the check again") {
		t.Error("the note still invites a re-run")
	}
	if len(llm.calls) != 4 {
		t.Fatalf("model calls = %d, want 4", len(llm.calls))
	}
}

// Announced-but-not-done is read off the prose: one push-back, then the
// reply stands, however many retries a failed block would get.
func TestAnnouncedWorkGetsOneRetry(t *testing.T) {
	r, llm := mcphooksinitjsRunner(t, nil, "Let me check the tests.", "Now I'll run them.", "Next, let me verify.")
	kinds, texts := mcphooksinitjsRun(t, r, "go")
	if len(llm.calls) != 2 {
		t.Fatalf("model calls = %d, want 2 (one push-back)", len(llm.calls))
	}
	var final bool
	for i, k := range kinds {
		if k == "system" && strings.Contains(texts[i], "taking the reply as final") {
			final = true
		}
	}
	if !final {
		t.Fatalf("the second announcement was not taken as final: %v %v", kinds, texts)
	}
}
