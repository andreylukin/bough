//go:build !windows

package llm

// llm-script: a deterministic model read from a JSON tape
// (internal/unreal/fake), for headless e2e runs and the browser spec on
// the engine. Config: script (required, a path). One tape per row: the
// main session and its subagents consume it in request order.

import (
	"context"
	"fmt"
	"strings"
	"sync"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/unreal/fake"
	"github.com/andreylukin/bough/internal/unreal/wrap"
	"github.com/andreylukin/bough/kernel"
)

func init() {
	kernel.Register("llm-script", func() kernel.Plugin { return &scriptPlugin{} })
}

type scriptPlugin struct{}

func (p *scriptPlugin) Name() string     { return "llm-script" }
func (p *scriptPlugin) Inject() []string { return nil }

func (p *scriptPlugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	path, ok := cfg["script"].(string)
	if !ok || path == "" {
		return fmt.Errorf("llm-script: config needs script (a path to a JSON tape)")
	}
	steps, err := fake.Load(path)
	if err != nil {
		return fmt.Errorf("llm-script: %w", err)
	}
	ctx.Provide(serviceKey(cfg), &scriptLLM{tape: fake.New(nil, steps...)})
	return nil
}

type scriptLLM struct {
	tape *fake.Adapter

	mu    sync.Mutex
	usage Usage
}

var _ agentllm.Source = (*scriptLLM)(nil)

// Complete does not touch the tape. The title and status jobs fall back
// to the main llm when there is no llm-small row, and a script that
// lost a step to a session title would answer the wrong request.
func (s *scriptLLM) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	last := ""
	for _, m := range messages {
		if m.Role == "user" {
			last = m.Content
		}
	}
	line, _, _ := strings.Cut(strings.TrimSpace(last), "\n")
	return "script: " + line, nil
}

// Model implements Modeler.
func (s *scriptLLM) Model() string { return "script" }

// Ready implements Ready: the tape loaded at mount.
func (s *scriptLLM) Ready() error { return nil }

// Usage implements UsageReporter with the tape's recorded counts.
func (s *scriptLLM) Usage() Usage {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.usage
}

// AgentAdapter implements agentllm.Source: a view on the shared tape.
func (s *scriptLLM) AgentAdapter(o agentllm.Options) (agentllm.Adapter, error) {
	return wrap.Observe(wrap.Envelope(s.tape.View(o)), func(r ullm.Response) {
		s.mu.Lock()
		addAgentUsage(&s.usage, r.Usage)
		s.mu.Unlock()
	}), nil
}
