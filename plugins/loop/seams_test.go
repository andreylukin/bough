package loop

import (
	"context"
	"strings"
	"testing"

	"github.com/andreylukin/bough/kernel"
)

// recordLLM records the system prompt and every message it was sent,
// and always replies with plain text (ending the turn).
type recordLLM struct {
	system   string
	messages []Message
	reply    string
}

func (l *recordLLM) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	l.system = system
	l.messages = append([]Message(nil), messages...)
	if l.reply == "" {
		return "ok", nil
	}
	return l.reply, nil
}

// stubHooks returns canned results per event and records fired events.
type stubHooks struct {
	results map[string]map[string]any
	fired   []string
}

func (h *stubHooks) Fire(ctx context.Context, event string, payload map[string]any) (map[string]any, error) {
	h.fired = append(h.fired, event)
	return h.results[event], nil
}

type stubSkills struct{ blocks []string }

func (s *stubSkills) Inject(input string) []string { return s.blocks }

// buildRunner mounts the loop plugin against stub services and returns
// the runner directly so tests can call Run synchronously.
func buildRunner(t *testing.T, llm *recordLLM, code Codemode, hooks Hooks, skills Skills) *runner {
	t.Helper()
	kctx := kernel.NewContext()
	kctx.Provide("llm", llm)
	kctx.Provide("codemode", code)
	if hooks != nil {
		kctx.Provide("hooks", hooks)
	}
	if skills != nil {
		kctx.Provide("skills", skills)
	}
	if err := (&plugin{}).Apply(kctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	r, err := kernel.Get[*runner](kctx, "runner")
	if err != nil {
		t.Fatalf("runner: %v", err)
	}
	t.Cleanup(kctx.Unmount)
	return r
}

func collect(kinds *[]string, texts *[]string) func(kind, text string) {
	return func(kind, text string) {
		*kinds = append(*kinds, kind)
		*texts = append(*texts, text)
	}
}

func TestHookRewritesPrompt(t *testing.T) {
	llm := &recordLLM{}
	hooks := &stubHooks{results: map[string]map[string]any{
		"user-prompt-submit": {"input": "rewritten"},
	}}
	r := buildRunner(t, llm, &stubCode{}, hooks, nil)

	var kinds, texts []string
	if err := r.Run(context.Background(), "original", collect(&kinds, &texts)); err != nil {
		t.Fatalf("Run: %v", err)
	}
	if len(llm.messages) != 1 || llm.messages[0].Content != "rewritten" {
		t.Fatalf("llm saw %v, want single message %q", llm.messages, "rewritten")
	}
}

func TestHookBlocksPrompt(t *testing.T) {
	llm := &recordLLM{}
	hooks := &stubHooks{results: map[string]map[string]any{
		"user-prompt-submit": {"block": "nope"},
	}}
	r := buildRunner(t, llm, &stubCode{}, hooks, nil)

	var kinds, texts []string
	if err := r.Run(context.Background(), "do it", collect(&kinds, &texts)); err != nil {
		t.Fatalf("Run: %v", err)
	}
	if llm.messages != nil {
		t.Fatalf("llm was called with %v, want no call", llm.messages)
	}
	if len(kinds) == 0 || kinds[len(kinds)-1] != "error" || texts[len(texts)-1] != "nope" {
		t.Fatalf("events %v %v, want trailing error %q", kinds, texts, "nope")
	}
}

func TestHookDeniesCodeExec(t *testing.T) {
	llm := &recordLLM{reply: "Trying.\n```js\nrm()\n```"}
	code := &stubCode{}
	hooks := &stubHooks{results: map[string]map[string]any{
		"pre-code-exec": {"deny": "too spicy"},
	}}
	r := buildRunner(t, llm, code, hooks, nil)

	var kinds, texts []string
	err := r.Run(context.Background(), "go", collect(&kinds, &texts))
	if err == nil {
		t.Fatal("expected gave-up error (every step denied)")
	}
	if len(code.ran) != 0 {
		t.Fatalf("codemode ran %q, want nothing", code.ran)
	}
	found := false
	for i, k := range kinds {
		if k == "result" && texts[i] == "[hook denied: too spicy]" {
			found = true
		}
	}
	if !found {
		t.Fatalf("no denied result in events %v %v", kinds, texts)
	}
	// history got the denial as the block's result
	got := ""
	for _, m := range r.history {
		if strings.Contains(m.Content, "[hook denied: too spicy]") {
			got = m.Content
		}
	}
	if got == "" {
		t.Fatalf("denial not fed back to model; history %v", r.history)
	}
}

func TestSkillsInjection(t *testing.T) {
	llm := &recordLLM{}
	skills := &stubSkills{blocks: []string{"[skill: wiki]\nuse the wiki cli"}}
	r := buildRunner(t, llm, &stubCode{}, nil, skills)

	var kinds, texts []string
	if err := r.Run(context.Background(), "update the wiki", collect(&kinds, &texts)); err != nil {
		t.Fatalf("Run: %v", err)
	}
	want := "update the wiki\n\n[skill: wiki]\nuse the wiki cli"
	if len(llm.messages) != 1 || llm.messages[0].Content != want {
		t.Fatalf("llm saw %q, want %q", llm.messages, want)
	}
}
