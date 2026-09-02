// Package llm provides LLM provider plugins and the "llm" service contract.
package llm

import "context"

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
