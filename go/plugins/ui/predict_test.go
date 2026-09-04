package ui

import (
	"context"
	"strings"
	"testing"

	"github.com/andreylukin/bough/plugins/llm"
)

// A small model repeats the prompt, quotes itself, wanders onto a
// second line, or answers instead of continuing. Only a usable
// continuation survives.
func TestCleanSuggestion(t *testing.T) {
	cases := []struct{ name, draft, reply, want string }{
		{"plain", "fix the flaky", "golden test in plugins/ui", " golden test in plugins/ui"},
		{"repeats the draft", "fix the flaky", "fix the flaky golden test", " golden test"},
		{"quoted", "fix the flaky", `"golden test"`, " golden test"},
		{"second line dropped", "fix the flaky", "golden test\nthen push it", " golden test"},
		{"punctuation joins tight", "fix the flaky test", ", then push", ", then push"},
		{"draft already spaced", "fix the flaky ", "golden test", "golden test"},
		{"nothing to add", "fix the flaky", "", ""},
		{"too long", "fix", strings.Repeat("word ", 40), ""},
		// A small model refuses in words rather than staying quiet;
		// shown as a completion, "No output." reads as text to accept.
		{"refusal: none", "fix the flaky", "NONE", ""},
		{"refusal: no output", "fix the flaky", "No output.", ""},
		{"refusal: nothing", "fix the flaky", "nothing", ""},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := cleanSuggestion(c.draft, c.reply); got != c.want {
				t.Fatalf("cleanSuggestion(%q, %q) = %q, want %q", c.draft, c.reply, got, c.want)
			}
		})
	}
}

// Guessing is for a half-written instruction, not for a command, a
// shell line, a finished sentence, or two characters.
func TestPredictable(t *testing.T) {
	yes := []string{"fix the flaky", "why is the gate red", "add a test for the loop"}
	no := []string{"", "fix", "/model list", "!ls -la", "done.", "what now?", "line one\n"}
	for _, d := range yes {
		if !predictable(d) {
			t.Fatalf("%q should be predictable", d)
		}
	}
	for _, d := range no {
		if predictable(d) {
			t.Fatalf("%q should not be predictable", d)
		}
	}
}

type predictLLM struct{ reply string }

func (p predictLLM) Complete(ctx context.Context, system string, msgs []llm.Message) (string, error) {
	return p.reply, nil
}

// Tab takes the guess; a guess made for older text is never applied to
// what is in the composer now.
func TestSuggestionAcceptedByTab(t *testing.T) {
	m := testModel(t)
	cfg := m.cfg.Load()
	cfg.small = predictLLM{}
	m.cfg.Store(cfg)

	m.setDraft("fix the flaky")
	m.finishPredict(predictMsg{draft: "fix the flaky", text: " golden test"})
	if got := m.suggestion(); got != " golden test" {
		t.Fatalf("suggestion = %q", got)
	}
	if bar := m.statusBar(m.cfg.Load()); !strings.Contains(bar, "↹ …golden test") {
		t.Fatalf("status bar = %q", bar)
	}
	if !m.acceptSuggestion() {
		t.Fatal("tab did not take the suggestion")
	}
	if got := m.input.Value(); got != "fix the flaky golden test" {
		t.Fatalf("draft = %q", got)
	}
	if m.suggestion() != "" {
		t.Fatal("the suggestion outlived being accepted")
	}

	// A guess for text the user has moved past is dropped.
	m.setDraft("something else entirely")
	m.finishPredict(predictMsg{draft: "fix the flaky", text: " golden test"})
	if m.suggestion() != "" {
		t.Fatalf("a stale guess was kept: %q", m.suggestion())
	}
	if m.acceptSuggestion() {
		t.Fatal("tab took a suggestion that was not offered")
	}
}

// Without an llm-small row nothing is scheduled and nothing is shown:
// the composer's guess is never worth the agent's own model.
func TestNoPredictionWithoutSmallModel(t *testing.T) {
	m := testModel(t)
	m.setDraft("fix the flaky")
	if cmd := m.schedulePredict(m.cfg.Load()); cmd != nil {
		t.Fatal("scheduled a prediction with no llm-small row")
	}
	if cmd := m.startPredict(m.cfg.Load(), "fix the flaky"); cmd != nil {
		t.Fatal("started a prediction with no llm-small row")
	}
}

// While the agent is running, the bottom line says what it is doing
// instead of the session name.
func TestActivityLine(t *testing.T) {
	m := testModel(t)
	m.addEvent(Event{Kind: "title", Text: "Fix the flaky golden test"})
	m.running = true // a label only ever arrives mid-turn
	m.addEvent(Event{Kind: "activity", Text: "running the test suite"})
	if len(m.blocks) != 0 {
		t.Fatalf("activity added %d blocks", len(m.blocks))
	}
	if bar := m.statusBar(m.cfg.Load()); !strings.Contains(bar, "▸ running the test suite") {
		t.Fatalf("running bar = %q", bar)
	}
	// The turn ends: the plugin clears the label and the bar goes back
	// to the session name. A label that lands after the turn is
	// dropped rather than captioning the next one.
	m.running = false
	m.addEvent(Event{Kind: "activity", Text: ""})
	m.addEvent(Event{Kind: "activity", Text: "a late label"})
	if m.activity != "" {
		t.Fatalf("a late label was kept: %q", m.activity)
	}
	if bar := m.statusBar(m.cfg.Load()); !strings.Contains(bar, "Fix the flaky golden test") {
		t.Fatalf("idle bar = %q", bar)
	}
}
