package mcp

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/kernel"
)

// The engine's view of the host: three native tools that never change
// while a session runs. Per-tool native registrations (native_tools)
// remain the opt-in for servers whose tools should be first-class; the
// trio is what keeps the tool set, and so the prompt cache, stable
// while the model explores.

func registerTrio(ctx *kernel.Context, h *Host) {
	reg, err := kernel.Get[agenttools.Registry](ctx, "agent-tools")
	if err != nil {
		return
	}
	str := func(m map[string]any, k string) string { s, _ := m[k].(string); return s }
	parse := func(raw json.RawMessage) map[string]any {
		var m map[string]any
		_ = json.Unmarshal(raw, &m)
		if m == nil {
			m = map[string]any{}
		}
		return m
	}
	asJSON := func(v any) agenttools.Result {
		b, _ := json.MarshalIndent(v, "", " ")
		return agenttools.Result{Text: string(b)}
	}
	tools := []agenttools.Tool{{
		Name:        "mcp_search",
		Description: "Find MCP tools by words in their name, description or parameters. Returns ranked {server, tool, signature, description}; call one with mcp_call. Servers: " + strings.Join(h.Servers(), ", "),
		Schema: agenttools.Object([]string{"query"}, map[string]any{
			"query": agenttools.Prop("string", "words to look for"),
			"limit": agenttools.Prop("integer", "how many results (default 8)"),
		}),
		Detail: func(raw json.RawMessage) string { return str(parse(raw), "query") },
		Call: func(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
			a := parse(c.Args)
			limit, _ := a["limit"].(float64)
			hits := h.Search(str(a, "query"), int(limit))
			if len(hits) == 0 {
				return agenttools.Result{Text: "no tool matches; mcp_describe a server's tool by name, or search other words"}, nil
			}
			return asJSON(hits), nil
		},
	}, {
		Name:        "mcp_describe",
		Description: "An MCP tool in full: its description, input schema and output schema, before calling it with mcp_call.",
		Schema: agenttools.Object([]string{"server", "tool"}, map[string]any{
			"server": agenttools.Prop("string", "server name"),
			"tool":   agenttools.Prop("string", "tool name"),
		}),
		Detail: func(raw json.RawMessage) string { a := parse(raw); return str(a, "server") + "/" + str(a, "tool") },
		Call: func(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
			a := parse(c.Args)
			d, err := h.Describe(str(a, "server"), str(a, "tool"))
			if err != nil {
				return agenttools.Result{Error: err.Error()}, nil
			}
			return asJSON(d), nil
		},
	}, {
		Name:        "mcp_call",
		Description: "Call one MCP tool. Returns {ok, value, text, content, isError, error}: value is the server's structured result; ok false with error.message when the tool itself failed.",
		Schema: agenttools.Object([]string{"server", "tool"}, map[string]any{
			"server": agenttools.Prop("string", "server name"),
			"tool":   agenttools.Prop("string", "tool name"),
			"args":   map[string]any{"type": "object", "description": "the tool's arguments, per its input schema"},
		}),
		Detail: func(raw json.RawMessage) string {
			a := parse(raw)
			return strings.TrimSpace(str(a, "server") + "/" + str(a, "tool") + " " + detailOf(mustJSON(a["args"])))
		},
		Call: func(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
			a := parse(c.Args)
			args, _ := a["args"].(map[string]any)
			e, err := h.Call(ctx, str(a, "server"), str(a, "tool"), args)
			if err != nil {
				return agenttools.Result{Error: err.Error()}, nil
			}
			r := asJSON(e)
			if e.IsError {
				r.Error = fmt.Sprintf("mcp: %s/%s failed: %s", str(a, "server"), str(a, "tool"), e.Error.Message)
			}
			return r, nil
		},
	}}
	for _, t := range tools {
		unreg, err := reg.Register(t)
		if err != nil {
			kernel.Logf("mcp: %v\n", err)
			continue
		}
		ctx.Effect(unreg)
	}
}

func mustJSON(v any) json.RawMessage {
	if v == nil {
		return nil
	}
	b, _ := json.Marshal(v)
	return b
}
