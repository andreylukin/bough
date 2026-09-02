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

// runner implements the "runner" service.
type runner struct {
	mu      sync.Mutex
	llm     LLM
	code    Codemode
	history []Message
}

func (r *runner) Run(ctx context.Context, input string, emit func(kind, text string)) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.history = append(r.history, Message{Role: "user", Content: input})
	for step := 0; step < maxSteps; step++ {
		reply, err := r.llm.Complete(ctx, systemPrompt, r.history)
		if err != nil {
			emit("error", err.Error())
			return err
		}
		r.history = append(r.history, Message{Role: "assistant", Content: reply})
		emit("assistant", reply)
		blocks := jsBlock.FindAllStringSubmatch(reply, -1)
		if len(blocks) == 0 {
			emit("done", "")
			return nil
		}
		for _, m := range blocks {
			code := m[1]
			emit("code", code)
			out, err := r.code.Run(code)
			if err != nil {
				out = "error: " + err.Error()
				emit("error", out)
			} else {
				out = truncate(out, maxResultBytes)
				emit("result", out)
			}
			r.history = append(r.history, Message{Role: "user", Content: "[tool output]\n" + out})
		}
	}
	err := fmt.Errorf("loop: gave up after %d steps", maxSteps)
	emit("error", err.Error())
	emit("done", "")
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
