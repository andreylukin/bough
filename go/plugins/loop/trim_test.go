package loop

// Trimming stale tool output. The properties that matter: a session
// under budget is untouched, the thread is never trimmed, the most
// recent outputs stay whole however far over budget the turn is, and
// the history log is unchanged.

import (
	"strings"
	"testing"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

// bigOutput is one tool result, ~2.8KB, with a head and a tail marker
// so a test can tell a whole one from a trimmed one.
func bigOutput(marker string) string {
	return marker + "_HEAD\n" +
		strings.Repeat("a line of output that nobody will read again\n", 60) +
		marker + "_TAIL\n"
}

func markerFor(i int) string { return "MARKER_" + string(rune('A'+i)) }

// results builds n assistant/tool-output pairs.
func results(n int) []llm.Message {
	var msgs []llm.Message
	for i := range n {
		msgs = append(msgs,
			llm.Message{Role: "assistant", Content: "step"},
			llm.Message{Role: "user", Content: toolOutputPrefix + bigOutput(markerFor(i))})
	}
	return msgs
}

func sizeOf(ms []llm.Message) int {
	n := 0
	for _, m := range ms {
		n += len(m.Content)
	}
	return n
}

// Most sessions never reach the budget and must come out byte-identical.
func TestTrimLeavesASmallSessionAlone(t *testing.T) {
	msgs := results(10) // ~28KB, well under budget
	if sizeOf(msgs) >= projectionBudget {
		t.Fatalf("precondition: %d should be under the budget", sizeOf(msgs))
	}
	got := trimProjection(msgs, keepWholeResults)
	for i := range msgs {
		if got[i].Content != msgs[i].Content {
			t.Fatalf("message %d changed in a session under budget", i)
		}
	}
}

// Over budget: the oldest are cut, the newest keepWholeResults are not,
// and the result lands under the budget.
func TestTrimCutsOldestUntilUnderBudget(t *testing.T) {
	const n = 40 // ~113KB, well over
	msgs := results(n)
	got := trimProjection(msgs, keepWholeResults)

	// Under budget here because these outputs are small enough that the
	// floor fits inside it. That is not a universal invariant: when the
	// most recent keepWholeResults outputs are themselves larger than
	// the budget, the floor wins and the projection stays above it —
	// deliberately, since those are what the model is working from.
	if after := sizeOf(got); after > projectionBudget {
		t.Errorf("with outputs this size the floor fits, so trimming should reach %d, got %d", projectionBudget, after)
	}
	// The newest keepWholeResults outputs are whole, tail and all.
	for i := n - keepWholeResults; i < n; i++ {
		if !strings.Contains(got[i*2+1].Content, markerFor(i)+"_TAIL") {
			t.Errorf("output %d is within the floor and must be whole", i)
		}
	}
	// The oldest were cut, and say so, and keep their head.
	c := got[1].Content
	if strings.Contains(c, markerFor(0)+"_TAIL") {
		t.Errorf("the oldest output should have lost its tail: %q", c)
	}
	for _, want := range []string{toolOutputPrefix, markerFor(0) + "_HEAD", "were trimmed"} {
		if !strings.Contains(c, want) {
			t.Errorf("a trimmed output should keep %q: %q", want, c)
		}
	}
}

// The floor wins over the budget: the most recent outputs are what the
// model is working from, and taking them is what made a real run read
// the same file nineteen times.
func TestTrimNeverCutsTheMostRecent(t *testing.T) {
	msgs := results(keepWholeResults) // every one is within the floor
	got := trimProjection(msgs, keepWholeResults)
	for i := range msgs {
		if got[i].Content != msgs[i].Content {
			t.Fatalf("message %d is within the floor and must not be trimmed", i)
		}
	}
}

// Prompts and replies are the thread: never trimmed, never reordered.
func TestTrimNeverTouchesTheConversation(t *testing.T) {
	msgs := append([]llm.Message{{Role: "user", Content: "FIRST_PROMPT"}}, results(40)...)
	msgs = append(msgs, llm.Message{Role: "user", Content: "LAST_PROMPT " + strings.Repeat("y", 4000)})
	got := trimProjection(msgs, keepWholeResults)

	if len(got) != len(msgs) {
		t.Fatalf("trimming must not add or drop messages: %d vs %d", len(got), len(msgs))
	}
	if got[0].Content != msgs[0].Content {
		t.Error("the opening prompt was trimmed")
	}
	if got[len(got)-1].Content != msgs[len(msgs)-1].Content {
		t.Error("the closing prompt was trimmed")
	}
	for i, m := range got {
		if m.Role != msgs[i].Role {
			t.Fatalf("roles must not change at %d", i)
		}
	}
}

func TestTrimDisabled(t *testing.T) {
	msgs := results(40)
	for _, keep := range []int{0, -1} {
		got := trimProjection(msgs, keep)
		for i := range msgs {
			if got[i].Content != msgs[i].Content {
				t.Fatalf("keep=%d must trim nothing", keep)
			}
		}
	}
}

// End to end through the real projection: a long turn stops growing
// without bound, and the log still holds every byte.
func TestProjectionShrinksWithoutTouchingHistory(t *testing.T) {
	entries := []history.Entry{{Kind: "input", Data: map[string]any{"text": "do the thing"}}}
	for i := range 40 {
		entries = append(entries,
			history.Entry{Kind: "assistant", Data: map[string]any{"text": "```js\nx()\n```"}},
			history.Entry{Kind: "result", Data: map[string]any{"text": bigOutput(markerFor(i % 20))}})
	}
	full := DefaultProject(entries)
	trimmed := trimProjection(full, keepWholeResults)

	if sizeOf(trimmed) > projectionBudget {
		t.Errorf("a 40-step turn should end up under budget, got %d", sizeOf(trimmed))
	}
	if len(trimmed) != len(full) {
		t.Error("the message count must not change")
	}
	for _, e := range entries {
		if e.Kind != "result" {
			continue
		}
		if txt, _ := e.Data["text"].(string); !strings.Contains(txt, "_TAIL") {
			t.Fatal("trimming must not mutate the history entries")
		}
	}
}
