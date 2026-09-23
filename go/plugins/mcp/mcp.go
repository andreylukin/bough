// Package mcp is the "mcp" plugin: MCP servers as a CLI, not as
// model tools. `bough mcp list` names the configured servers, `tools
// [server]` and `search <query>` find tools (refreshing the cached
// catalog), `status` checks each server answers, and `call
// <server/tool> [args]` runs one and prints its text.
// The model reaches them through the shell, so nothing is injected
// into its tool surface or prompt beyond a one-line pointer to the
// CLI (only when servers are configured). The exception is opt-in:
// config.native_tools lists servers whose tools the engine-unreal row
// offers as native mcp__<server>__<tool> calls (native.go).
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
	"maps"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"time"

	sdk "github.com/modelcontextprotocol/go-sdk/mcp"

	"github.com/andreylukin/bough/kernel"
)

const (
	connectTimeout = 10 * time.Second
	callTimeout    = 60 * time.Second
)

// ServerConfig is one MCP server: stdio (Command) or streamable HTTP
// (URL). Header values may be "${keychain:<service>#<json.path>}"
// references, resolved from the login keychain at connect time so no
// token lives in a file. Disabled entries are kept for listing (a
// stale grant from sync-mcp) but never connected.
type ServerConfig struct {
	Command  string
	Args     []string
	Env      map[string]string
	URL      string
	Headers  map[string]string
	Disabled bool
	Note     string // why it is disabled, for `bough mcp list`
}

// sections is the slice of the loop's "prompt-sections" service we need.
type sections interface {
	Set(name, text string)
}

// catalog is the cached tool list, written by `bough mcp tools`,
// `search` and `status`, read at mount so the model's context names the
// real tools without spawning a server at startup.
type catalog struct {
	At      time.Time                `json:"at"`
	Servers map[string][]catalogTool `json:"servers"`
}

type catalogTool struct {
	Name string `json:"name"`
	Desc string `json:"desc"` // first line
	// The rest, kept since the host's search and describe read them:
	// a schema is what lets code call a tool without a second lookup.
	Full   string         `json:"full,omitempty"`
	Schema map[string]any `json:"schema,omitempty"`
	Output map[string]any `json:"output,omitempty"`
}

func now() time.Time { return time.Now() }

func catalogPath() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return filepath.Join(home, ".bough", "mcp-catalog.json")
}

func loadCatalog() catalog {
	var c catalog
	if p := catalogPath(); p != "" {
		if data, err := os.ReadFile(p); err == nil {
			_ = json.Unmarshal(data, &c)
		}
	}
	return c
}

func saveCatalog(c catalog) error {
	p := catalogPath()
	if p == "" {
		return nil
	}
	data, err := json.MarshalIndent(c, "", "  ")
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
		return err
	}
	return os.WriteFile(p, data, 0o644)
}

