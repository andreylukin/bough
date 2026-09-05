package loop

// Trimming stale tool output. The properties that matter: the thread is
// never touched, recent output is whole, old output says what was cut,
// and the history log is unchanged.

import (
	"strings"
	"testing"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

// bigOutput is a tool result long enough to be worth trimming.
func bigOutput(marker string) string {
	return marker + "_HEAD\n" + strings.Repeat("a line of output that nobody will read again\n", 60) + marker + "_TAIL\n"
}

func TestTrimLeavesRecentOutputWhole(t *testing.T) {
	var msgs []llm.Message
	for i := range 12 {
		msgs = append(msgs,
			llm.Message{Role: "assistant", Content: "step"},
			llm.Message{Role: "user", Content: toolOutputPrefix + bigOutput(markerFor(i))})
	}
	got := trimProjection(msgs, 8)

	// The last 8 outputs are whole, tail and all.
	for i := 4; i < 12; i++ {
		if !strings.Contains(got[i*2+1].Content, markerFor(i)+"_TAIL") {
			t.Errorf("output %d is recent and should be whole", i)
		}
	}
	// The first 4 are trimmed but still identifiable.
	for i := range 4 {
		c := got[i*2+1].Content
		if !strings.Contains(c, markerFor(i)) {
			t.Errorf("output %d lost its head, so the model cannot tell what it was: %q", i, c)
		}
		if !strings.Contains(c, "were trimmed") {
			t.Errorf("output %d was cut without saying so: %q", i, c)
		}
		if !strings.HasPrefix(c, toolOutputPrefix) {
			t.Errorf("output %d lost its [tool output] prefix", i)
		}
		if len(c) >= len(msgs[i*2+1].Content) {
			t.Errorf("output %d was not actually shortened", i)
		}
	}
}

func markerFor(i int) string { return "MARKER_" + string(rune('A'+i)) }

// Prompts and replies are the thread: never trimmed, never reordered.
func TestTrimNeverTouchesTheConversation(t *testing.T) {
	msgs := []llm.Message{
		{Role: "user", Content: "FIRST_PROMPT"},
		{Role: "assistant", Content: "long assistant reply " + strings.Repeat("x", 4000)},
		{Role: "user", Content: toolOutputPrefix + bigOutput("OLD")},
		{Role: "user", Content: toolOutputPrefix + bigOutput("NEW")},
		{Role: "user", Content: "SECOND_PROMPT " + strings.Repeat("y", 4000)},
	}
	got := trimProjection(msgs, 1)
	if len(got) != len(msgs) {
		t.Fatalf("trimming must not add or drop messages: %d vs %d", len(got), len(msgs))
	}
	if got[0].Content != msgs[0].Content || got[1].Content != msgs[1].Content || got[4].Content != msgs[4].Content {
		t.Error("a prompt or a reply was trimmed; only tool output may be")
	}
	if !strings.Contains(got[3].Content, "NEW_TAIL") {
		t.Error("the most recent output should be whole")
	}
	if strings.Contains(got[2].Content, "OLD_TAIL") {
		t.Error("the older output should have lost its tail")
	}
	if !strings.Contains(got[2].Content, "OLD_HEAD") {
		t.Error("the older output should keep its head")
	}
}

// Small outputs are left alone: the marker would cost as much as the text.
func TestTrimSkipsShortOutput(t *testing.T) {
	short := toolOutputPrefix + "ok\n"
	msgs := []llm.Message{
		{Role: "user", Content: short},
		{Role: "user", Content: toolOutputPrefix + bigOutput("NEW")},
	}
	if got := trimProjection(msgs, 1); got[0].Content != short {
		t.Errorf("a short output should be left exactly as it was: %q", got[0].Content)
	}
}

func TestTrimDisabled(t *testing.T) {
	msgs := []llm.Message{
		{Role: "user", Content: toolOutputPrefix + bigOutput("A")},
		{Role: "user", Content: toolOutputPrefix + bigOutput("B")},
	}
	for _, keep := range []int{0, -1} {
		got := trimProjection(msgs, keep)
		for i := range msgs {
			if got[i].Content != msgs[i].Content {
				t.Errorf("keep=%d must trim nothing", keep)
			}
		}
	}
}

// The point of the whole thing, end to end through the real projection:
// a long turn's context stops growing without bound, and the log still
// holds every byte.
func TestProjectionShrinksWithoutTouchingHistory(t *testing.T) {
	var entries []history.Entry
	entries = append(entries, history.Entry{Kind: "input", Data: map[string]any{"text": "do the thing"}})
	for i := range 30 {
		entries = append(entries,
			history.Entry{Kind: "assistant", Data: map[string]any{"text": "```js\nx()\n```"}},
			history.Entry{Kind: "result", Data: map[string]any{"text": bigOutput(markerFor(i % 20))}})
	}
	full := DefaultProject(entries)
	trimmed := trimProjection(full, keepWholeResults)

	sizeOf := func(ms []llm.Message) int {
		n := 0
		for _, m := range ms {
			n += len(m.Content)
		}
		return n
	}
	before, after := sizeOf(full), sizeOf(trimmed)
	if after >= before/2 {
		t.Errorf("a 30-step turn should shrink a lot: %d -> %d", before, after)
	}
	if len(trimmed) != len(full) {
		t.Error("the message count must not change")
	}
	// History itself is untouched: /export, resume and `bough log` still
	// show everything.
	for _, e := range entries {
		if e.Kind == "result" {
			if txt, _ := e.Data["text"].(string); !strings.Contains(txt, "_TAIL") {
				t.Fatal("trimming must not mutate the history entries")
			}
		}
	}
}
