package loop

import (
	"context"
	"strings"
	"testing"

	"github.com/andreylukin/bough/kernel"
)

// streamLLM streams "one two three" in three fragments.
type streamLLM struct{ complete bool }

func (s *streamLLM) Complete(context.Context, string, []Message) (string, error) {
	s.complete = true
	return "one two three", nil
}

func (s *streamLLM) Stream(_ context.Context, _ string, _ []Message, onDelta func(string)) (string, error) {
	for _, d := range []string{"one ", "two ", "three"} {
		onDelta(d)
	}
	return "one two three", nil
}

// A streaming provider's fragments reach the ui as assistant-delta
// events, in order, before the single assistant event; history records
// only the finished reply, so the model never sees a fragment.
func TestStreamingEmitsDeltasRecordsWhole(t *testing.T) {
	llm := &streamLLM{}
	mem := &memHistory{}
	kctx := kernel.NewContext()
	kctx.Provide("llm", llm)
	kctx.Provide("codemode", &stubCode{})
	kctx.Provide("history", mem)
	if err := (&plugin{}).Apply(kctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	r, err := kernel.Get[*runner](kctx, "runner")
	if err != nil {
		t.Fatal(err)
	}
	var kinds, texts []string
	if err := r.Run(context.Background(), "hi", collect(&kinds, &texts)); err != nil {
		t.Fatal(err)
	}
	if llm.complete {
		t.Fatal("Complete called although the provider streams")
	}
	var deltas []string
	for i, k := range kinds {
		if k == "assistant-delta" {
			deltas = append(deltas, texts[i])
		}
	}
	if got := strings.Join(deltas, "|"); got != "one |two |three" {
		t.Fatalf("deltas = %q", got)
	}
	last := -1
	for i, k := range kinds {
		if k == "assistant-delta" {
			last = i
		}
	}
	if ai := indexOf(kinds, "assistant"); ai < 0 || ai < last || texts[ai] != "one two three" {
		t.Fatalf("assistant event must follow the deltas with the whole reply: kinds=%v texts=%v", kinds, texts)
	}
	for _, e := range mem.Entries() {
		if e.Kind == "assistant-delta" {
			t.Fatal("a delta was recorded to history")
		}
	}
}

func indexOf(xs []string, want string) int {
	for i, x := range xs {
		if x == want {
			return i
		}
	}
	return -1
}
