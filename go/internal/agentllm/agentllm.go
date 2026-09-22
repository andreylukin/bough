// Package agentllm is the seam between the llm rows (which own keys,
// model and effort) and an engine that drives a harness adapter.
package agentllm

import (
	"context"
	"errors"
	"time"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
)

type DeltaKind string

const (
	DeltaStart     DeltaKind = "start"      // a provider request began (the Gate emits it)
	DeltaText      DeltaKind = "text"       // → assistant-delta
	DeltaThinking  DeltaKind = "thinking"   // → thinking-delta
	DeltaToolStart DeltaKind = "tool_start" // → activity "writing <Name> call"
	DeltaRetry     DeltaKind = "retry"      // → system "provider hiccup — retrying…" (not recorded)
)

// Delta is one live fragment. Seq identifies the Gate's Respond call
// (SeqOf(ctx)); a consumer drops deltas from any Seq but the newest and
// resets its partial text when Attempt changes.
type Delta struct {
	Seq     uint64
	Attempt int
	Kind    DeltaKind
	Text    string
	CallID  string
	Name    string
	Wait    time.Duration // retry only
	Err     string        // retry only: the error being retried
}

// Exchange is one request/response pair for the trace file. Adapters
// never put credentials in it; the engine redacts again before writing.
type Exchange struct {
	Provider string
	Attempt  int
	Status   int
	Request  []byte
	Response []byte
	Err      string
}

type Options struct {
	Session string // harness session id; cache key and per-session adapter state
	Worker  string // "" for the main agent
	Sink    func(Delta)
	Trace   func(Exchange) // nil = off
}

// Adapter is a harness adapter an llm row built for one session.
type Adapter interface {
	ullm.Adapter
	Provider() string // envelope family: "anthropic", "openai", "openrouter:<vendor>", "ollama", "echo", "fake"
	Model() string    // the model id this adapter is calling right now
	Close() error
}

// Source is the optional seam on the "llm" service value. A row
// without it cannot drive the engine.
type Source interface {
	AgentAdapter(o Options) (Adapter, error)
}

type seqKey struct{}

func WithSeq(ctx context.Context, seq uint64) context.Context {
	return context.WithValue(ctx, seqKey{}, seq)
}

func SeqOf(ctx context.Context) uint64 {
	s, _ := ctx.Value(seqKey{}).(uint64)
	return s
}

// ErrContextOverflow is wrapped by adapters when the provider says the
// prompt no longer fits. The Gate makes it sticky until the model changes.
var ErrContextOverflow = errors.New("context window exceeded")
