//go:build !windows

package llm

// llm-control: a deterministic model a test steers turn by turn. Each
// engine request takes the lexically first <name>.json in the control
// dir (renaming it <name>.taken, so the test can see the call is in
// flight) and answers as it says: finish with text, fail, stream slowly,
// call one tool, or hold until <name>.release appears. llm-script's tape is fixed at
// mount; this one is fed while the session runs, which is what cancel,
// steer and "the model is still thinking" tests need. The test side is
// go/tests/model/llm. Config: dir (default ~/.bough/llm-control).

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"time"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/kernel"
)

func init() {
	kernel.Register("llm-control", func() kernel.Plugin { return &controlPlugin{} })
}

type controlPlugin struct{}

func (p *controlPlugin) Name() string     { return "llm-control" }
func (p *controlPlugin) Inject() []string { return nil }

func (p *controlPlugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	dir, _ := cfg["dir"].(string)
	if dir == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return fmt.Errorf("llm-control: no dir configured and no home: %w", err)
		}
		dir = filepath.Join(home, ".bough", "llm-control")
	}
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return fmt.Errorf("llm-control: %w", err)
	}
	ctx.Provide(serviceKey(cfg), &controlLLM{dir: dir})
	return nil
}

type controlLLM struct {
	dir string

	mu    sync.Mutex // serialises taking a turn across session views
	n     int
	usage Usage
}

var _ agentllm.Source = (*controlLLM)(nil)

// Complete does not take a turn, for the reason llm-script's does not:
// title and status jobs fall back to the main llm, and one that took a
// queued turn would leave the session's request answering the wrong one.
func (c *controlLLM) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	return "control", nil
}

func (c *controlLLM) Model() string { return "control" }
func (c *controlLLM) Ready() error  { return nil }

func (c *controlLLM) Usage() Usage {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.usage
}

func (c *controlLLM) AgentAdapter(o agentllm.Options) (agentllm.Adapter, error) {
	return &controlAdapter{c: c, opts: o}, nil
}

type controlTurn struct {
	Mode    string          `json:"mode"`
	Text    string          `json:"text"`
	Error   string          `json:"error"`
	DelayMS int             `json:"delay_ms"`
	Tool    string          `json:"tool"`
	Args    json.RawMessage `json:"args"`
}

// take claims the next queued turn. A name the test is still writing
// ends in .tmp and is skipped, so a half-written file is never read.
func (c *controlLLM) take() (name string, t controlTurn, ok bool, err error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	ents, err := os.ReadDir(c.dir)
	if err != nil {
		return "", t, false, err
	}
	names := []string{}
	for _, e := range ents {
		if n, ok := strings.CutSuffix(e.Name(), ".json"); ok && !e.IsDir() {
			names = append(names, n)
		}
	}
	if len(names) == 0 {
		return "", t, false, nil
	}
	slices.Sort(names)
	name = names[0]
	src := filepath.Join(c.dir, name+".json")
	b, err := os.ReadFile(src)
	if err != nil {
		return "", t, false, err
	}
	if err := json.Unmarshal(b, &t); err != nil {
		return "", t, false, fmt.Errorf("llm-control: %s.json: %w", name, err)
	}
	if err := os.Rename(src, filepath.Join(c.dir, name+".taken")); err != nil {
		return "", t, false, err
	}
	c.n++
	return name, t, true, nil
}

type controlAdapter struct {
	c    *controlLLM
	opts agentllm.Options
}

func (a *controlAdapter) Provider() string { return "control" }
func (a *controlAdapter) Model() string    { return "control" }
func (a *controlAdapter) Close() error     { return nil }

