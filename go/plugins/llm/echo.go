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
	if strings.Contains(last, "CODE!") {
		return "```js\ntools.bash(\"echo hi from codemode\")\n```", nil
	}
	return "echo: " + last, nil
}
