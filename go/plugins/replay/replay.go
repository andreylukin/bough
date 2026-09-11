// Package replay plays a recorded session back through the real loop
// and TUI without a model or a shell: the "llm" service answers each
// call with the next recorded assistant reply, and the "codemode"
// service answers each block with the recorded result of that block.
// Every ~/.bough/history/*.jsonl file is therefore a free, deterministic
// TUI test case, hours of real usage included.
//
//   - id: llm
//     plugin: replay
//     config: {file: /Users/me/.bough/history/<id>.jsonl}  # or session: <id>
//   - id: codemode
//     plugin: replay
//     config: {file: ..., provide: codemode}               # the same file
//
// delay_ms (llm side) pauses between streamed words; default 0.
//
// Only the top-level transcript is replayed: sub:* entries were produced
// by subagents, whose spawn call is itself a block with a recorded
// result, so they never run.
package replay

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

func init() {
	kernel.Register("replay", func() kernel.Plugin { return &plugin{} })
}

type plugin struct{}

func (plugin) Name() string     { return "replay" }
func (plugin) Inject() []string { return nil }

// Apply provides the model side under "llm" (or `service`), or the
// runtime side when `provide: codemode`.
func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	path, _ := cfg["file"].(string)
	if id, _ := cfg["session"].(string); path == "" && id != "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return err
		}
		path = filepath.Join(home, ".bough", "history", id+".jsonl")
	}
	if path == "" {
		return errors.New("replay: file or session is required")
	}
	tape, err := Load(path)
	if err != nil {
		return err
	}
	if ms, ok := cfg["delay_ms"].(int); ok {
		tape.Delay = time.Duration(ms) * time.Millisecond
	} else if ms, ok := cfg["delay_ms"].(float64); ok {
		tape.Delay = time.Duration(ms * float64(time.Millisecond))
	}
	if p, _ := cfg["provide"].(string); p == "codemode" {
		ctx.Provide("codemode", &Runtime{CodeMode: codemode.New(30 * time.Second), tape: tape})
		return nil
	}
	key := "llm"
	if s, ok := cfg["service"].(string); ok && s != "" {
		key = s
	}
	ctx.Provide(key, &Model{tape: tape})
	return nil
}

// Tape is a recorded session split into what the model said and what
// the runtime answered, in order. The two cursors are independent:
// the loop calls the model, then runs the block, then calls again.
type Tape struct {
	Path    string
	Inputs  []string // what the user typed, for a driver to replay
	Replies []string // assistant entries, top level only
	Results []result // result entries, top level only
	Delay   time.Duration

	mu sync.Mutex
	ri int
	xi int
}

type result struct {
	code string
	text string
}

// Load reads a history file into a Tape.
func Load(path string) (*Tape, error) {
	entries, err := history.Read(path)
	if err != nil {
		return nil, fmt.Errorf("replay: %w", err)
	}
	t := &Tape{Path: path}
	for _, e := range entries {
		text, _ := e.Data["text"].(string)
		switch e.Kind {
		case "input":
			t.Inputs = append(t.Inputs, text)
		case "assistant":
			t.Replies = append(t.Replies, text)
		case "result":
			code, _ := e.Data["code"].(string)
			t.Results = append(t.Results, result{code: code, text: text})
		}
	}
	if len(t.Replies) == 0 {
		return nil, fmt.Errorf("replay: %s has no assistant entries", path)
	}
	return t, nil
}

// Turns is how many user inputs the tape holds.
func (t *Tape) Turns() int { return len(t.Inputs) }

func (t *Tape) nextReply() (string, bool) {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.ri >= len(t.Replies) {
		return "", false
	}
	r := t.Replies[t.ri]
	t.ri++
	return r, true
}

// nextResult prefers the recorded result of this exact block, looking
// forward from the cursor; a block the recording never ran (the loop
// changed since) takes the next result in order so the tape keeps
// moving.
func (t *Tape) nextResult(code string) (string, bool) {
	t.mu.Lock()
	defer t.mu.Unlock()
	for i := t.xi; i < len(t.Results) && i < t.xi+3; i++ {
		if strings.TrimSpace(t.Results[i].code) == strings.TrimSpace(code) {
			r := t.Results[i]
			t.xi = i + 1
			return r.text, true
		}
	}
	if t.xi >= len(t.Results) {
		return "", false
	}
	r := t.Results[t.xi]
	t.xi++
	return r.text, true
}

// Model is the llm side of the tape.
type Model struct{ tape *Tape }

// Complete returns the next recorded reply. Past the end of the tape
// it stops the turn, so a driver that sends more inputs than the
// recording had still gets a clean done.
func (m *Model) Complete(ctx context.Context, system string, messages []llm.Message) (string, error) {
	r, ok := m.tape.nextReply()
	if !ok {
		return "```stop\n[replay: end of tape]\n```", nil
	}
	return r, nil
}

// Stream delivers the reply one word at a time, pausing Delay between
// words, so the live block renders as it would from a real provider.
func (m *Model) Stream(ctx context.Context, system string, messages []llm.Message, onDelta func(string)) (string, error) {
	reply, err := m.Complete(ctx, system, messages)
	if err != nil {
		return "", err
	}
	rest := reply
	for rest != "" {
		if ctx.Err() != nil {
			return "", ctx.Err()
		}
		i := strings.IndexAny(rest, " \n")
		if i < 0 {
			i = len(rest) - 1
		}
		onDelta(rest[:i+1])
		rest = rest[i+1:]
		if m.tape.Delay > 0 {
			select {
			case <-time.After(m.tape.Delay):
			case <-ctx.Done():
				return "", ctx.Err()
			}
		}
	}
	return reply, nil
}

// Runtime is the codemode side of the tape: a real codemode (so every
// row that registers tools, runs hooks or loads init.js mounts as
// usual) whose Run answers from the recording instead of executing.
type Runtime struct {
	*codemode.CodeMode
	tape *Tape
}

// Run answers with the recorded result of the block. A result the
// recording stored as "error: ..." comes back as an error, so the UI
// draws it the way it did live.
func (r *Runtime) Run(code string) (string, error) {
	out, ok := r.tape.nextResult(code)
	if !ok {
		return "", errors.New("replay: end of tape")
	}
	if msg, found := strings.CutPrefix(out, "error: "); found {
		return "", errors.New(msg)
	}
	if i := strings.LastIndex(out, "\nerror: "); i >= 0 {
		return out[:i+1], errors.New(out[i+len("\nerror: "):])
	}
	return out, nil
}

// RunCtx is Run: the recording cannot be cancelled mid-block.
func (r *Runtime) RunCtx(ctx context.Context, code string) (string, error) {
	return r.Run(code)
}
