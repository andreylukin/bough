package mcp

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	sdk "github.com/modelcontextprotocol/go-sdk/mcp"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/kernel"
)

func TestNativeName(t *testing.T) {
	t.Parallel()
	none := func(string) bool { return false }
	if got := nativeName("linear-server", "list_issues", none); got != "mcp__linear-server__list_issues" {
		t.Fatalf("plain = %q", got)
	}
	if got := nativeName("my.server", "get/item v2", none); got != "mcp__my_server__get_item_v2" {
		t.Fatalf("sanitized = %q", got)
	}
	long := nativeName("s", strings.Repeat("x", 80), none)
	if len(long) != 64 || !agenttools.ValidName(long) {
		t.Fatalf("long = %q (%d)", long, len(long))
	}
	taken := map[string]bool{"mcp__a__b": true, "mcp__a__b_2": true}
	if got := nativeName("a", "b", func(s string) bool { return taken[s] }); got != "mcp__a__b_3" {
		t.Fatalf("clash = %q", got)
	}
	got := nativeName("s", strings.Repeat("x", 80), func(s string) bool { return s == long })
	if len(got) != 64 || !strings.HasSuffix(got, "_2") {
		t.Fatalf("clash on a cut name = %q", got)
	}
}

func TestNativeServersConfig(t *testing.T) {
	t.Parallel()
	if got, err := nativeServers(map[string]any{}); err != nil || got != nil {
		t.Fatalf("absent = %v %v", got, err)
	}
	if got, err := nativeServers(map[string]any{"native_tools": []any{"a", "b"}}); err != nil || len(got) != 2 {
		t.Fatalf("list = %v %v", got, err)
	}
	for _, bad := range []any{"a", []any{1}, []any{""}} {
		if _, err := nativeServers(map[string]any{"native_tools": bad}); err == nil || !strings.HasPrefix(err.Error(), "mcp: native_tools") {
			t.Fatalf("%v: err = %v", bad, err)
		}
	}
	// Without native_tools the row reads nothing more: no agent-tools
	// lookup, so a loop session mounts as it always has.
	if err := startNative(kernel.NewContext(), map[string]any{}, nil); err != nil {
		t.Fatal(err)
	}
	if err := startNative(kernel.NewContext(), map[string]any{"native_tools": []any{"s"}}, nil); err == nil || !strings.Contains(err.Error(), "agent-tools") {
		t.Fatalf("missing registry: %v", err)
	}
}

// testServer is a streamable-HTTP MCP server with two tools. sessions
// counts the client sessions bough opened.
func testServer(t *testing.T) (url string, sessions *atomic.Int32) {
	t.Helper()
	sessions = &atomic.Int32{}
	srv := sdk.NewServer(&sdk.Implementation{Name: "t", Version: "0"}, &sdk.ServerOptions{
		InitializedHandler: func(context.Context, *sdk.InitializedRequest) { sessions.Add(1) },
	})
	type greetArgs struct {
		Name string `json:"name" jsonschema:"who to greet"`
	}
	sdk.AddTool(srv, &sdk.Tool{Name: "greet", Description: "Say hi.\nSecond line."},
		func(_ context.Context, _ *sdk.CallToolRequest, a greetArgs) (*sdk.CallToolResult, any, error) {
			return &sdk.CallToolResult{Content: []sdk.Content{&sdk.TextContent{Text: "hi " + a.Name}}}, nil, nil
		})
	sdk.AddTool(srv, &sdk.Tool{Name: "fail.now", Description: "Always fails."},
		func(context.Context, *sdk.CallToolRequest, map[string]any) (*sdk.CallToolResult, any, error) {
			return &sdk.CallToolResult{Content: []sdk.Content{&sdk.TextContent{Text: "kaboom"}}, IsError: true}, nil, nil
		})
	handler := sdk.NewStreamableHTTPHandler(func(*http.Request) *sdk.Server { return srv }, nil)
	ts := httptest.NewServer(handler)
	t.Cleanup(ts.Close)
	return ts.URL, sessions
}