func (a *controlAdapter) Respond(ctx context.Context, r ullm.Request, _ ullm.RequestOptions) (ullm.Response, error) {
	if err := ctx.Err(); err != nil {
		return ullm.Response{}, err
	}
	name, turn, ok, err := a.c.take()
	if err != nil {
		return ullm.Response{}, err
	}
	// An empty queue answers instead of waiting, so a test that queued
	// too few turns fails on its output rather than timing out.
	if !ok {
		return a.reply(ctx, "[llm-control: no turn queued in "+a.c.dir+"]", 0)
	}
	switch turn.Mode {
	case "ok", "":
		return a.reply(ctx, turn.Text, 0)
	case "error":
		return ullm.Response{}, errors.New(turn.Error)
	case "slow":
		return a.reply(ctx, turn.Text, time.Duration(turn.DelayMS)*time.Millisecond)
	case "call":
		return a.call(name, turn)
	case "block":
		release := filepath.Join(a.c.dir, name+".release")
		tick := time.NewTicker(10 * time.Millisecond)
		defer tick.Stop()
		for {
			if b, err := os.ReadFile(release); err == nil {
				// A release that carries a turn answers as it says, so a
				// test can hold a session in "running" and only then
				// choose whether the turn finishes or fails.
				var then controlTurn
				if len(b) > 0 && json.Unmarshal(b, &then) == nil && then.Mode == "error" {
					return ullm.Response{}, errors.New(then.Error)
				}
				if then.Mode == "call" {
					return a.call(name, then)
				}
				if then.Text != "" {
					return a.reply(ctx, then.Text, 0)
				}
				return a.reply(ctx, turn.Text, 0)
			}
			select {
			case <-ctx.Done():
				return ullm.Response{}, ctx.Err()
			case <-tick.C:
			}
		}
	}
	return ullm.Response{}, fmt.Errorf("llm-control: %s.json: unknown mode %q (want ok, error, slow, call or block)", name, turn.Mode)
}

// reply streams text a word at a time, delay apart, so the live
// assistant-delta path runs as it does against a real provider.
func (a *controlAdapter) reply(ctx context.Context, text string, delay time.Duration) (ullm.Response, error) {
	seq := agentllm.SeqOf(ctx)
	for i, w := range controlWords(text) {
		if i > 0 && delay > 0 {
			select {
			case <-ctx.Done():
				return ullm.Response{}, ctx.Err()
			case <-time.After(delay):
			}
		}
		if a.opts.Sink != nil {
			a.opts.Sink(agentllm.Delta{Seq: seq, Attempt: 1, Kind: agentllm.DeltaText, Text: w})
		}
	}
	u := ullm.Usage{InputTokens: 1, OutputTokens: 1}
	a.c.mu.Lock()
	addAgentUsage(&a.c.usage, u)
	n := a.c.n
	a.c.mu.Unlock()
	return ullm.Response{
		ID:     fmt.Sprintf("control-%d", n),
		Stop:   ullm.StopComplete,
		Output: []ullm.Item{{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleAssistant, Text: text}}},
		Usage:  u,
	}, nil
}

// call answers with one tool call, so a test can make the model ask
// (tools.ask, tools.secret) at a moment it picks. The call's result
// goes out on the next request, which takes the next queued turn.
func (a *controlAdapter) call(name string, turn controlTurn) (ullm.Response, error) {
	if turn.Tool == "" {
		return ullm.Response{}, fmt.Errorf("llm-control: %s: a call turn needs a tool", name)
	}
	args := string(turn.Args)
	if args == "" || args == "null" {
		args = "{}"
	}
	u := ullm.Usage{InputTokens: 1, OutputTokens: 1}
	a.c.mu.Lock()
	addAgentUsage(&a.c.usage, u)
	n := a.c.n
	a.c.mu.Unlock()
	return ullm.Response{
		ID:     fmt.Sprintf("control-%d", n),
		Stop:   ullm.StopComplete,
		Output: []ullm.Item{{Type: ullm.ItemToolCall, Data: ullm.ToolCall{CallID: fmt.Sprintf("control-call-%d", n), Name: turn.Tool, Arguments: args}}},
		Usage:  u,
	}, nil
}

func controlWords(s string) []string {
	var out []string
	for s != "" {
		i := strings.IndexAny(s, " \n")
		if i < 0 {
			return append(out, s)
		}
		out = append(out, s[:i+1])
		s = s[i+1:]
	}
	return out
}
