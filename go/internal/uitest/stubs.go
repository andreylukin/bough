package uitest

// LLM stubs for TUI-integration tests: scripted replies, streaming with
// hostile chunking, failures, and a provider that hangs until the turn
// is cancelled. Mount one with
//
//	uitest.Mount(t, func(c *kernel.Context) { c.Provide("llm", stub) }, "codemode", "loop")

import (
	"context"
	"errors"
	"strings"
	"sync"
	"unicode/utf8"

	"github.com/andreylukin/bough/plugins/llm"
)

// Script replies with each string in turn, then repeats the last one.
// Calls counts completions.
type Script struct {
	Replies []string
	mu      sync.Mutex
	Calls   int
}

func (s *Script) Complete(_ context.Context, _ string, _ []llm.Message) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	i := s.Calls
	s.Calls++
	if i >= len(s.Replies) {
		i = len(s.Replies) - 1
	}
	if i < 0 {
		return "", nil
	}
	return endTurn(s.Replies[i]), nil
}

// endTurn wraps a fixture's plain prose in a stop block, the way a
// model that follows the contract would. A reply that already carries
// a js or stop block is left exactly as written, so a test can still
// script the reply that ends nothing (Raw).
func endTurn(reply string) string {
	if strings.Contains(reply, "```") || strings.HasPrefix(reply, rawMarker) {
		return strings.TrimPrefix(reply, rawMarker)
	}
	return Stop(reply)
}

// rawMarker opts a fixture out of the automatic stop block: prefix a
// reply with it to script a model that neither runs nor stops.
const rawMarker = "\x00raw\x00"

// Raw is a reply the loop will find neither a js block nor a stop
// block in — the shape that gets asked again.
func Raw(text string) string { return rawMarker + text }

// Stop is a reply that ends the turn with text as its answer.
func Stop(text string) string { return "```stop\n" + text + "\n```" }

// Chunker splits a reply into the deltas a streaming provider would send.
type Chunker func(reply string) []string

// ByRune streams one rune at a time (the slowest realistic provider).
func ByRune(reply string) []string {
	var out []string
	for _, r := range reply {
		out = append(out, string(r))
	}
	return out
}

// ByN streams fixed n-rune chunks, which lands chunk boundaries inside
// words, inside "```" fences and between a combining mark and its base.
func ByN(n int) Chunker {
	return func(reply string) []string {
		var out []string
		rs := []rune(reply)
		for i := 0; i < len(rs); i += n {
			j := min(i+n, len(rs))
			out = append(out, string(rs[i:j]))
		}
		return out
	}
}

// ByBytes streams fixed byte chunks: a multi-byte rune can straddle
// two deltas, as it does when a proxy re-chunks an SSE stream.
func ByBytes(n int) Chunker {
	return func(reply string) []string {
		var out []string
		for i := 0; i < len(reply); i += n {
			out = append(out, reply[i:min(i+n, len(reply))])
		}
		return out
	}
}

// Whole streams the reply as one delta.
func Whole(reply string) []string { return []string{reply} }

// Streaming wraps Script as a Streamer, chunking each reply with Chunk.
// Deltas are joined back for the returned reply, so a chunker that
// splits runes still returns a well-formed final text (the way a
// provider's final message is the canonical one).
type Streaming struct {
	Script
	Chunk Chunker
}

func (s *Streaming) Stream(ctx context.Context, sys string, msgs []llm.Message, onDelta func(string)) (string, error) {
	reply, err := s.Complete(ctx, sys, msgs)
	if err != nil {
		return "", err
	}
	chunk := s.Chunk
	if chunk == nil {
		chunk = Whole
	}
	for _, d := range chunk(reply) {
		if ctx.Err() != nil {
			return "", ctx.Err()
		}
		onDelta(d)
	}
	return reply, nil
}

// InvalidUTF8 reports whether any delta is not valid UTF-8 on its own.
func InvalidUTF8(chunks []string) bool {
	for _, c := range chunks {
		if !utf8.ValidString(c) {
			return true
		}
	}
	return false
}

// Failing fails every completion with Err until After calls have
// happened (After 0 = always), then behaves like Script.
type Failing struct {
	Script
	Err   error
	After int
}

func (f *Failing) Complete(ctx context.Context, sys string, msgs []llm.Message) (string, error) {
	f.mu.Lock()
	n := f.Calls
	f.mu.Unlock()
	if f.After == 0 || n < f.After {
		f.mu.Lock()
		f.Calls++
		f.mu.Unlock()
		return "", f.Err
	}
	return f.Script.Complete(ctx, sys, msgs)
}

// ErrProvider is the error Failing uses when none is given.
var ErrProvider = errors.New("provider: 503 overloaded")

// Slow blocks until the context is cancelled (the user's esc / ctrl+c)
// or Release is closed, then replies "late".
type Slow struct {
	Release chan struct{}
	Started chan struct{} // closed once the first completion begins
	once    sync.Once
}

func NewSlow() *Slow {
	return &Slow{Release: make(chan struct{}), Started: make(chan struct{})}
}

func (s *Slow) Complete(ctx context.Context, _ string, _ []llm.Message) (string, error) {
	s.once.Do(func() { close(s.Started) })
	select {
	case <-ctx.Done():
		return "", ctx.Err()
	case <-s.Release:
		return "late", nil
	}
}

// JS wraps a JavaScript snippet in the fence the loop executes.
func JS(code string) string { return "```js\n" + code + "\n```" }

// Bash is a reply that runs one shell command through tools.bash and
// makes its output the block result.
func Bash(cmd string) string {
	return JS("tools.bash(" + jsString(cmd) + ")")
}

func jsString(s string) string {
	r := strings.NewReplacer(`\`, `\\`, `"`, `\"`, "\n", `\n`)
	return `"` + r.Replace(s) + `"`
}
