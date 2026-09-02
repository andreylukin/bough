package mcp

import (
	"context"
	"strings"
	"testing"

	sdk "github.com/modelcontextprotocol/go-sdk/mcp"
)

func TestMergePrecedence(t *testing.T) {
	global := map[string]ServerConfig{
		"a": {Command: "global-a"},
		"b": {Command: "global-b"},
		"c": {Command: "global-c"},
	}
	project := map[string]ServerConfig{
		"b": {Command: "project-b"},
		"d": {Command: "project-d"},
	}
	row := map[string]ServerConfig{
		"b": {Command: "row-b", Args: []string{"x"}},
	}
	got := merge([]string{"c"}, global, project, row)

	if len(got) != 3 {
		t.Fatalf("want 3 servers, got %v", got)
	}
	if got["a"].Command != "global-a" {
		t.Errorf("a: %+v", got["a"])
	}
	if got["b"].Command != "row-b" || len(got["b"].Args) != 1 {
		t.Errorf("b: row should win: %+v", got["b"])
	}
	if got["d"].Command != "project-d" {
		t.Errorf("d: %+v", got["d"])
	}
	if _, ok := got["c"]; ok {
		t.Errorf("c should be disabled")
	}
}

func TestParseEntryStdioAndHTTP(t *testing.T) {
	if sc, ok := parseEntry(map[string]any{"url": "http://x", "type": "http", "headers": map[string]any{"A": "b"}}); !ok || sc.URL != "http://x" || sc.Headers["A"] != "b" {
		t.Fatalf("url server should parse as http: %+v ok=%v", sc, ok)
	}
	if _, ok := parseEntry(map[string]any{"type": "http"}); ok {
		t.Fatal("neither command nor url should not parse")
	}
	sc, ok := parseEntry(map[string]any{
		"command": "srv",
		"args":    []any{"-v", "2"},
		"env":     map[string]any{"K": "V"},
	})
	if !ok || sc.Command != "srv" || len(sc.Args) != 2 || sc.Env["K"] != "V" {
		t.Fatalf("parseEntry: %+v ok=%v", sc, ok)
	}
}

// inMemorySession builds a one-tool in-process MCP server and returns a
// connected client session.
func inMemorySession(t *testing.T) *sdk.ClientSession {
	t.Helper()
	server := sdk.NewServer(&sdk.Implementation{Name: "test", Version: "0.1"}, nil)
	type args struct {
		Name string `json:"name"`
	}
	sdk.AddTool(server, &sdk.Tool{Name: "greet", Description: "say hi"},
		func(_ context.Context, _ *sdk.CallToolRequest, a args) (*sdk.CallToolResult, any, error) {
			if a.Name == "boom" {
				return &sdk.CallToolResult{
					Content: []sdk.Content{&sdk.TextContent{Text: "kaboom"}},
					IsError: true,
				}, nil, nil
			}
			return &sdk.CallToolResult{
				Content: []sdk.Content{&sdk.TextContent{Text: "hi " + a.Name}},
			}, nil, nil
		})

	ct, st := sdk.NewInMemoryTransports()
	ss, err := server.Connect(context.Background(), st, nil)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { ss.Close() })

	client := sdk.NewClient(&sdk.Implementation{Name: "bough-test", Version: "0.1"}, nil)
	cs, err := client.Connect(context.Background(), ct, nil)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { cs.Close() })
	return cs
}

// The CLI helpers over an in-memory server: list renders
// "server/tool  description"; call maps plain text onto the schema's
// required property and JSON onto the arguments; IsError is an error.
func TestListAndCall(t *testing.T) {
	cs := inMemorySession(t)
	tools, err := listTools(cs)
	if err != nil || len(tools) != 1 || tools[0] != (catalogTool{Name: "greet", Desc: "say hi"}) {
		t.Fatalf("listTools = %v, %v", tools, err)
	}
	out, err := callOn(cs, "greet", "you")
	if err != nil || out != "hi you" {
		t.Fatalf("call with text = (%q, %v)", out, err)
	}
	out, err = callOn(cs, "greet", `{"name": "json"}`)
	if err != nil || out != "hi json" {
		t.Fatalf("call with json = (%q, %v)", out, err)
	}
	if _, err := callOn(cs, "greet", "boom"); err == nil || !strings.Contains(err.Error(), "kaboom") {
		t.Fatalf("IsError should surface: %v", err)
	}
}

func TestArgsFor(t *testing.T) {
	schema := map[string]any{"required": []any{"q"}, "properties": map[string]any{"q": 1, "n": 2}}
	if got := argsFor(schema, "hello world"); got["q"] != "hello world" {
		t.Fatalf("required property binding = %v", got)
	}
	one := map[string]any{"properties": map[string]any{"path": 1}}
	if got := argsFor(one, "x"); got["path"] != "x" {
		t.Fatalf("single property binding = %v", got)
	}
	if got := argsFor(nil, "x"); got["query"] != "x" {
		t.Fatalf("fallback binding = %v", got)
	}
	if got := argsFor(nil, ""); len(got) != 0 {
		t.Fatalf("empty query = %v", got)
	}
}

// The model's context names the cached tools per configured server
// and points at the CLI; a server missing from the cache is named with
// the refresh hint; no servers, no section.
func TestPromptSectionCarriesCatalog(t *testing.T) {
	if promptSection(nil, catalog{}) != "" {
		t.Fatal("no servers should mean no section")
	}
	servers := map[string]ServerConfig{"b": {}, "a": {}}
	cat := catalog{Servers: map[string][]catalogTool{"a": {{Name: "greet", Desc: "say hi"}}}}
	sec := promptSection(servers, cat)
	for _, want := range []string{"bough mcp call", "- a (1 tools):", "a/greet  say hi", "- b: tools not listed yet, run bough mcp tools b"} {
		if !strings.Contains(sec, want) {
			t.Fatalf("section missing %q:\n%s", want, sec)
		}
	}
	if strings.Contains(sec, "tools.mcp_") {
		t.Fatalf("no codemode tool names: %s", sec)
	}
}

func TestCatalogRoundTrip(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	if got := loadCatalog(); got.Servers != nil {
		t.Fatalf("fresh home should have no catalog: %+v", got)
	}
	want := catalog{Servers: map[string][]catalogTool{"s": {{Name: "t", Desc: "d"}}}}
	if err := saveCatalog(want); err != nil {
		t.Fatal(err)
	}
	if got := loadCatalog(); len(got.Servers["s"]) != 1 || got.Servers["s"][0].Name != "t" {
		t.Fatalf("loadCatalog = %+v", got)
	}
}
