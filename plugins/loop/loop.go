// Package loop is the agent loop plugin: it owns the conversation,
// provides the "inputs" channel and the "runner" service, and drives
// the codemode loop (llm -> extract js -> run -> feed back).
package loop

import (
	"context"
	"fmt"
	"regexp"
	"sync"

	"github.com/andreylukin/bough/kernel"
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

// Event is the payload emitted on "loop/event".
// Kind is one of: "assistant", "code", "result", "error", "done".
type Event struct {
	Kind string
	Text string
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

var jsBlock = regexp.MustCompile("(?s)```js\\s*\n(.*?)```")

// runner implements the "runner" service. hooks, skills and sysctx are
// optional seams; nil means no-op.
type runner struct {
	mu      sync.Mutex
	llm     LLM
	code    Codemode
	hooks   Hooks
	skills  Skills
	sysctx  SystemContext
	history []Message
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

func (r *runner) Run(ctx context.Context, input string, emit func(kind, text string)) error {
	r.mu.Lock()
	defer r.mu.Unlock()

	if !r.started {
		r.started = true
		r.system = systemPrompt
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
			emit("error", b)
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

	r.history = append(r.history, Message{Role: "user", Content: msg})
	for step := 0; step < maxSteps; step++ {
		reply, err := r.llm.Complete(ctx, r.system, r.history)
		if err != nil {
			emit("error", err.Error())
			return err
		}
		r.history = append(r.history, Message{Role: "assistant", Content: reply})
		emit("assistant", reply)
		blocks := jsBlock.FindAllStringSubmatch(reply, -1)
		if len(blocks) == 0 {
			emit("done", "")
			r.fire(ctx, "stop", map[string]any{}, emit)
			return nil
		}
		for _, m := range blocks {
			code := m[1]
			if res := r.fire(ctx, "pre-code-exec", map[string]any{"code": code}, emit); res != nil {
				if reason, ok := res["deny"].(string); ok {
					out := "[hook denied: " + reason + "]"
					emit("result", out)
					r.history = append(r.history, Message{Role: "user", Content: "[tool output]\n" + out})
					continue
				}
				if c, ok := res["code"].(string); ok {
					code = c
				}
			}
			emit("code", code)
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
			if runErr != nil {
				emit("error", out)
			} else {
				emit("result", out)
			}
			r.history = append(r.history, Message{Role: "user", Content: "[tool output]\n" + out})
		}
	}
	err := fmt.Errorf("loop: gave up after %d steps", maxSteps)
	emit("error", err.Error())
	emit("done", "")
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
	r := &runner{llm: llm, code: code}
	// Optional seams: absent services are a clean no-op.
	if h, err := kernel.Get[Hooks](kctx, "hooks"); err == nil {
		r.hooks = h
	}
	if s, err := kernel.Get[Skills](kctx, "skills"); err == nil {
		r.skills = s
	}
	if sc, err := kernel.Get[SystemContext](kctx, "context-md"); err == nil {
		r.sysctx = sc
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
