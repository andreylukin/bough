package mcp

import (
	"context"
	"encoding/json"
	"fmt"
	"slices"
	"strings"
	"sync"

	sdk "github.com/modelcontextprotocol/go-sdk/mcp"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/kernel"
)

// nativeServers reads config.native_tools: the servers whose tools the
// engine calls directly instead of through `bough mcp call`. Opt-in,
// because each one changes the native tool set when it connects, and a
// changed tool set restarts the engine's coordinator.
func nativeServers(cfg map[string]any) ([]string, error) {
	v, ok := cfg["native_tools"]
	if !ok || v == nil {
		return nil, nil
	}
	list, ok := v.([]any)
	if !ok {
		return nil, fmt.Errorf("mcp: native_tools must be a list of server names, got %T", v)
	}
	var out []string
	for _, e := range list {
		s, ok := e.(string)
		if !ok || s == "" {
			return nil, fmt.Errorf("mcp: native_tools entries must be server names, got %v", e)
		}
		out = append(out, s)
	}
	return out, nil
}

// native owns one row's MCP connections and the tools registered for
// them. A session is opened once per server and reused by every call:
// a stdio server is a process, and spawning one per call is what the
// CLI path costs and the native path exists to avoid.
type native struct {
	reg     agenttools.Registry
	servers map[string]ServerConfig
	connect func(ServerConfig) (*sdk.ClientSession, error)

	mu       sync.Mutex
	sessions map[string]*sdk.ClientSession
	unreg    []func()
	closed   bool
}

func newNative(reg agenttools.Registry, servers map[string]ServerConfig) *native {
	return &native{reg: reg, servers: servers, connect: connect, sessions: map[string]*sdk.ClientSession{}}
}

// start connects to each listed server and registers its tools. It runs
// off the mount: a server can take seconds to answer, and the tools
// appearing late is the same event as a server connecting later.
func (n *native) start(names []string) {
	taken := map[string]bool{}
	for _, name := range names {
		sc, ok := n.servers[name]
		if !ok {
			kernel.Logf("mcp: native_tools: no server %q configured (bough mcp list)\n", name)
			continue
		}
		if sc.Disabled {
			continue
		}
		s, err := n.session(name)
		if err != nil {
			kernel.Logf("mcp: native_tools: %s: %v\n", name, err)
			continue
		}
		ctx, cancel := context.WithTimeout(context.Background(), connectTimeout)
		var tools []*sdk.Tool
		for t, err := range s.Tools(ctx, nil) {
			if err != nil {
				kernel.Logf("mcp: native_tools: %s: list tools: %v\n", name, err)
				break
			}
			tools = append(tools, t)
		}
		cancel()
		for _, t := range tools {
			n.register(name, t, taken)
		}
	}
}

func (n *native) register(server string, t *sdk.Tool, taken map[string]bool) {
	name := nativeName(server, t.Name, func(s string) bool {
		if taken[s] {
			return true
		}
		_, ok := n.reg.Lookup(s)
		return ok
	})
	taken[name] = true
	remote := t.Name
	unreg, err := n.reg.Register(agenttools.Tool{
		Name:        name,
		Description: strings.TrimSpace(t.Description),
		Schema:      schemaOf(t.InputSchema),
		Detail: func(args json.RawMessage) string {
			return strings.TrimSpace(server + "/" + remote + " " + detailOf(args))
		},
		Call: func(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
			return n.call(ctx, server, remote, c.Args)
		},
	})
	if err != nil {
		kernel.Logf("mcp: native_tools: %v\n", err)
		return
	}
	n.mu.Lock()
	defer n.mu.Unlock()
	if n.closed {
		unreg()
		return
	}
	n.unreg = append(n.unreg, unreg)
}

// session is the server's open session, connecting on first use. The
// connect runs unlocked: it can take seconds, and neither a call to
// another server nor the row's unmount should wait on it.
func (n *native) session(server string) (*sdk.ClientSession, error) {
	n.mu.Lock()
	s, closed := n.sessions[server], n.closed
	n.mu.Unlock()
	if closed {
		return nil, fmt.Errorf("the mcp row was unmounted")
	}
	if s != nil {
		return s, nil
	}
	s, err := n.connect(n.servers[server])
	if err != nil {
		return nil, fmt.Errorf("connect: %w", err)
	}
	n.mu.Lock()
	defer n.mu.Unlock()
	if n.closed {
		s.Close()
		return nil, fmt.Errorf("the mcp row was unmounted")
	}
	if had := n.sessions[server]; had != nil {
		s.Close() // a concurrent call connected first; keep one session
		return had, nil
	}
	n.sessions[server] = s
	return s, nil
}

