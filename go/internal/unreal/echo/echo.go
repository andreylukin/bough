// Package echo is the deterministic adapter behind llm-echo on the
// engine: no network, no key, and a few sentinels that make it call
// tools, so the engine's whole loop (calls, results, asks, spawns) can
// be driven from a shell pipe. It streams every reply word by word, so
// the live UI paths run too.
package echo

import (
	"context"
	"fmt"
	"strings"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
)

// Adapter is stateless apart from its sink.
type Adapter struct{ opts agentllm.Options }

var _ agentllm.Adapter = (*Adapter)(nil)

// New returns the echo adapter; the options, when given, carry the sink.
func New(o ...agentllm.Options) *Adapter {
	a := &Adapter{}
	if len(o) > 0 {
		a.opts = o[0]
	}
	return a
}

func (a *Adapter) Provider() string { return "echo" }
func (a *Adapter) Model() string    { return "echo" }
func (a *Adapter) Close() error     { return nil }

// Respond applies the rules in order; the first that matches answers.
func (a *Adapter) Respond(ctx context.Context, r ullm.Request, _ ullm.RequestOptions) (ullm.Response, error) {
	if err := ctx.Err(); err != nil {
		return ullm.Response{}, err
	}
	out := reply(r)
	seq := agentllm.SeqOf(ctx)
	if sink := a.opts.Sink; sink != nil {
		for _, it := range out {
			switch d := it.Data.(type) {
			case ullm.Message:
				for _, w := range words(d.Text) {
					sink(agentllm.Delta{Seq: seq, Attempt: 1, Kind: agentllm.DeltaText, Text: w})
				}
			case ullm.ToolCall:
				sink(agentllm.Delta{Seq: seq, Attempt: 1, Kind: agentllm.DeltaToolStart, CallID: d.CallID, Name: d.Name})
			}
		}
	}
	return ullm.Response{
		ID:     fmt.Sprintf("echo-%d", len(r.Input)),
		Stop:   ullm.StopComplete,
		Output: out,
	}, nil
}

func reply(r ullm.Request) []ullm.Item {
	last := lastUserText(r.Input)
	call := func(name, args string) []ullm.Item {
		return []ullm.Item{{Type: ullm.ItemToolCall, Data: ullm.ToolCall{
			CallID: fmt.Sprintf("echo_%d", len(r.Input)), Name: name, Arguments: args,
		}}}
	}
	switch {
	case strings.Contains(last, "SYSTEM!"):
		// For reading what the model is actually told: the prompt is
		// assembled from parts, so reading the source is not the same
		// as reading the prompt.
		return text(systemText(r.Input))
	case len(r.Input) > 0 && r.Input[len(r.Input)-1].Type == ullm.ItemToolResult:
		res := r.Input[len(r.Input)-1].Data.(ullm.ToolResult)
		return text("ran: " + firstLine(res))
	case strings.Contains(last, "CODE!"):
		return call("bash", `{"command":"echo hi from codemode"}`)
	case strings.Contains(last, "SLOW!"):
		return call("bash", `{"command":"sleep 3; echo slow done"}`)
	case strings.Contains(last, "ASK!"):
		return call("ask", `{"question":"echo asks?","options":["yes","no"]}`)
	case strings.Contains(last, "SPAWN!"):
		return call("spawn", `{"task":"say hi"}`)
	}
	return text("echo: " + last)
}

func text(s string) []ullm.Item {
	return []ullm.Item{{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleAssistant, Text: s}}}
}

func lastUserText(in []ullm.Item) string {
	for i := len(in) - 1; i >= 0; i-- {
		if m, ok := in[i].Data.(ullm.Message); ok && m.Role == ullm.RoleUser {
			return m.Text
		}
	}
	return ""
}

func systemText(in []ullm.Item) string {
	if len(in) == 0 {
		return ""
	}
	if m, ok := in[0].Data.(ullm.Message); ok {
		return m.Text
	}
	return ""
}

func firstLine(res ullm.ToolResult) string {
	for _, o := range res.Output {
		if o.Kind != ullm.ToolResultText {
			continue
		}
		line, _, _ := strings.Cut(strings.TrimSpace(o.Value), "\n")
		return line
	}
	return ""
}

func words(s string) []string {
	var out []string
	for s != "" {
		i := strings.IndexAny(s, " \n")
		if i < 0 {
			out = append(out, s)
			break
		}
		out = append(out, s[:i+1])
		s = s[i+1:]
	}
	return out
}