// promptSection is the model's MCP context: the programmatic surface
// (tools.mcp in code mode, mcp_search/mcp_describe/mcp_call as native
// tools) and the servers, with their tools inline while the catalog is
// small enough to read and a search otherwise. Empty when no server is
// configured.
func promptSection(servers map[string]ServerConfig, cat catalog, native bool) string {
	if len(servers) == 0 {
		return ""
	}
	names := slices.Sorted(maps.Keys(servers))
	total := 0
	for _, n := range names {
		total += len(cat.Servers[n])
	}
	var b strings.Builder
	if native {
		b.WriteString("MCP servers are three native tools. mcp_search(query) returns [{server, tool, signature, description}] ranked; " +
			"mcp_describe(server, tool) returns {jsdoc, inputSchema, outputSchema}; mcp_call(server, tool, args) returns " +
			"{ok, value, text, content, isError, error}: value is the server's structured result, ok false with error.message when the tool itself failed, " +
			"and an error when the server could not be reached. Independent calls run at once: issue them together.\n")
		for _, n := range names {
			tools := cat.Servers[n]
			switch {
			case servers[n].Disabled:
				fmt.Fprintf(&b, "- %s: off (%s)\n", n, servers[n].Note)
			case len(tools) == 0:
				fmt.Fprintf(&b, "- %s: tools not listed yet; mcp_search lists them\n", n)
			case total > promptCatalogMax:
				fmt.Fprintf(&b, "- %s (%d tools; search to find one)\n", n, len(tools))
			default:
				fmt.Fprintf(&b, "- %s (%d tools):\n", n, len(tools))
				for _, t := range tools {
					fmt.Fprintf(&b, "  %s/%s  %s\n", n, t.Name, t.Desc)
				}
			}
		}
		return strings.TrimRight(b.String(), "\n")
	}
	b.WriteString("MCP servers are callable from code, as values, not through the shell. " +
		"tools.mcp.search(\"words\") returns [{server, tool, signature, description}] ranked; " +
		"tools.mcp.describe(server, tool) returns {jsdoc, inputSchema, outputSchema}; " +
		"tools.mcp.<server>.<tool>({...args}) (or tools.mcp.call(server, tool, args)) returns " +
		"{ok, value, text, content, isError, error}: value is the server's structured result (JSON parsed for you), " +
		"ok false with error.message when the tool itself failed, and a thrown exception when the server could not be reached. " +
		"Filter and join results in the same block and print only what matters.\n")
	for _, n := range names {
		tools := cat.Servers[n]
		switch {
		case servers[n].Disabled:
			fmt.Fprintf(&b, "- %s: off (%s)\n", n, servers[n].Note)
		case len(tools) == 0:
			fmt.Fprintf(&b, "- %s: tools not listed yet; tools.mcp.search lists them\n", n)
		case total > promptCatalogMax:
			fmt.Fprintf(&b, "- %s (%d tools; search to find one)\n", n, len(tools))
		default:
			fmt.Fprintf(&b, "- %s (%d tools):\n", n, len(tools))
			for _, t := range tools {
				sig, _ := signature(n, t.Name, t.Schema)
				fmt.Fprintf(&b, "  %s  %s\n", sig, t.Desc)
			}
		}
	}
	return strings.TrimRight(b.String(), "\n")
}

// promptCatalogMax is how many tools the prompt lists by signature; past
// it the model searches, as it would with hundreds.
const promptCatalogMax = 40

type plugin struct{}

func init() {
	kernel.Register("mcp", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "mcp" }
func (plugin) Inject() []string { return nil }

// Apply only documents the CLI to the model; servers are spawned on
// demand by the subcommands, never at mount — except the ones
// config.native_tools names, which the engine calls as native tools.
func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	servers, err := configuredServers(cfg)
	if err != nil {
		return err
	}
	host := newHost(servers, loadCatalog())
	ctx.Effect(host.Close)
	if err := startNative(ctx, cfg, servers, host); err != nil {
		return err
	}
	if len(servers) > 0 && programmatic(cfg) {
		bindCodeMode(ctx, host)
		registerTrio(ctx, host)
	}
	if s, err := kernel.Get[sections](ctx, "prompt-sections"); err == nil {
		if programmatic(cfg) {
			_, native := kernel.Get[any](ctx, "engine")
			s.Set("mcp", promptSection(servers, loadCatalog(), native == nil))
		} else {
			s.Set("mcp", shellPromptSection(servers, loadCatalog()))
		}
		ctx.Effect(func() { s.Set("mcp", "") })
	}
	// /connect shows server health next to the providers. Config is
	// re-read per call: mcp.json may change while bough runs.
	ctx.Provide("mcp-status", func() string {
		servers, err := configuredServers(cfg)
		if err != nil {
			return err.Error()
		}
		return probe(servers)
	})
	return nil
}

// probe connects to every server at once and reports one line each:
// ok with its tool count, off, or DOWN with the error.
func probe(servers map[string]ServerConfig) string {
	names := slices.Sorted(maps.Keys(servers))
	lines := make([]string, len(names))
	var wg sync.WaitGroup
	for i, n := range names {
		wg.Go(func() {
			sc := servers[n]
			if sc.Disabled {
				lines[i] = fmt.Sprintf("  %-20s off   %s", n, sc.Note)
				return
			}
			session, err := connect(sc)
			if err != nil {
				lines[i] = fmt.Sprintf("  %-20s DOWN  %v", n, err)
				return
			}
			tools, err := listTools(session)
			session.Close()
			if err != nil {
				lines[i] = fmt.Sprintf("  %-20s DOWN  list tools: %v", n, err)
				return
			}
			lines[i] = fmt.Sprintf("  %-20s ok    %d tools", n, len(tools))
		})
	}
	wg.Wait()
	return strings.Join(lines, "\n")
}