// forget drops a session a call failed on, so the next call reconnects:
// a stdio server that exited must not fail every later call.
func (n *native) forget(server string, s *sdk.ClientSession) {
	n.mu.Lock()
	defer n.mu.Unlock()
	if n.sessions[server] == s {
		delete(n.sessions, server)
		s.Close()
	}
}

func (n *native) call(ctx context.Context, server, tool string, args json.RawMessage) (agenttools.Result, error) {
	s, err := n.session(server)
	if err != nil {
		return agenttools.Result{}, fmt.Errorf("mcp: %s: %w", server, err)
	}
	if len(args) == 0 {
		args = json.RawMessage("{}")
	}
	res, err := s.CallTool(ctx, &sdk.CallToolParams{Name: tool, Arguments: args})
	if err != nil {
		if ctx.Err() == nil {
			n.forget(server, s)
		}
		return agenttools.Result{}, fmt.Errorf("mcp: %s/%s: %w", server, tool, err)
	}
	var b strings.Builder
	for _, c := range res.Content {
		if tc, ok := c.(*sdk.TextContent); ok {
			b.WriteString(tc.Text)
		}
	}
	if res.IsError {
		return agenttools.Result{Text: b.String(), Error: fmt.Sprintf("mcp: %s/%s failed", server, tool)}, nil
	}
	return agenttools.Result{Text: b.String()}, nil
}

// close unregisters the tools and closes every session.
func (n *native) close() {
	n.mu.Lock()
	n.closed = true
	unreg, sessions := n.unreg, n.sessions
	n.unreg, n.sessions = nil, map[string]*sdk.ClientSession{}
	n.mu.Unlock()
	for _, u := range unreg {
		u()
	}
	for _, s := range sessions {
		s.Close()
	}
}

// nativeName is mcp__<server>__<tool> in the bytes every provider
// accepts: anything outside [a-zA-Z0-9_-] becomes _, the name is cut to
// 64 bytes, and a clash takes the first free _2, _3, … suffix.
func nativeName(server, tool string, taken func(string) bool) string {
	clean := func(s string) string {
		b := []byte(s)
		for i, c := range b {
			if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '_' || c == '-') {
				b[i] = '_'
			}
		}
		return string(b)
	}
	base := "mcp__" + clean(server) + "__" + clean(tool)
	if len(base) > 64 {
		base = base[:64]
	}
	name := base
	for i := 2; taken(name); i++ {
		suffix := fmt.Sprintf("_%d", i)
		name = base[:min(len(base), 64-len(suffix))] + suffix
	}
	return name
}

// schemaOf is the server's input schema as the object the providers
// take; a server that sends none, or not an object schema, gets an
// open object so the call is still offered.
func schemaOf(s any) map[string]any {
	var m map[string]any
	if raw, err := json.Marshal(s); err == nil {
		_ = json.Unmarshal(raw, &m)
	}
	if m == nil || m["type"] != "object" {
		return map[string]any{"type": "object"}
	}
	return m
}

// detailOf is the call row's text: the arguments, compact and short.
func detailOf(args json.RawMessage) string {
	var v any
	if json.Unmarshal(args, &v) != nil {
		return ""
	}
	b, _ := json.Marshal(v)
	s := string(b)
	if s == "{}" || s == "null" {
		return ""
	}
	if r := []rune(s); len(r) > 120 {
		s = string(r[:119]) + "…"
	}
	return s
}

// startNative registers native tools for the servers config.native_tools
// names, when it names any. Nothing is read or connected otherwise, so
// a session without the key mounts exactly as before.
func startNative(ctx *kernel.Context, cfg map[string]any, servers map[string]ServerConfig) error {
	names, err := nativeServers(cfg)
	if err != nil || len(names) == 0 {
		return err
	}
	reg, err := kernel.Get[agenttools.Registry](ctx, "agent-tools")
	if err != nil {
		return fmt.Errorf("mcp: native_tools needs the agent-tools row: %w", err)
	}
	n := newNative(reg, servers)
	ctx.Effect(n.close)
	go n.start(slices.Clone(names))
	return nil
}
