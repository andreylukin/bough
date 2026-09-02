// Package mcp is the "mcp" plugin: MCP servers as a CLI, not as
// model tools. `bough mcp list` shows every configured server's tools;
// `bough mcp call <server/tool> [args]` runs one and prints its text.
// The model reaches them through the shell, so nothing is injected
// into its tool surface or prompt beyond a one-line pointer to the
// CLI (only when servers are configured).
//
// Config sources, merged by server name (highest precedence first):
// row config (config.servers / config.disable) > ./.mcp.json mcpServers
// > ~/.claude.json mcpServers. Only stdio servers (command present) are
// used; url/http entries are skipped with a log line.
package mcp

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"time"

	sdk "github.com/modelcontextprotocol/go-sdk/mcp"

	"github.com/andreylukin/bough/kernel"
)

const (
	connectTimeout = 10 * time.Second
	callTimeout    = 60 * time.Second
)

// ServerConfig is one stdio MCP server.
type ServerConfig struct {
	Command string
	Args    []string
	Env     map[string]string
}

// sections is the slice of the loop's "prompt-sections" service we need.
type sections interface {
	Set(name, text string)
}

// promptSection is the one line the model gets: how to reach MCP from
// the shell. Empty when no server is configured.
func promptSection(servers map[string]ServerConfig) string {
	if len(servers) == 0 {
		return ""
	}
	names := make([]string, 0, len(servers))
	for n := range servers {
		names = append(names, n)
	}
	sort.Strings(names)
	return "MCP servers (" + strings.Join(names, ", ") + ") are reachable from the shell, not as tools: " +
		"tools.bash(\"bough mcp list\") shows their tools; tools.bash(\"bough mcp call <server/tool> '<json args or plain text>'\") runs one."
}

type plugin struct{}

func init() {
	kernel.Register("mcp", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "mcp" }
func (plugin) Inject() []string { return nil }

// Apply only documents the CLI to the model; servers are spawned on
// demand by the subcommands, never at mount.
func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	servers, err := configuredServers(cfg)
	if err != nil {
		return err
	}
	if s, err := kernel.Get[sections](ctx, "prompt-sections"); err == nil {
		s.Set("mcp", promptSection(servers))
		ctx.Effect(func() { s.Set("mcp", "") })
	}
	return nil
}

// configuredServers merges every config source for the given row config.
func configuredServers(cfg map[string]any) (map[string]ServerConfig, error) {
	row, err := rowServers(cfg)
	if err != nil {
		return nil, err
	}
	var global map[string]ServerConfig
	if home, err := os.UserHomeDir(); err == nil {
		global = loadServersFile(filepath.Join(home, ".claude.json"))
	}
	project := loadServersFile(".mcp.json")
	return merge(disableList(cfg), global, project, row), nil
}

// Commands implements kernel.Commander: `bough mcp list|call`.
func (plugin) Commands() []kernel.Command {
	return []kernel.Command{{
		Name:    "mcp",
		Usage:   "list | call <server/tool> [json-args|text]",
		Summary: "list configured MCP servers' tools, or call one",
		Run:     runCLI,
	}}
}

func runCLI(cfg map[string]any, args []string) error {
	if len(args) == 0 {
		return fmt.Errorf("usage: bough mcp list | call <server/tool> [json-args|text]")
	}
	servers, err := configuredServers(cfg)
	if err != nil {
		return err
	}
	switch args[0] {
	case "list":
		if len(servers) == 0 {
			return fmt.Errorf("no MCP servers configured (row config, ./.mcp.json, ~/.claude.json)")
		}
		names := make([]string, 0, len(servers))
		for n := range servers {
			names = append(names, n)
		}
		sort.Strings(names)
		for _, n := range names {
			session, err := connect(servers[n])
			if err != nil {
				fmt.Fprintf(os.Stderr, "mcp: %s: connect: %v\n", n, err)
				continue
			}
			lines, err := listLines(n, session)
			session.Close()
			if err != nil {
				fmt.Fprintf(os.Stderr, "mcp: %s: list tools: %v\n", n, err)
				continue
			}
			for _, l := range lines {
				fmt.Println(l)
			}
		}
		return nil
	case "call":
		if len(args) < 2 {
			return fmt.Errorf("usage: bough mcp call <server/tool> [json-args|text]")
		}
		server, tool, _ := strings.Cut(args[1], "/")
		if tool == "" {
			return fmt.Errorf("name %q must be <server>/<tool> (see bough mcp list)", args[1])
		}
		sc, ok := servers[server]
		if !ok {
			return fmt.Errorf("no MCP server %q configured", server)
		}
		session, err := connect(sc)
		if err != nil {
			return fmt.Errorf("%s: connect: %w", server, err)
		}
		defer session.Close()
		out, err := callOn(session, tool, strings.Join(args[2:], " "))
		if err != nil {
			return err
		}
		fmt.Println(out)
		return nil
	}
	return fmt.Errorf("unknown mcp command %q (list | call)", args[0])
}

// merge combines server maps lowest-precedence FIRST (later layers win
// by name), then removes names in disable. Pure.
func merge(disable []string, layers ...map[string]ServerConfig) map[string]ServerConfig {
	out := map[string]ServerConfig{}
	for _, layer := range layers {
		for name, sc := range layer {
			out[name] = sc
		}
	}
	for _, name := range disable {
		delete(out, name)
	}
	return out
}