// configuredServers merges every config source for the given row config.
func configuredServers(cfg map[string]any) (map[string]ServerConfig, error) {
	row, err := rowServers(cfg)
	if err != nil {
		return nil, err
	}
	var global, synced, mine map[string]ServerConfig
	if home, err := os.UserHomeDir(); err == nil {
		global = loadServersFile(filepath.Join(home, ".claude.json"))
		synced = loadServersFile(filepath.Join(home, ".bough", "mcp.sync.json"))
		mine = loadServersFile(filepath.Join(home, ".bough", "mcp.json"))
	}
	project := loadServersFile(".mcp.json")
	return merge(disableList(cfg), global, project, synced, mine, row), nil
}

// Commands implements kernel.Commander: `bough mcp list|call`.
func (plugin) Commands() []kernel.Command {
	return []kernel.Command{{
		Name:    "sync-mcp",
		Usage:   "[--dry-run]",
		Summary: "adopt Claude Code's MCP OAuth grants by keychain reference (writes ~/.bough/mcp.sync.json)",
		Run:     func(_ map[string]any, args []string) error { return runSync(args) },
	}, {
		Name:    "mcp",
		Usage:   "list | tools [server] | search <q> | status | call <server/tool> [args]",
		Summary: "MCP servers: list them, their tools, search tools, check health, call one",
		Run:     runCLI,
	}}
}

