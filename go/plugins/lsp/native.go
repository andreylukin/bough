package lsp

import (
	"context"
	"encoding/json"
	"fmt"

	"github.com/andreylukin/bough/internal/agenttools"
)

type lspArgs struct {
	Op     string `json:"op"`
	Path   string `json:"path"`
	Symbol string `json:"symbol"`
	Line   int    `json:"line"`
	Query  string `json:"query"`
}

// nativeTool is tools.lsp as one native tool: op picks the method, and
// each op reads the arguments its codemode method takes.
func (m *Manager) nativeTool() agenttools.Tool {
	return agenttools.Tool{
		Name: "lsp",
		Description: "Language-server navigation. def, refs, hover (path, symbol, optional line): where a symbol is defined, where it is used, its type and docs. " +
			"outline (path): a file's classes and functions with their line ranges. symbols (query, optional path): project symbols matching query. " +
			"diagnostics (path): the file's type errors. For a named thing in code use these before grep.",
		Schema: agenttools.Object([]string{"op"}, map[string]any{
			"op":     map[string]any{"type": "string", "enum": []any{"def", "refs", "hover", "outline", "symbols", "diagnostics"}},
			"path":   agenttools.Prop("string", "the file"),
			"symbol": agenttools.Prop("string", "def, refs, hover: the name"),
			"line":   agenttools.Prop("integer", "def, refs, hover: the 1-based line it is on, when the name repeats"),
			"query":  agenttools.Prop("string", "symbols: the name to search for"),
		}),
		Detail: func(args json.RawMessage) string {
			var a lspArgs
			_ = json.Unmarshal(args, &a)
			s := a.Op
			for _, p := range []string{a.Path, a.Symbol, a.Query} {
				if p != "" {
					s += " " + p
				}
			}
			return s
		},
		Call: func(_ context.Context, c agenttools.Call) (agenttools.Result, error) {
			var a lspArgs
			if err := agenttools.Decode("lsp", c.Args, &a); err != nil {
				return agenttools.Result{}, err
			}
			var line []any
			if a.Line > 0 {
				line = []any{a.Line}
			}
			var out string
			var err error
			switch a.Op {
			case "def":
				out, err = m.def(a.Path, a.Symbol, line...)
			case "refs":
				out, err = m.refs(a.Path, a.Symbol, line...)
			case "hover":
				out, err = m.hover(a.Path, a.Symbol, line...)
			case "outline":
				out, err = m.outline(a.Path)
			case "symbols":
				var path []string
				if a.Path != "" {
					path = []string{a.Path}
				}
				out, err = m.symbols(a.Query, path...)
			case "diagnostics":
				out, err = m.diagnosticsTool(a.Path)
			default:
				err = fmt.Errorf("lsp: op must be def, refs, hover, outline, symbols or diagnostics, got %q", a.Op)
			}
			if err != nil {
				return agenttools.Result{Text: out, Error: err.Error()}, nil
			}
			return agenttools.Result{Text: out}, nil
		},
	}
}
