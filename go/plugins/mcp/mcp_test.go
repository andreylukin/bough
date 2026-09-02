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

func TestParseEntrySkipsNonStdio(t *testing.T) {
	if _, ok := parseEntry(map[string]any{"url": "http://x", "type": "http"}); ok {
		t.Fatal("url server should not parse as stdio")
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
	lines, err := listLines("test", cs)
	if err != nil || len(lines) != 1 || lines[0] != "test/greet  say hi" {
		t.Fatalf("listLines = %v, %v", lines, err)
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

// The model gets one line naming the servers and the CLI, nothing
// else; no servers, no line.
func TestPromptSectionIsOnlyAPointer(t *testing.T) {
	if promptSection(nil) != "" {
		t.Fatal("no servers should mean no section")
	}
	sec := promptSection(map[string]ServerConfig{"b": {}, "a": {}})
	if !strings.Contains(sec, "(a, b)") || !strings.Contains(sec, "bough mcp list") || strings.Contains(sec, "tools.mcp_") {
		t.Fatalf("section = %q", sec)
	}
}