// waitTools waits until the registry holds n tools.
func waitTools(t *testing.T, reg agenttools.Registry, n int) []agenttools.Tool {
	t.Helper()
	deadline := time.After(10 * time.Second)
	for {
		changed := reg.Changed()
		if tools := reg.Tools(); len(tools) == n {
			return tools
		}
		select {
		case <-changed:
		case <-deadline:
			t.Fatalf("registry holds %d tools, want %d", len(reg.Tools()), n)
		}
	}
}

// A server in native_tools becomes one native tool per server tool,
// with its input schema; calls share one connection; a tool error is
// the call's error with the server's text; unmount unregisters them.
func TestNativeToolsRegisterCallAndUnmount(t *testing.T) {
	t.Parallel()
	url, sessions := testServer(t)
	reg := agenttools.NewRegistry()
	ctx := kernel.NewContext()
	t.Cleanup(ctx.Unmount) // before the server's Close, which waits on open sessions
	ctx.Provide("agent-tools", reg)
	servers := map[string]ServerConfig{"srv": {URL: url}, "off": {URL: url, Disabled: true}}
	if err := startNative(ctx, map[string]any{"native_tools": []any{"srv", "off", "missing"}}, servers); err != nil {
		t.Fatal(err)
	}
	tools := waitTools(t, reg, 2)
	if tools[0].Name != "mcp__srv__fail_now" || tools[1].Name != "mcp__srv__greet" {
		t.Fatalf("names = %s, %s", tools[0].Name, tools[1].Name)
	}
	greet := tools[1]
	if greet.Description != "Say hi.\nSecond line." {
		t.Fatalf("description = %q", greet.Description)
	}
	props, _ := greet.Schema["properties"].(map[string]any)
	if greet.Schema["type"] != "object" || props["name"] == nil {
		t.Fatalf("schema = %v", greet.Schema)
	}
	if d := greet.Detail(json.RawMessage(`{"name": "you"}`)); d != `srv/greet {"name":"you"}` {
		t.Fatalf("detail = %q", d)
	}

	for _, who := range []string{"a", "b"} {
		res, err := greet.Call(context.Background(), agenttools.Call{ID: who, Args: json.RawMessage(`{"name":"` + who + `"}`)})
		if err != nil || res.Text != "hi "+who || res.Error != "" {
			t.Fatalf("call %s = %+v %v", who, res, err)
		}
	}
	res, err := tools[0].Call(context.Background(), agenttools.Call{ID: "f"})
	if err != nil || res.Text != "kaboom" || !strings.Contains(res.Error, "srv/fail.now failed") {
		t.Fatalf("failing tool = %+v %v", res, err)
	}
	if n := sessions.Load(); n != 1 {
		t.Fatalf("opened %d connections for 3 calls and a listing, want 1", n)
	}

	ctx.Unmount()
	if n := len(reg.Tools()); n != 0 {
		t.Fatalf("%d tools left after unmount", n)
	}
	if _, err := greet.Call(context.Background(), agenttools.Call{ID: "late"}); err == nil || !strings.Contains(err.Error(), "unmounted") {
		t.Fatalf("call after unmount = %v", err)
	}
}

// A name another row already registered is not taken over: the MCP
// tool gets the next free suffix.
func TestNativeToolsAvoidTakenNames(t *testing.T) {
	t.Parallel()
	url, _ := testServer(t)
	reg := agenttools.NewRegistry()
	if _, err := reg.Register(agenttools.Tool{Name: "mcp__srv__greet", Call: func(context.Context, agenttools.Call) (agenttools.Result, error) {
		return agenttools.Result{}, nil
	}}); err != nil {
		t.Fatal(err)
	}
	ctx := kernel.NewContext()
	t.Cleanup(ctx.Unmount)
	ctx.Provide("agent-tools", reg)
	if err := startNative(ctx, map[string]any{"native_tools": []any{"srv"}}, map[string]ServerConfig{"srv": {URL: url}}); err != nil {
		t.Fatal(err)
	}
	tools := waitTools(t, reg, 3)
	if tools[2].Name != "mcp__srv__greet_2" {
		t.Fatalf("names = %v", []string{tools[0].Name, tools[1].Name, tools[2].Name})
	}
}
