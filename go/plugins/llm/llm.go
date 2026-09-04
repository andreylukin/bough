// Package llm provides LLM provider plugins and the "llm" service contract.
package llm

import (
	"context"
	"fmt"
	"slices"

	"github.com/andreylukin/bough/kernel"
)

// Message is one turn of a conversation.
type Message struct {
	Role    string // "user" or "assistant"
	Content string
}

// LLM is the "llm" service contract. Consumers do:
//
//	kernel.Get[llm.LLM](ctx, "llm")
type LLM interface {
	Complete(ctx context.Context, system string, messages []Message) (string, error)
}

// Streamer is the optional streaming seam: a provider that can stream
// calls onDelta with each text fragment as it arrives and still
// returns the complete reply (or the error). Consumers fall back to
// Complete when the service does not implement it.
type Streamer interface {
	Stream(ctx context.Context, system string, messages []Message, onDelta func(string)) (string, error)
}

// ThinkingStreamer is the optional seam for a provider that also
// streams the model's reasoning. onThink receives the reasoning
// fragments, onDelta the reply as usual; the return value is the reply
// alone, so reasoning never becomes part of the conversation the model
// is fed back.
type ThinkingStreamer interface {
	StreamThinking(ctx context.Context, system string, messages []Message, onDelta, onThink func(string)) (string, error)
}

// Efforter is the optional seam for changing how hard the model thinks
// at runtime (the /think command). "" is the provider's own default.
type Efforter interface {
	Effort() string
	SetEffort(level string) error
}

// Efforts are the levels Efforter accepts, weakest first; "off" asks
// the provider for no reasoning at all and "" restores its default.
var Efforts = []string{"off", "low", "medium", "high", "xhigh"}

// ValidEffort reports whether level is one Efforter accepts.
func ValidEffort(level string) bool {
	return level == "" || slices.Contains(Efforts, level)
}

// serviceKey is the service a provider row publishes under: "llm" (the
// agent's model) unless the row says otherwise. A second row with
// {service: llm-small} gives the harness a cheap model for the jobs
// that are not the conversation — naming a session, extracting a fact
// worth remembering — the way opencode's small_model does.
func serviceKey(cfg map[string]any) string {
	if s, ok := cfg["service"].(string); ok && s != "" {
		return s
	}
	return "llm"
}

// SmallKey is the service key of that cheap model.
const SmallKey = "llm-small"

// Small returns the small model, falling back to the main one when no
// row provides it (so a caller never has to branch), plus whether the
// fallback happened.
func Small(ctx *kernel.Context) (LLM, bool) {
	if l, err := kernel.Get[LLM](ctx, SmallKey); err == nil {
		return l, true
	}
	l, err := kernel.Get[LLM](ctx, "llm")
	if err != nil {
		return nil, false
	}
	return l, false
}

// Name is a provider's model id, "" when it does not say.
func Name(l LLM) string {
	if m, ok := l.(Modeler); ok {
		return m.Model()
	}
	return ""
}

// Usage is a provider's running token/cost tally for this mount (it
// resets when the llm row is swapped). Cost is only meaningful when
// Priced: OpenRouter reports it per response, Anthropic does not.
// LastInputTokens is the most recent request's input alone — the
// size of the context the model last saw — for the status bar's
// context percentage; 0 when the provider reports no per-request usage.
type Usage struct {
	InputTokens     int
	OutputTokens    int
	LastInputTokens int
	Cost            float64
	Priced          bool
}

// Modeler is the optional seam naming the model an llm service runs;
// the cost row prices a token tally by it.
type Modeler interface {
	Model() string
}

// UsageReporter is the optional seam an llm service exposes for the
// status bar and /cost; a provider that cannot count stays silent.
type UsageReporter interface {
	Usage() Usage
}

// String renders the tally as "in/out tokens" plus the cost when
// priced; "" when nothing has been used.
func (u Usage) String() string {
	if u.InputTokens == 0 && u.OutputTokens == 0 {
		return ""
	}
	s := fmt.Sprintf("%s in · %s out", kilo(u.InputTokens), kilo(u.OutputTokens))
	if u.Priced {
		s += fmt.Sprintf(" · $%.4f", u.Cost)
	}
	return s
}

// Short renders the tally for the status bar: the cost when priced,
// else the total tokens; "" when nothing has been used.
func (u Usage) Short() string {
	if u.InputTokens == 0 && u.OutputTokens == 0 {
		return ""
	}
	tok := kilo(u.InputTokens+u.OutputTokens) + " tok"
	if u.Priced {
		return fmt.Sprintf("$%.4f · %s", u.Cost, tok)
	}
	return tok
}

func kilo(n int) string {
	if n >= 1000 {
		return fmt.Sprintf("%.1fk", float64(n)/1000)
	}
	return fmt.Sprint(n)
}