func runCLI(cfg map[string]any, args []string) error {
	const usage = "usage: bough mcp list | tools [server] | search <query> | status | sync [--dry-run] | call <server/tool> [json-args|text]"
	if len(args) == 0 {
		return fmt.Errorf("%s", usage)
	}
	servers, err := configuredServers(cfg)
	if err != nil {
		return err
	}
	if len(servers) == 0 && args[0] != "call" {
		return fmt.Errorf("no MCP servers configured (row config, ./.mcp.json, ~/.claude.json)")
	}
	names := slices.Sorted(maps.Keys(servers))
	cat := loadCatalog()

	switch args[0] {
	case "sync":
		return runSync(args[1:])
	case "list":
		// Servers only, from config plus the cached tool counts: no
		// connections. `tools`/`status` refresh the counts.
		for _, n := range names {
			count := "tools not listed yet"
			if ts := cat.Servers[n]; len(ts) > 0 {
				count = fmt.Sprintf("%d tools", len(ts))
			}
			if servers[n].Disabled {
				count = "disabled: " + servers[n].Note
			}
			fmt.Printf("%-20s %-22s %s\n", n, count, where(servers[n]))
		}
		return nil

	case "tools":
		want := names
		if len(args) > 1 {
			if _, ok := servers[args[1]]; !ok {
				return fmt.Errorf("no MCP server %q configured (bough mcp list)", args[1])
			}
			want = []string{args[1]}
		}
		failed := refresh(servers, want, &cat, func(n string, err error) {
			fmt.Fprintf(os.Stderr, "mcp: %s: %v\n", n, err)
		})
		for _, n := range want {
			for _, t := range cat.Servers[n] {
				fmt.Printf("%s/%s  %s\n", n, t.Name, t.Desc)
			}
		}
		if failed > 0 {
			return fmt.Errorf("%d of %d servers failed", failed, len(want))
		}
		return nil

	case "search":
		if len(args) < 2 {
			return fmt.Errorf("usage: bough mcp search <query>")
		}
		query := strings.ToLower(strings.Join(args[1:], " "))
		// Search the catalog; fill it first for servers never listed.
		var missing []string
		for _, n := range names {
			if len(cat.Servers[n]) == 0 {
				missing = append(missing, n)
			}
		}
		refresh(servers, missing, &cat, func(n string, err error) {
			fmt.Fprintf(os.Stderr, "mcp: %s: %v\n", n, err)
		})
		hits := 0
		for _, n := range names {
			for _, t := range cat.Servers[n] {
				if strings.Contains(strings.ToLower(n+"/"+t.Name+" "+t.Desc), query) {
					fmt.Printf("%s/%s  %s\n", n, t.Name, t.Desc)
					hits++
				}
			}
		}
		if hits == 0 {
			return fmt.Errorf("no tool matches %q", strings.Join(args[1:], " "))
		}
		return nil

	case "status":
		failed := 0
		for _, n := range names {
			if servers[n].Disabled {
				fmt.Printf("%-20s off   %s\n", n, servers[n].Note)
				continue
			}
			started := time.Now()
			session, err := connect(servers[n])
			if err != nil {
				failed++
				fmt.Printf("%-20s DOWN  %v\n", n, err)
				continue
			}
			tools, err := listTools(session)
			session.Close()
			if err != nil {
				failed++
				fmt.Printf("%-20s DOWN  list tools: %v\n", n, err)
				continue
			}
			if cat.Servers == nil {
				cat.Servers = map[string][]catalogTool{}
			}
			cat.Servers[n] = tools
			fmt.Printf("%-20s ok    %d tools  %s  %s\n", n, len(tools),
				time.Since(started).Round(time.Millisecond), where(servers[n]))
		}
		cat.At = time.Now()
		if err := saveCatalog(cat); err != nil {
			fmt.Fprintf(os.Stderr, "mcp: catalog not saved: %v\n", err)
		} else {
			fmt.Printf("catalog: %s (the model's context is rebuilt from it at the next start)\n", catalogPath())
		}
		if failed > 0 {
			return fmt.Errorf("%d of %d servers failed", failed, len(names))
		}
		return nil

	case "call":
		if len(args) < 2 {
			return fmt.Errorf("usage: bough mcp call <server/tool> [json-args|text]")
		}
		server, tool, _ := strings.Cut(args[1], "/")
		if tool == "" {
			return fmt.Errorf("name %q must be <server>/<tool> (see bough mcp tools)", args[1])
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
	return fmt.Errorf("unknown mcp command %q\n%s", args[0], usage)
}

// refresh connects to each named server, replaces its catalog entry
// with the live tool list, saves the catalog, and reports failures
// through onErr. Returns the failure count.
func refresh(servers map[string]ServerConfig, names []string, cat *catalog, onErr func(string, error)) int {
	if len(names) == 0 {
		return 0
	}
	if cat.Servers == nil {
		cat.Servers = map[string][]catalogTool{}
	}
	failed := 0
	for _, n := range names {
		if servers[n].Disabled {
			continue
		}
		session, err := connect(servers[n])
		if err != nil {
			failed++
			onErr(n, fmt.Errorf("connect: %w", err))
			continue
		}
		tools, err := listTools(session)
		session.Close()
		if err != nil {
			failed++
			onErr(n, fmt.Errorf("list tools: %w", err))
			continue
		}
		cat.Servers[n] = tools
	}
	cat.At = time.Now()
	if err := saveCatalog(*cat); err != nil {
		onErr("catalog", err)
	}
	return failed
}

// where renders a server's location for listings.
func where(sc ServerConfig) string {
	if sc.URL != "" {
		return sc.URL
	}
	return strings.TrimSpace(sc.Command + " " + strings.Join(sc.Args, " "))
}

// merge combines server maps lowest-precedence FIRST (later layers win
// by name), then removes names in disable. Pure.
func merge(disable []string, layers ...map[string]ServerConfig) map[string]ServerConfig {
	out := map[string]ServerConfig{}
	for _, layer := range layers {
		maps.Copy(out, layer)
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
			return nil, fmt.Errorf("mcp: config.servers.%s: command or url is required", name)
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
		MCPServers map[string]map[string]any `json:"mcpServers"` // Claude Code
		Servers    map[string]map[string]any `json:"servers"`    // bough
	}
	if err := json.Unmarshal(data, &doc); err != nil {
		fmt.Fprintf(os.Stderr, "mcp: %s: %v (skipped)\n", path, err)
		return nil
	}
	entries := doc.MCPServers
	if entries == nil {
		entries = doc.Servers
	}
	out := map[string]ServerConfig{}
	for name, m := range entries {
		sc, ok := parseEntry(m)
		if !ok {
			fmt.Fprintf(os.Stderr, "mcp: %s: server %q has neither command nor url, skipped\n", path, name)
			continue
		}
		out[name] = sc
	}
	return out
}

// parseEntry converts one raw server entry: a command (stdio) or a
// url (streamable HTTP, with optional headers). ok=false with neither.
func parseEntry(m map[string]any) (ServerConfig, bool) {
	cmd, _ := m["command"].(string)
	url, _ := m["url"].(string)
	if cmd == "" && url == "" {
		return ServerConfig{}, false
	}
	sc := ServerConfig{Command: cmd, URL: url}
	if h, ok := m["headers"].(map[string]any); ok && len(h) > 0 {
		sc.Headers = map[string]string{}
		for k, v := range h {
			if s, ok := v.(string); ok {
				sc.Headers[k] = s
			}
		}
	}
	if d, ok := m["disabled"].(bool); ok {
		sc.Disabled = d
	}
	if n, ok := m["note"].(string); ok {
		sc.Note = n
	}
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

// connect opens an MCP session: an HTTP server via streamable HTTP
// with its headers resolved, else the stdio server process.
func connect(sc ServerConfig) (*sdk.ClientSession, error) {
	ctx, cancel := context.WithTimeout(context.Background(), connectTimeout)
	defer cancel()
	if sc.Disabled {
		return nil, fmt.Errorf("disabled: %s", sc.Note)
	}
	client := sdk.NewClient(&sdk.Implementation{Name: "bough", Version: "0.1"}, nil)
	if sc.URL != "" {
		headers := map[string]string{}
		for k, v := range sc.Headers {
			rv, err := resolveRef(v)
			if err != nil {
				return nil, fmt.Errorf("header %s: %w", k, err)
			}
			headers[k] = rv
		}
		hc := &http.Client{Transport: headerRoundTripper{headers: headers, next: http.DefaultTransport}}
		return client.Connect(ctx, &sdk.StreamableClientTransport{Endpoint: sc.URL, HTTPClient: hc}, nil)
	}
	cmd := exec.Command(sc.Command, sc.Args...)
	cmd.Env = os.Environ()
	for k, v := range sc.Env {
		cmd.Env = append(cmd.Env, k+"="+v)
	}
	return client.Connect(ctx, &sdk.CommandTransport{Command: cmd}, nil)
}

// headerRoundTripper adds fixed headers to every request.
type headerRoundTripper struct {
	headers map[string]string
	next    http.RoundTripper
}

func (h headerRoundTripper) RoundTrip(r *http.Request) (*http.Response, error) {
	r = r.Clone(r.Context())
	for k, v := range h.headers {
		r.Header.Set(k, v)
	}
	return h.next.RoundTrip(r)
}

// listTools returns the session's tools with one-line descriptions.
func listTools(session *sdk.ClientSession) ([]catalogTool, error) {
	ctx, cancel := context.WithTimeout(context.Background(), connectTimeout)
	defer cancel()
	var out []catalogTool
	for tool, err := range session.Tools(ctx, nil) {
		if err != nil {
			return out, err
		}
		desc, _, _ := strings.Cut(strings.TrimSpace(tool.Description), "\n")
		ct := catalogTool{Name: tool.Name, Desc: desc, Full: strings.TrimSpace(tool.Description)}
		if ct.Full == ct.Desc {
			ct.Full = ""
		}
		if m := schemaOf(tool.InputSchema); len(m) > 1 {
			ct.Schema = m
		}
		if tool.OutputSchema != nil {
			if m := schemaOf(tool.OutputSchema); len(m) > 1 {
				ct.Output = m
			}
		}
		out = append(out, ct)
	}
	return out, nil
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
	// A server with structured output prints it as JSON: the shell
	// caller gets the same value code mode does, not a rendering.
	if res.StructuredContent != nil && b.Len() == 0 {
		if raw, err := json.MarshalIndent(res.StructuredContent, "", "  "); err == nil {
			return string(raw), nil
		}
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

// Call runs one tool on a configured server for another plugin (the
// collectors): the same config sources as `bough mcp call`, one
// session per call. args is a JSON object or the free-form text
// `bough mcp call` accepts.
func Call(server, tool, args string) (string, error) {
	servers, err := configuredServers(nil)
	if err != nil {
		return "", err
	}
	sc, ok := servers[server]
	if !ok {
		return "", fmt.Errorf("no MCP server %q configured", server)
	}
	session, err := connect(sc)
	if err != nil {
		return "", fmt.Errorf("%s: connect: %w", server, err)
	}
	defer session.Close()
	return callOn(session, tool, args)
}