// rowServers parses config.servers from the bough.yml row. Malformed
// row config fails the mount (it is our own file).
func rowServers(cfg map[string]any) (map[string]ServerConfig, error) {
	if cfg["servers"] == nil {
		return nil, nil
	}
	raw, ok := cfg["servers"].(map[string]any)
	if !ok {
		return nil, fmt.Errorf("mcp: config.servers is %T, want a map", cfg["servers"])
	}
	out := map[string]ServerConfig{}
	for name, v := range raw {
		m, ok := v.(map[string]any)
		if !ok {
			return nil, fmt.Errorf("mcp: config.servers.%s is %T, want a map", name, v)
		}
		sc, ok := parseEntry(m)
		if !ok {
			return nil, fmt.Errorf("mcp: config.servers.%s: command is required", name)
		}
		out[name] = sc
	}
	return out, nil
}

func disableList(cfg map[string]any) []string {
	raw, _ := cfg["disable"].([]any)
	var out []string
	for _, v := range raw {
		if s, ok := v.(string); ok {
			out = append(out, s)
		}
	}
	return out
}

// loadServersFile reads a Claude Code style JSON file with a top-level
// "mcpServers" key. Missing file is fine; anything else wrong is logged
// and skipped (these files are not ours to fail the mount over).
func loadServersFile(path string) map[string]ServerConfig {
	data, err := os.ReadFile(path)
	if err != nil {
		if !os.IsNotExist(err) {
			fmt.Fprintf(os.Stderr, "mcp: %s: %v (skipped)\n", path, err)
		}
		return nil
	}
	var doc struct {
		MCPServers map[string]map[string]any `json:"mcpServers"`
	}
	if err := json.Unmarshal(data, &doc); err != nil {
		fmt.Fprintf(os.Stderr, "mcp: %s: %v (skipped)\n", path, err)
		return nil
	}
	out := map[string]ServerConfig{}
	for name, m := range doc.MCPServers {
		sc, ok := parseEntry(m)
		if !ok {
			fmt.Fprintf(os.Stderr, "mcp: %s: server %q is not stdio (no command), skipped\n", path, name)
			continue
		}
		out[name] = sc
	}
	return out
}

// parseEntry converts one raw server entry. ok=false when there is no
// command (url/http servers).
func parseEntry(m map[string]any) (ServerConfig, bool) {
	cmd, _ := m["command"].(string)
	if cmd == "" {
		return ServerConfig{}, false
	}
	sc := ServerConfig{Command: cmd}
	if args, ok := m["args"].([]any); ok {
		for _, a := range args {
			if s, ok := a.(string); ok {
				sc.Args = append(sc.Args, s)
			}
		}
	}
	if env, ok := m["env"].(map[string]any); ok {
		sc.Env = map[string]string{}
		for k, v := range env {
			if s, ok := v.(string); ok {
				sc.Env[k] = s
			}
		}
	}
	return sc, true
}

// connect spawns the server process and initializes an MCP session.
func connect(sc ServerConfig) (*sdk.ClientSession, error) {
	ctx, cancel := context.WithTimeout(context.Background(), connectTimeout)
	defer cancel()
	cmd := exec.Command(sc.Command, sc.Args...)
	cmd.Env = os.Environ()
	for k, v := range sc.Env {
		cmd.Env = append(cmd.Env, k+"="+v)
	}
	client := sdk.NewClient(&sdk.Implementation{Name: "bough", Version: "0.1"}, nil)
	return client.Connect(ctx, &sdk.CommandTransport{Command: cmd}, nil)
}

// listLines renders one "server/tool  description" line per tool.
func listLines(server string, session *sdk.ClientSession) ([]string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), connectTimeout)
	defer cancel()
	var lines []string
	for tool, err := range session.Tools(ctx, nil) {
		if err != nil {
			return lines, err
		}
		desc := strings.SplitN(strings.TrimSpace(tool.Description), "\n", 2)[0]
		lines = append(lines, fmt.Sprintf("%s/%s  %s", server, tool.Name, desc))
	}
	return lines, nil
}

// callOn runs one tool: query is a JSON object (used as the arguments)
// or plain text (bound to the schema's first required property, else
// "query"). Text content is concatenated; IsError is a Go error.
func callOn(session *sdk.ClientSession, tool, query string) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), callTimeout)
	defer cancel()
	var schema any
	for t, err := range session.Tools(ctx, nil) {
		if err == nil && t.Name == tool {
			schema = t.InputSchema
		}
	}
	args := argsFor(schema, query)
	res, err := session.CallTool(ctx, &sdk.CallToolParams{Name: tool, Arguments: args})
	if err != nil {
		return "", fmt.Errorf("mcp: %s: %w", tool, err)
	}
	var b strings.Builder
	for _, c := range res.Content {
		if tc, ok := c.(*sdk.TextContent); ok {
			b.WriteString(tc.Text)
		}
	}
	if res.IsError {
		return "", fmt.Errorf("mcp: %s: %s", tool, b.String())
	}
	return b.String(), nil
}

// argsFor turns the CLI's free-form argument into tool arguments.
func argsFor(schema any, query string) map[string]any {
	query = strings.TrimSpace(query)
	if query == "" {
		return map[string]any{}
	}
	var obj map[string]any
	if strings.HasPrefix(query, "{") && json.Unmarshal([]byte(query), &obj) == nil {
		return obj
	}
	key := "query"
	if m, ok := schema.(map[string]any); ok {
		if req, ok := m["required"].([]any); ok && len(req) > 0 {
			if k, ok := req[0].(string); ok {
				key = k
			}
		} else if props, ok := m["properties"].(map[string]any); ok && len(props) == 1 {
			for k := range props {
				key = k
			}
		}
	}
	return map[string]any{key: query}
}
