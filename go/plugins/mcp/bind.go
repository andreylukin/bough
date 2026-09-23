package mcp

import (
	"context"
	"fmt"
	"maps"
	"slices"
	"strings"

	"github.com/dop251/goja"

	"github.com/andreylukin/bough/kernel"
)

// Code mode's tools.mcp: search, describe and call as host functions,
// plus one stub per known tool (tools.mcp.<server>.<tool>(args)) so a
// call reads the way the signature in a search result reads. Stubs
// come from the catalog at mount and are added live as the host learns
// tools, so a fresh install still gets them after its first search.

// programmatic reads config.programmatic: false keeps the shell-only
// surface (the prompt names bough mcp call) for an A/B.
func programmatic(cfg map[string]any) bool {
	v, ok := cfg["programmatic"]
	if !ok {
		return true
	}
	b, ok := v.(bool)
	return !ok || b
}

// vmAccess is the slice of codemode bindCodeMode needs.
type vmAccess interface {
	WithVM(fn func(vm *goja.Runtime, tools *goja.Object) error) error
	RunContext() context.Context
}

type describer interface{ Describe(name, line string) }

func bindCodeMode(ctx *kernel.Context, h *Host) {
	cm, err := kernel.Get[vmAccess](ctx, "codemode")
	if err != nil {
		return // engine session: the trio serves it
	}
	_ = cm.WithVM(func(vm *goja.Runtime, tools *goja.Object) error {
		mcp := vm.NewObject()
		// Values cross into JS by their JSON names (value, ok, …), not Go
		// field names: plain() round-trips them.
		_ = mcp.Set("search", func(query string, limit int) (any, error) {
			hits := h.Search(query, limit)
			if hits == nil {
				hits = []Hit{}
			}
			return plain(hits), nil
		})
		_ = mcp.Set("describe", func(server, tool string) (any, error) {
			d, err := h.Describe(server, tool)
			if err != nil {
				return nil, err
			}
			return plain(d), nil
		})
		_ = mcp.Set("call", func(server, tool string, args map[string]any) (any, error) {
			e, err := h.Call(cm.RunContext(), server, tool, args)
			if err != nil {
				return nil, err
			}
			return plain(e), nil
		})
		_ = mcp.Set("servers", func() []string { return h.Servers() })
		for server, ts := range h.Catalog() {
			for _, t := range ts {
				addStub(vm, mcp, cm, h, server, t.Name)
			}
		}
		return tools.Set("mcp", mcp)
	})
	h.mu.Lock()
	h.onBind = func(server, tool string) {
		_ = cm.WithVM(func(vm *goja.Runtime, tools *goja.Object) error {
			mcp, ok := tools.Get("mcp").(*goja.Object)
			if !ok {
				return nil
			}
			addStub(vm, mcp, cm, h, server, tool)
			return nil
		})
	}
	h.mu.Unlock()
	if d, err := kernel.Get[describer](ctx, "codemode"); err == nil {
		d.Describe("mcp", "tools.mcp.search(words[, limit]) → [{server, tool, signature, description}]; "+
			"tools.mcp.describe(server, tool) → {jsdoc, inputSchema, outputSchema}; "+
			"tools.mcp.<server>.<tool>({…}) / tools.mcp.call(server, tool, args) → {ok, value, text, content, isError, error} — MCP servers: "+
			strings.Join(h.Servers(), ", "))
	}
}

// addStub sets tools.mcp.<server>.<tool> (and the raw names as
// properties, for a name that is not an identifier).
func addStub(vm *goja.Runtime, mcp *goja.Object, cm vmAccess, h *Host, server, tool string) {
	so, ok := mcp.Get(ident(server)).(*goja.Object)
	if !ok || so == nil {
		so = vm.NewObject()
		_ = mcp.Set(ident(server), so)
		if ident(server) != server {
			_ = mcp.Set(server, so)
		}
	}
	fn := func(args map[string]any) (any, error) {
		e, err := h.Call(cm.RunContext(), server, tool, args)
		if err != nil {
			return nil, err
		}
		return plain(e), nil
	}
	_ = so.Set(ident(tool), fn)
	if ident(tool) != tool {
		_ = so.Set(tool, fn)
	}
}

// shellPromptSection is the surface before tools.mcp: the CLI over
// bash, kept behind programmatic: false.
func shellPromptSection(servers map[string]ServerConfig, cat catalog) string {
	if len(servers) == 0 {
		return ""
	}
	names := slices.Sorted(maps.Keys(servers))
	var b strings.Builder
	b.WriteString("MCP servers are reachable from the shell, not as tools: " +
		"tools.bash(\"bough mcp call <server/tool> '<json args or plain text>'\") runs one " +
		"(plain text binds to the tool's first required argument); bough mcp search <query> finds a tool, bough mcp tools [server] refreshes this catalog.\n")
	for _, n := range names {
		tools := cat.Servers[n]
		if len(tools) == 0 {
			fmt.Fprintf(&b, "- %s: tools not listed yet, run bough mcp tools %s\n", n, n)
			continue
		}
		fmt.Fprintf(&b, "- %s (%d tools):\n", n, len(tools))
		for _, t := range tools {
			fmt.Fprintf(&b, "  %s/%s  %s\n", n, t.Name, t.Desc)
		}
	}
	return strings.TrimRight(b.String(), "\n")
}
