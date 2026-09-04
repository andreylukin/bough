package ui

// Prompt autocomplete: while you pause over a half-written message, a
// small model guesses the rest and tab takes it. The guess is shown on
// the status line rather than inside the textarea — ghost text inside
// a wrapped, multi-line composer moves the cursor around and hides the
// draft's real end; a line that says "↹ …the rest" costs nothing and
// is never mistaken for what you typed.
//
// It runs only on an llm-small row (never the agent's model), only
// while idle with a draft that looks unfinished, and one call at a
// time. Anything you type invalidates a guess in flight.

import (
	"context"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/andreylukin/bough/plugins/llm"
)

// predictPrompt asks for a continuation, not an answer.
const predictPrompt = `You complete a half-typed instruction that a developer is writing to a coding agent.

Continue their text from exactly where it stops. Answer with the CONTINUATION ONLY — no repetition of what they wrote, no quotes, no explanation, one line, at most 12 words. Keep their voice and their spelling.

If the text already reads as a finished instruction, or you cannot guess what comes next, answer with the single word NONE. Never describe what you are doing; never answer their question.`

// predictDelay is the pause that means "thinking about it", not
// "mid-word".
const predictDelay = 500 * time.Millisecond

// predictMax bounds how long a suggestion may be.
const predictMax = 120

// predictMinDraft is the shortest draft worth guessing at: a couple of
// characters carry no intent.
const predictMinDraft = 8

// predictState is the composer's suggestion, keyed by the draft it was
// made for so a stale answer is dropped.
type predictState struct {
	forDraft string // the draft the suggestion continues
	text     string // the continuation itself
	pending  string // the draft a call is in flight for
}

// predictMsg carries a finished guess back into Update.
type predictMsg struct {
	draft string
	text  string
}

// predictTickMsg fires after the typing pause.
type predictTickMsg struct{ draft string }

// suggestion is the continuation on offer for the current draft, "" if
// none (or the draft has moved on since).
func (m *model) suggestion() string {
	if m.pred.text == "" || m.pred.forDraft != m.input.Value() {
		return ""
	}
	return m.pred.text
}

// schedulePredict is called after every edit: it arms the pause timer.
// The draft is carried in the message so a tick for older text is
// ignored without any extra bookkeeping.
func (m *model) schedulePredict(cfg *uiCfg) tea.Cmd {
	if cfg.small == nil || m.running || m.pendingAsk != "" {
		return nil
	}
	draft := m.input.Value()
	if !predictable(draft) {
		m.pred = predictState{}
		return nil
	}
	if m.pred.forDraft != draft {
		m.pred.text = "" // the draft moved: the old guess is not for it
	}
	return tea.Tick(predictDelay, func(time.Time) tea.Msg { return predictTickMsg{draft: draft} })
}

// predictable rejects drafts a guess would only get in the way of: too
// short, a command, a shell line, or one that already ends in
// punctuation (a finished sentence).
func predictable(draft string) bool {
	if len(draft) < predictMinDraft || strings.HasPrefix(draft, "/") || strings.HasPrefix(draft, "!") {
		return false
	}
	if strings.HasSuffix(draft, "\n") {
		return false
	}
	switch draft[len(draft)-1] {
	case '.', '?', '!', '"', ')':
		return false
	}
	return true
}

// startPredict fires the call if the draft is still the one the tick
// was armed for.
func (m *model) startPredict(cfg *uiCfg, draft string) tea.Cmd {
	if cfg.small == nil || m.running || draft != m.input.Value() || m.pred.pending == draft {
		return nil
	}
	m.pred.pending = draft
	small := cfg.small
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		reply, err := small.Complete(ctx, predictPrompt, []llm.Message{{Role: "user", Content: draft}})
		if err != nil {
			return predictMsg{draft: draft}
		}
		return predictMsg{draft: draft, text: cleanSuggestion(draft, reply)}
	}
}

// finishPredict lands a guess, dropping one the draft has outrun.
func (m *model) finishPredict(msg predictMsg) {
	if m.pred.pending == msg.draft {
		m.pred.pending = ""
	}
	if msg.draft != m.input.Value() || msg.text == "" {
		return
	}
	m.pred = predictState{forDraft: msg.draft, text: msg.text}
}

// acceptSuggestion appends the continuation to the draft. True when
// there was one — tab falls through to path completion otherwise.
func (m *model) acceptSuggestion() bool {
	s := m.suggestion()
	if s == "" {
		return false
	}
	m.setDraft(m.input.Value() + s)
	m.pred = predictState{}
	return true
}

// refusals are what a small model says instead of staying quiet. Shown
// as a completion they read as text to accept, which is worse than no
// suggestion at all ("↹ …No output." was the first one seen live).
var refusals = []string{"none", "no output", "nothing", "n/a", "na", "no continuation", "empty", "no suggestion"}

// cleanSuggestion takes the continuation out of whatever came back: a
// small model likes to repeat the prompt, quote itself, refuse in
// words, or answer the question instead of finishing it.
func cleanSuggestion(draft, reply string) string {
	s := strings.TrimSpace(reply)
	if i := strings.IndexByte(s, '\n'); i >= 0 {
		s = s[:i]
	}
	s = strings.Trim(s, `"'`)
	// Repeated the draft: keep only what comes after it.
	if len(s) > len(draft) && strings.EqualFold(s[:len(draft)], draft) {
		s = s[len(draft):]
	}
	if s == "" || len(s) > predictMax {
		return ""
	}
	bare := strings.ToLower(strings.Trim(s, " .!"))
	for _, r := range refusals {
		if bare == r {
			return ""
		}
	}
	if len(strings.Fields(s)) > 12 {
		return ""
	}
	// Join with a space unless one side already has the separator.
	if !strings.HasSuffix(draft, " ") && !strings.HasPrefix(s, " ") &&
		!strings.ContainsAny(s[:1], ",.;:!?)") {
		s = " " + s
	}
	if strings.TrimSpace(s) == "" {
		return ""
	}
	return s
}
