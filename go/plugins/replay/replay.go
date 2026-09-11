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
// An "error" entry that ends a turn (the next top-level entry is its
// done) is a failed model call: Complete returns it at that point. A
// done's recorded usage is reported (llm.UsageReporter) once the call
// before it has been answered, and an assistant entry's recorded model
// is what Model() names, so the cost and model chips work under replay.
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
	"github.com/andreylukin/bough/plugins/loop"
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

	mu    sync.Mutex
	calls []call // every model call in order: replies and failures
	ri    int
	xi    int
}

// call is one recorded model call: a reply, or an error when err is
// set. usage is what the done that followed it recorded.
type call struct {
	text  string
	err   string
	model string
	usage llm.Usage
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
	var top []history.Entry
	for _, e := range entries {
		if !strings.HasPrefix(e.Kind, "sub:") {
			top = append(top, e)
		}
	}
	for i, e := range top {
		text, _ := e.Data["text"].(string)
		switch e.Kind {
		case "input":
			t.Inputs = append(t.Inputs, text)
		case "assistant":
			t.Replies = append(t.Replies, text)
			model, _ := e.Data["model"].(string)
			t.calls = append(t.calls, call{text: text, model: model})
		case "error":
			if i+1 < len(top) && top[i+1].Kind == "done" && text != "" {
				t.calls = append(t.calls, call{err: text})
			}
		case "done":
			if n := len(t.calls); n > 0 {
				t.calls[n-1].usage = loop.SumUsage([]history.Entry{e})
			}
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

func (t *Tape) nextCall() (call, bool) {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.ri >= len(t.calls) {
		return call{}, false
	}
	c := t.calls[t.ri]
	t.ri++
	return c, true
}

// served sums the usage of every call answered so far and names the
// model of the latest reply that recorded one.
func (t *Tape) served() (llm.Usage, string) {
	t.mu.Lock()
	defer t.mu.Unlock()
	var u llm.Usage
	model := ""
	for _, c := range t.calls[:t.ri] {
		u.InputTokens += c.usage.InputTokens
		u.OutputTokens += c.usage.OutputTokens
		u.CacheReadTokens += c.usage.CacheReadTokens
		u.CacheCreationTokens += c.usage.CacheCreationTokens
		if c.usage.LastInputTokens > 0 {
			u.LastInputTokens = c.usage.LastInputTokens
		}
		u.Cost += c.usage.Cost
		u.Priced = u.Priced || c.usage.Priced
		if c.model != "" {
			model = c.model
		}
	}
	return u, model
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
type Model struct {
	tape *Tape

	mu   sync.Mutex
	seen [][]llm.Message
}

// Complete returns the next recorded reply, or the recorded error of a
// failed call. Past the end of the tape it stops the turn, so a driver
// that sends more inputs than the recording had still gets a clean done.
func (m *Model) Complete(ctx context.Context, system string, messages []llm.Message) (string, error) {
	m.mu.Lock()
	m.seen = append(m.seen, append([]llm.Message(nil), messages...))
	m.mu.Unlock()
	c, ok := m.tape.nextCall()
	if !ok {
		return "```stop\n[replay: end of tape]\n```", nil
	}
	if c.err != "" {
		return "", errors.New(c.err)
	}
	return c.text, nil
}

// Messages is what each call so far was sent, oldest first, so a test
// can assert what the model saw.
func (m *Model) Messages() [][]llm.Message {
	m.mu.Lock()
	defer m.mu.Unlock()
	return append([][]llm.Message(nil), m.seen...)
}

// Usage implements llm.UsageReporter from the usage the tape's done
// entries recorded, summed over the calls answered so far.
func (m *Model) Usage() llm.Usage {
	u, _ := m.tape.served()
	return u
}

// Model implements llm.Modeler: the model the latest answered reply
// recorded, "" before one has.
func (m *Model) Model() string {
	_, model := m.tape.served()
	return model
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

// RunCtx is Run, unless ctx is already done: a cancelled block
// returns ctx's error and leaves the tape where it was.
func (r *Runtime) RunCtx(ctx context.Context, code string) (string, error) {
	if err := ctx.Err(); err != nil {
		return "", err
	}
	return r.Run(code)
}
