// Package loop is the agent loop plugin: it owns the conversation,
// provides the "inputs" channel and the "runner" service, and drives
// the codemode loop (llm -> extract js -> run -> feed back).
//
// Conversation state lives in history entries (the optional "history"
// service makes them durable; absent, a process-local list is used).
// Model messages are derived from entries by the optional "projection"
// service (DefaultProject otherwise), and the system prompt may be
// transformed by the optional "cognition" service.
package loop

import (
	"context"
	"fmt"
	"regexp"
	"sync"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

// Message is one turn of conversation. The seam type is owned by
// plugins/llm; this alias keeps the two packages nominally identical
// so kernel.Get[LLM] succeeds (llm does not import loop, so no cycle).
type Message = llm.Message

// LLM is the "llm" service seam.
type LLM = llm.LLM

// Codemode is the "codemode" service seam.
type Codemode interface {
	RegisterTool(name string, fn any)
	Run(code string) (string, error)
}

// Hooks is the optional "hooks" service seam. Fire runs every hook
// file for event and returns the merged result object (nil if none).
type Hooks interface {
	Fire(ctx context.Context, event string, payload map[string]any) (map[string]any, error)
}

// Skills is the optional "skills" service seam. Inject returns
// ready-formatted "[skill: <name>]\n<body>" blocks for skills whose
// name is mentioned in the human input.
type Skills interface {
	Inject(input string) []string
}

// SystemContext is the optional "context-md" service seam. Preamble
// is prepended to the system prompt once, at session start.
type SystemContext interface {
	Preamble() string
}

// History is the optional "history" service seam: append-only session
// record. Absent, the loop keeps a process-local in-memory list.
type History interface {
	Append(kind string, data map[string]any) history.Entry
	Entries() []history.Entry
	Path() string
}

// Cognition is the optional "cognition" service seam: transforms or
// replaces the built default system prompt.
type Cognition interface {
	System(base string) string
}

// Projection is the optional "projection" service seam: derives the
// model messages from the history entries each step.
type Projection interface {
	Project(entries []history.Entry) []llm.Message
}

// Event is the payload emitted on "loop/event".
// Kind is one of: "assistant", "code", "result", "error", "done" from
// the loop itself; other plugins may emit further kinds (e.g. workers'
// "sub:*" subagent events). Data carries optional extra payload (e.g.
// {"worker": N} on sub:* events); nil for the loop's own events.
type Event struct {
	Kind string
	Text string
	Data map[string]any
}

const maxSteps = 10
const maxResultBytes = 8 * 1024

const systemPrompt = `You are bough, a coding agent. You act by writing JavaScript
in fenced code blocks:

` + "```js" + `
console.log(tools.bash("ls"))
` + "```" + `

Available in the runtime:
- tools.bash(cmd) -> string: run a shell command, returns its output
- tools.readFile(path) -> string: read a file
- tools.writeFile(path, content): write a file
- console.log(...): print; everything printed is returned to you

Each code block you write is executed and its output is sent back to you
as the next message. Take as many steps as you need. When you are done,
reply with plain text only — no code block — and that ends the turn.`

// askPromptSection documents tools.ask; appended to the system prompt
// only when an "ask-answers" service (the ask plugin) is mounted. The
// options nudge matters: options inlined into the question string
// render as plain text, separate arguments render as clickable option
// rows in the UI.
const askPromptSection = `You may ask the user a question from code: tools.ask(question, ...options) -> string blocks until they answer and returns the answer. Pass each option as a separate argument — tools.ask(question, opt1, opt2, ...) — so they render as clickable choices; never inline the options into the question text.`

var jsBlock = regexp.MustCompile("(?s)```js\\s*\n(.*?)```")

// DefaultProject is the built-in history -> model-messages projection:
// input -> user, assistant -> assistant, result -> user "[tool output]\n...".
// Other kinds (code, error, done) carry no model-visible text. Pure:
// no state, entries in -> messages out.
func DefaultProject(entries []history.Entry) []llm.Message {
	var msgs []llm.Message
	for _, e := range entries {
		text, _ := e.Data["text"].(string)
		switch e.Kind {
		case "input":
			msgs = append(msgs, llm.Message{Role: "user", Content: text})
		case "assistant":
			msgs = append(msgs, llm.Message{Role: "assistant", Content: text})
		case "result":
			msgs = append(msgs, llm.Message{Role: "user", Content: "[tool output]\n" + text})
		}
	}
	return msgs
}

// memHistory is the fallback History when no "history" service is
// mounted: same contract, process-local, gone at exit.
type memHistory struct {
	mu      sync.Mutex
	entries []history.Entry
}

func (m *memHistory) Append(kind string, data map[string]any) history.Entry {
	m.mu.Lock()
	defer m.mu.Unlock()
	e := history.Entry{Seq: int64(len(m.entries) + 1), At: time.Now(), Kind: kind, Data: data}
	m.entries = append(m.entries, e)
	return e
}

func (m *memHistory) Entries() []history.Entry {
	m.mu.Lock()
	defer m.mu.Unlock()
	return append([]history.Entry(nil), m.entries...)
}

func (m *memHistory) Path() string { return "" }

// runner implements the "runner" service. hooks, skills, sysctx, cog
// and proj are optional seams; nil means built-in behavior. hist is
// never nil (memHistory fallback).
type runner struct {
	mu      sync.Mutex
	llm     LLM
	code    Codemode
	hooks   Hooks
	skills  Skills
	sysctx  SystemContext
	hist    History
	cog     Cognition
	proj    Projection
	hasAsk  bool // an "ask-answers" service is mounted: document tools.ask
	system  string
	started bool
}

// fire runs a hook event if a hooks service is present. A Fire error
// is logged as a loop error event and treated as no-op, never fatal.
func (r *runner) fire(ctx context.Context, event string, payload map[string]any, emit func(kind, text string)) map[string]any {
	if r.hooks == nil {
		return nil
	}
	res, err := r.hooks.Fire(ctx, event, payload)
	if err != nil {
		emit("error", "hook "+event+": "+err.Error())
		return nil
	}
	return res
}

// project derives this step's model messages from the history entries.
func (r *runner) project() []Message {
	entries := r.hist.Entries()
	if r.proj != nil {
		return r.proj.Project(entries)
	}
	return DefaultProject(entries)
}

func (r *runner) Run(ctx context.Context, input string, emit func(kind, text string)) error {
	r.mu.Lock()
	defer r.mu.Unlock()

	// note appends a history entry and emits the matching event.
	note := func(kind, text string, extra map[string]any) {
		data := map[string]any{"text": text}
		for k, v := range extra {
			data[k] = v
		}
		r.hist.Append(kind, data)
		emit(kind, text)
	}

	if !r.started {
		r.started = true
		r.system = systemPrompt
		if r.hasAsk {
			r.system += "\n\n" + askPromptSection
		}
		if r.sysctx != nil {
			if p := r.sysctx.Preamble(); p != "" {
				r.system = p + "\n\n" + r.system
			}
		}
		if res := r.fire(ctx, "session-start", map[string]any{}, emit); res != nil {
			if c, ok := res["context"].(string); ok && c != "" {
				r.system += "\n\n" + c
			}
		}
	}

	if res := r.fire(ctx, "user-prompt-submit", map[string]any{"input": input}, emit); res != nil {
		if b, ok := res["block"].(string); ok {
			note("error", b, nil)
			note("done", "", nil) // end the turn so headless drain sees it
			return nil
		}
		if in, ok := res["input"].(string); ok {
			input = in
		}
	}

	msg := input
	if r.skills != nil {
		for _, block := range r.skills.Inject(input) {
			msg += "\n\n" + block
		}
	}
	r.hist.Append("input", map[string]any{"text": msg})

	for step := 0; step < maxSteps; step++ {
		sys := r.system
		if r.cog != nil {
			sys = r.cog.System(sys)
		}
		reply, err := r.llm.Complete(ctx, sys, r.project())
		if err != nil {
			note("error", err.Error(), nil)
			note("done", "", nil) // every turn ends with a done, even on llm failure
			return err
		}
		note("assistant", reply, nil)
		blocks := jsBlock.FindAllStringSubmatch(reply, -1)
		if len(blocks) == 0 {
			note("done", "", nil)
			r.fire(ctx, "stop", map[string]any{}, emit)
			return nil
		}
		for _, m := range blocks {
			code := m[1]
			if res := r.fire(ctx, "pre-code-exec", map[string]any{"code": code}, emit); res != nil {
				if reason, ok := res["deny"].(string); ok {
					note("result", "[hook denied: "+reason+"]", map[string]any{"code": code})
					continue
				}
				if c, ok := res["code"].(string); ok {
					code = c
				}
			}
			note("code", code, nil)
			out, runErr := r.code.Run(code)
			if runErr != nil {
				out = "error: " + runErr.Error()
			} else {
				out = truncate(out, maxResultBytes)
			}
			if res := r.fire(ctx, "post-result", map[string]any{"code": code, "result": out}, emit); res != nil {
				if s, ok := res["result"].(string); ok {
					out = s
				}
			}
			// A run error still lands as a "result" entry (text
			// "error: ...") so the projection feeds it back to the
			// model; the UI event keeps the "error" kind.
			r.hist.Append("result", map[string]any{"text": out, "code": code})
			if runErr != nil {
				emit("error", out)
			} else {
				emit("result", out)
			}
		}
	}
	err := fmt.Errorf("loop: gave up after %d steps", maxSteps)
	note("error", err.Error(), nil)
	note("done", "", nil)
	r.fire(ctx, "stop", map[string]any{}, emit)
	return err
}

func truncate(s string, n int) string {
	if len(s) <= n {
		return s
	}
	return s[:n] + "\n[truncated]"
}

type plugin struct{}

func init() {
	kernel.Register("loop", func() kernel.Plugin { return &plugin{} })
}

func (p *plugin) Name() string     { return "loop" }
func (p *plugin) Inject() []string { return []string{"llm", "codemode"} }

func (p *plugin) Apply(kctx *kernel.Context, cfg map[string]any) error {
	llm, err := kernel.Get[LLM](kctx, "llm")
	if err != nil {
		return err
	}
	code, err := kernel.Get[Codemode](kctx, "codemode")
	if err != nil {
		return err
	}
	r := &runner{llm: llm, code: code, hist: &memHistory{}}
	// Optional seams: absent services are a clean no-op / built-in.
	if h, err := kernel.Get[Hooks](kctx, "hooks"); err == nil {
		r.hooks = h
	}
	if s, err := kernel.Get[Skills](kctx, "skills"); err == nil {
		r.skills = s
	}
	if sc, err := kernel.Get[SystemContext](kctx, "context-md"); err == nil {
		r.sysctx = sc
	}
	if h, err := kernel.Get[History](kctx, "history"); err == nil {
		r.hist = h
	}
	if c, err := kernel.Get[Cognition](kctx, "cognition"); err == nil {
		r.cog = c
	}
	if pr, err := kernel.Get[Projection](kctx, "projection"); err == nil {
		r.proj = pr
	}
	if _, err := kernel.Get[any](kctx, "ask-answers"); err == nil {
		r.hasAsk = true
	}
	kctx.Provide("runner", r)

	inputs := make(chan string, 8)
	kctx.Provide("inputs", inputs)

	ctx, cancel := context.WithCancel(context.Background())
	go func() {
		for input := range inputs {
			_ = r.Run(ctx, input, func(kind, text string) {
				kctx.Emit("loop/event", Event{Kind: kind, Text: text})
			})
		}
	}()
	kctx.Effect(func() {
		cancel()
		close(inputs)
	})
	return nil
}
