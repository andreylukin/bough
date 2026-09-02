package mcp

import (
	"context"
	"strings"
	"testing"
	"time"

	sdk "github.com/modelcontextprotocol/go-sdk/mcp"

	"github.com/andreylukin/bough/plugins/codemode"
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

func TestCodemodeRoundTrip(t *testing.T) {
	cs := inMemorySession(t)
	cm := codemode.New(5 * time.Second)

	lines, err := registerSession(cm, "test", cs)
	if err != nil {
		t.Fatal(err)
	}
	if len(lines) != 1 {
		t.Fatalf("want 1 tool bound, got %d", len(lines))
	}
	want := "tools.mcp_test_greet(args) -> string: say hi"
	if lines[0] != want {
		t.Fatalf("prompt line = %q, want %q", lines[0], want)
	}
	if sec := promptSection(lines); !strings.Contains(sec, "MCP tools") || !strings.Contains(sec, "- "+want) {
		t.Fatalf("promptSection = %q", sec)
	}
	if promptSection(nil) != "" {
		t.Fatal("empty section must be empty")
	}

	out, err := cm.Run(`tools.mcp_test_greet({name: "you"})`)
	if err != nil {
		t.Fatal(err)
	}
	if out != "hi you" {
		t.Fatalf("want %q, got %q", "hi you", out)
	}

	// IsError result surfaces as a JS exception.
	_, err = cm.Run(`tools.mcp_test_greet({name: "boom"})`)
	if err == nil || !strings.Contains(err.Error(), "kaboom") {
		t.Fatalf("want kaboom error, got %v", err)
	}
}
