package loop

import (
	"fmt"
	"strings"
	"testing"
)

// histPairs returns the run's code and result entries, in order.
func histPairs(r *runner) (codes, results []string) {
	for _, e := range r.hist.Entries() {
		s, _ := e.Data["text"].(string)
		switch e.Kind {
		case "code":
			codes = append(codes, s)
		case "result":
			results = append(results, s)
		}
	}
	return codes, results
}

// Every block of a reply runs, in order, each with its own code and
// result entry: blocks routinely depend on each other (write a file,
// then run it), and codemode's single VM runs them one at a time.
func TestMultiBlockRunsAllInOrder(t *testing.T) {
	r, _ := mcphooksinitjsRunner(t, nil,
		"two things:\n```js\nconsole.log('ONE')\n```\nthen:\n```js\nconsole.log('TWO')\n```")
	mcphooksinitjsRun(t, r, "go")

	codes, results := histPairs(r)
	if len(codes) != 2 || len(results) != 2 {
		t.Fatalf("want 2 code and 2 result entries, got %d/%d: %q %q", len(codes), len(results), codes, results)
	}
	if !strings.Contains(codes[0], "ONE") || !strings.Contains(codes[1], "TWO") {
		t.Fatalf("blocks out of order: %q", codes)
	}
	if !strings.Contains(results[0], "ONE") || !strings.Contains(results[1], "TWO") {
		t.Fatalf("results out of order: %q", results)
	}
	for _, res := range results {
		if strings.Contains(res, "dropped") || strings.Contains(res, "not run") {
			t.Fatalf("an under-cap reply was reported as truncated: %q", res)
		}
	}
}

// Past the cap the reply is truncated: maxBlocks blocks run and the
// last result tells the model exactly how many of its blocks ran.
func TestMultiBlockCapped(t *testing.T) {
	var b strings.Builder
	b.WriteString("here we go\n")
	for i := range maxBlocks + 5 {
		fmt.Fprintf(&b, "```js\nconsole.log('STEP%d')\n```\nlooks good.\n", i)
	}
	r, _ := mcphooksinitjsRunner(t, nil, b.String())
	mcphooksinitjsRun(t, r, "go")

	codes, results := histPairs(r)
	if len(codes) != maxBlocks || len(results) != maxBlocks {
		t.Fatalf("ran %d blocks (%d results), want %d", len(codes), len(results), maxBlocks)
	}
	if !strings.Contains(codes[maxBlocks-1], fmt.Sprintf("STEP%d", maxBlocks-1)) {
		t.Fatalf("last block is not the cap-th: %q", codes[maxBlocks-1])
	}
	last := results[maxBlocks-1]
	if !strings.Contains(last, fmt.Sprintf("%d of your %d code blocks ran", maxBlocks, maxBlocks+5)) {
		t.Fatalf("the drop note does not say what was dropped: %q", last)
	}
	// The assistant entry keeps the same truth for the transcript.
	found := false
	for _, e := range r.hist.Entries() {
		if e.Kind != "assistant" {
			continue
		}
		s, _ := e.Data["text"].(string)
		if strings.Contains(s, fmt.Sprintf("%d further code block(s) dropped", 5)) {
			found = true
		}
		if strings.Contains(s, fmt.Sprintf("STEP%d", maxBlocks)) {
			t.Fatalf("a block past the cap survived in the reply: %q", s)
		}
	}
	if !found {
		t.Fatal("no assistant entry carries the drop marker")
	}
}

// A failing block ends the reply: the blocks after it were written
// before its output existed and assume it succeeded, so they do not
// run — and the model is told so rather than left to infer it.
func TestMultiBlockStopsOnFailure(t *testing.T) {
	r, _ := mcphooksinitjsRunner(t, nil,
		"```js\nconsole.log('FIRST')\n```\n```js\nboom()\n```\n```js\nconsole.log('LAST')\n```",
		"the second block failed, so nothing after it ran.")
	mcphooksinitjsRun(t, r, "go")

	codes, results := histPairs(r)
	if len(codes) != 2 || len(results) != 2 {
		t.Fatalf("want 2 code and 2 result entries, got %q %q", codes, results)
	}
	if strings.Contains(strings.Join(codes, "\n"), "LAST") {
		t.Fatalf("a block after the failure ran: %q", codes)
	}
	last := results[1]
	if !strings.Contains(last, "error: ReferenceError") {
		t.Fatalf("the failure is missing from the result: %q", last)
	}
	if !strings.Contains(last, "[the 1 code block(s) after this one in your reply were not run") {
		t.Fatalf("the skipped block was not reported: %q", last)
	}
}
