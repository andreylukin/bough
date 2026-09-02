// Package mcp is the "mcp" plugin: it spawns stdio MCP servers and
// binds their tools into the codemode service as tools.mcp_<server>_<tool>.
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

// registry is the slice of the codemode service we need.
type registry interface {
	RegisterTool(name string, fn any)
}

type plugin struct{}

func init() {
	kernel.Register("mcp", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "mcp" }
func (plugin) Inject() []string { return []string{"codemode"} }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	reg, err := kernel.Get[registry](ctx, "codemode")
	if err != nil {
		return err
	}

	row, err := rowServers(cfg)
	if err != nil {
		return err
	}
	var global map[string]ServerConfig
	if home, err := os.UserHomeDir(); err == nil {
		global = loadServersFile(filepath.Join(home, ".claude.json"))
	}
	project := loadServersFile(".mcp.json")
	servers := merge(disableList(cfg), global, project, row)

	var sessions []*sdk.ClientSession
	for name, sc := range servers {
		session, err := connect(sc)
		if err != nil {
			fmt.Fprintf(os.Stderr, "mcp: server %q: connect: %v (skipped)\n", name, err)
			continue
		}
		n, err := registerSession(reg, name, session)
		if err != nil {
			fmt.Fprintf(os.Stderr, "mcp: server %q: list tools: %v (skipped)\n", name, err)
			session.Close()
			continue
		}
		kernel.Logf("mcp: server %q: %d tools bound\n", name, n)
		sessions = append(sessions, session)
	}
	ctx.Effect(func() {
		for _, s := range sessions {
			s.Close()
		}
	})
	return nil
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

// registerSession lists the session's tools and binds each into
// codemode as mcp_<server>_<tool>. Returns the number bound.
func registerSession(reg registry, server string, session *sdk.ClientSession) (int, error) {
	ctx, cancel := context.WithTimeout(context.Background(), connectTimeout)
	defer cancel()
	n := 0
	for tool, err := range session.Tools(ctx, nil) {
		if err != nil {
			return n, err
		}
		reg.RegisterTool("mcp_"+sanitize(server)+"_"+sanitize(tool.Name), bindTool(session, tool.Name))
		n++
	}
	return n, nil
}

// bindTool returns the codemode tool fn: one map arg in, concatenated
// text content out, IsError as a Go error.
func bindTool(session *sdk.ClientSession, tool string) func(map[string]any) (string, error) {
	return func(args map[string]any) (string, error) {
		ctx, cancel := context.WithTimeout(context.Background(), callTimeout)
		defer cancel()
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
}

// sanitize maps a name onto JS-identifier-safe characters so
// tools.mcp_x_y works with dot access.
func sanitize(s string) string {
	return strings.Map(func(r rune) rune {
		switch {
		case r >= 'a' && r <= 'z', r >= 'A' && r <= 'Z', r >= '0' && r <= '9', r == '_':
			return r
		default:
			return '_'
		}
	}, s)
}
