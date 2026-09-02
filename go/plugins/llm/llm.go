// Package llm provides LLM provider plugins and the "llm" service contract.
package llm

import (
	"context"
	"fmt"
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

// Usage is a provider's running token/cost tally for this mount (it
// resets when the llm row is swapped). Cost is only meaningful when
// Priced: OpenRouter reports it per response, Anthropic does not.
type Usage struct {
	InputTokens  int
	OutputTokens int
	Cost         float64
	Priced       bool
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
	if u.Priced {
		return fmt.Sprintf("$%.4f", u.Cost)
	}
	return kilo(u.InputTokens+u.OutputTokens) + " tok"
}

func kilo(n int) string {
	if n >= 1000 {
		return fmt.Sprintf("%.1fk", float64(n)/1000)
	}
	return fmt.Sprint(n)
}
