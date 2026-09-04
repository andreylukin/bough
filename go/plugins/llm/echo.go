package llm

import (
	"context"
	"strings"

	"github.com/andreylukin/bough/kernel"
)

func init() {
	kernel.Register("llm-echo", func() kernel.Plugin { return &echoPlugin{} })
}

type echoPlugin struct{}

func (p *echoPlugin) Name() string     { return "llm-echo" }
func (p *echoPlugin) Inject() []string { return nil }

func (p *echoPlugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	ctx.Provide("llm", echoLLM{})
	return nil
}

type echoLLM struct{}

func (echoLLM) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	last := ""
	for _, m := range messages {
		if m.Role == "user" {
			last = m.Content
		}
	}
	// A sentinel like CODE!, for inspecting what the model is actually
	// told: the assembled prompt is built from the base, the live
	// sections and the tool catalogue, so reading the source is not
	// the same as reading the prompt.
	if strings.Contains(last, "SYSTEM!") {
		return system, nil
	}
	if strings.Contains(last, "CODE!") {
		return "```js\ntools.bash(\"echo hi from codemode\")\n```", nil
	}
	return "echo: " + last, nil
}

// Stream implements Streamer for tests: the reply arrives one word at a
// time (whitespace kept), so the ui's live block is exercised without a
// network.
func (e echoLLM) Stream(ctx context.Context, system string, messages []Message, onDelta func(string)) (string, error) {
	reply, err := e.Complete(ctx, system, messages)
	if err != nil {
		return "", err
	}
	rest := reply
	for rest != "" {
		i := strings.IndexAny(rest, " \n")
		if i < 0 {
			i = len(rest) - 1
		}
		onDelta(rest[:i+1])
		rest = rest[i+1:]
	}
	return reply, nil
}
