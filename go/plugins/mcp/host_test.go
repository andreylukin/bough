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
	"github.com/andreylukin/bough/plugins/codemode"
)

// richServer is a streamable-HTTP server with a structured tool, a
// text tool and a failing one; sessions counts connections.
func richServer(t *testing.T) (string, *atomic.Int32) {
	t.Helper()
	sessions := &atomic.Int32{}
	srv := sdk.NewServer(&sdk.Implementation{Name: "r", Version: "0"}, &sdk.ServerOptions{
		InitializedHandler: func(context.Context, *sdk.InitializedRequest) { sessions.Add(1) },
	})
	type searchArgs struct {
		Query string `json:"query" jsonschema:"words to look for in issues"`
		Limit int    `json:"limit,omitempty" jsonschema:"how many"`
	}
	type issue struct {
		ID    int    `json:"id"`
		Title string `json:"title"`
	}
	type searchOut struct {
		Items []issue `json:"items"`
	}
	sdk.AddTool(srv, &sdk.Tool{Name: "issue-search", Description: "Search issues.\nFull text over titles."},
		func(_ context.Context, _ *sdk.CallToolRequest, a searchArgs) (*sdk.CallToolResult, searchOut, error) {
			return nil, searchOut{Items: []issue{{1, "first " + a.Query}, {2, "second"}}}, nil
		})
	sdk.AddTool(srv, &sdk.Tool{Name: "greet", Description: "Say hi."},
		func(_ context.Context, _ *sdk.CallToolRequest, a map[string]any) (*sdk.CallToolResult, any, error) {
			return &sdk.CallToolResult{Content: []sdk.Content{&sdk.TextContent{Text: "hi " + a["name"].(string)}}}, nil, nil
		})
	sdk.AddTool(srv, &sdk.Tool{Name: "fail", Description: "Always fails."},
		func(context.Context, *sdk.CallToolRequest, map[string]any) (*sdk.CallToolResult, any, error) {
			return &sdk.CallToolResult{Content: []sdk.Content{&sdk.TextContent{Text: "kaboom"}}, IsError: true}, nil, nil
		})
	ts := httptest.NewServer(sdk.NewStreamableHTTPHandler(func(*http.Request) *sdk.Server { return srv }, nil))
	t.Cleanup(ts.Close)
	return ts.URL, sessions
}

// The host: search finds a tool by a parameter name and returns a
// callable signature; describe carries the schema and a JSDoc; a call
// returns structured content as a value, text as text, a tool error as
// ok:false; every call shares one session.
func TestHostSearchDescribeCall(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	url, sessions := richServer(t)
	h := newHost(map[string]ServerConfig{"gh": {URL: url}}, catalog{})
	defer h.Close()

	hits := h.Search("issues query", 0)
	if len(hits) == 0 || hits[0].Tool != "issue-search" || hits[0].Signature != "tools.mcp.gh.issueSearch({query: string, limit?: number})" {
		t.Fatalf("search: %+v", hits)
	}
	if hits[0].Description != "Search issues." || !strings.Contains(strings.Join(hits[0].Required, ","), "query") {
		t.Fatalf("hit: %+v", hits[0])
	}
	d, err := h.Describe("gh", "issue-search")
	if err != nil || d.InputSchema["properties"] == nil || d.OutputSchema == nil || !strings.Contains(d.JSDoc, "@param {string} args.query words to look for in issues") || !strings.Contains(d.Description, "Full text over titles.") {
		t.Fatalf("describe: %+v %v", d, err)
	}
	e, err := h.Call(context.Background(), "gh", "issue-search", map[string]any{"query": "bug"})
	if err != nil || !e.OK {
		t.Fatalf("call: %+v %v", e, err)
	}
	items, _ := e.Value.(map[string]any)["items"].([]any)
	if len(items) != 2 || items[0].(map[string]any)["title"] != "first bug" {
		t.Fatalf("value: %#v", e.Value)
	}
	if e2, err := h.Call(context.Background(), "gh", "greet", map[string]any{"name": "bo"}); err != nil || e2.Value != "hi bo" || e2.Text != "hi bo" {
		t.Fatalf("text call: %+v %v", e2, err)
	}
	e3, err := h.Call(context.Background(), "gh", "fail", nil)
	if err != nil || e3.OK || !e3.IsError || e3.Error == nil || e3.Error.Message != "kaboom" || e3.Value != nil {
		t.Fatalf("failed call: %+v %v", e3, err)
	}
	if _, err := h.Call(context.Background(), "nope", "x", nil); err == nil || !strings.Contains(err.Error(), `no server "nope"`) {
		t.Fatalf("unknown server: %v", err)
	}
	if _, err := h.Describe("gh", "missing"); err == nil || !strings.Contains(err.Error(), "no tool") {
		t.Fatalf("unknown tool: %v", err)
	}
	if n := sessions.Load(); n != 1 {
		t.Fatalf("opened %d sessions, want 1 shared", n)
	}
	// The catalog on disk now carries the schemas: the next mount searches without a server.
	if c := loadCatalog(); c.Servers["gh"][0].Schema == nil && c.Servers["gh"][1].Schema == nil {
		t.Fatalf("cached catalog lacks schemas: %+v", c.Servers["gh"])
	}
}

// Code mode: tools.mcp.search, describe and the per-tool stubs run
// inside a goja block and hand values back, including a stub the host
// learned after mount.
func TestCodeModeBinding(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	url, _ := richServer(t)
	h := newHost(map[string]ServerConfig{"gh": {URL: url}}, catalog{Servers: map[string][]catalogTool{"gh": {{Name: "greet", Desc: "Say hi."}}}})
	defer h.Close()
	cm := codemode.New(10 * time.Second)
	ctx := kernel.NewContext()
	ctx.Provide("codemode", cm)
	bindCodeMode(ctx, h)
	out, err := cm.Run(`
var g = tools.mcp.gh.greet({name: "bo"});
var hits = tools.mcp.search("issue titles", 3);
var d = tools.mcp.describe("gh", "issue-search");
var r = tools.mcp.gh.issueSearch({query: "x"});
var r2 = tools.mcp["gh"]["issue-search"]({query: "y"});
var f = tools.mcp.call("gh", "fail", {});
console.log(JSON.stringify([g.value, hits[0].tool, d.signature, r.value.items.length, r2.value.items[0].title, f.ok, f.error.message, tools.mcp.servers()]));
`)
	if err != nil {
		t.Fatal(err)
	}
	want := `["hi bo","issue-search","tools.mcp.gh.issueSearch({query: string, limit?: number})",2,"first y",false,"kaboom",["gh"]]`
	if strings.TrimSpace(out) != want {
		t.Fatalf("got  %s\nwant %s", strings.TrimSpace(out), want)
	}
	// A server the host cannot reach is an exception, not an envelope.
	h2 := newHost(map[string]ServerConfig{"down": {URL: "http://127.0.0.1:1"}}, catalog{})
	defer h2.Close()
	cm2 := codemode.New(10 * time.Second)
	ctx2 := kernel.NewContext()
	ctx2.Provide("codemode", cm2)
	bindCodeMode(ctx2, h2)
	if _, err := cm2.Run(`tools.mcp.call("down", "x", {})`); err == nil || !strings.Contains(err.Error(), "connect") {
		t.Fatalf("unreachable server: %v", err)
	}
}

// The engine's trio: three stable native tools over the same host.
func TestTrio(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	url, _ := richServer(t)
	h := newHost(map[string]ServerConfig{"gh": {URL: url}}, catalog{})
	defer h.Close()
	reg := agenttools.NewRegistry()
	ctx := kernel.NewContext()
	ctx.Provide("agent-tools", reg)
	registerTrio(ctx, h)
	names := map[string]agenttools.Tool{}
	for _, tl := range reg.Tools() {
		names[tl.Name] = tl
	}
	for _, n := range []string{"mcp_search", "mcp_describe", "mcp_call"} {
		if _, ok := names[n]; !ok {
			t.Fatalf("missing %s in %v", n, names)
		}
	}
	call := func(name, args string) agenttools.Result {
		r, err := names[name].Call(context.Background(), agenttools.Call{ID: "c", Args: json.RawMessage(args)})
		if err != nil {
			t.Fatal(err)
		}
		return r
	}
	if r := call("mcp_search", `{"query":"greet"}`); !strings.Contains(r.Text, `"tool": "greet"`) {
		t.Fatalf("search: %+v", r)
	}
	if r := call("mcp_describe", `{"server":"gh","tool":"issue-search"}`); !strings.Contains(r.Text, `"inputSchema"`) {
		t.Fatalf("describe: %+v", r)
	}
	if r := call("mcp_call", `{"server":"gh","tool":"issue-search","args":{"query":"q"}}`); r.Error != "" || !strings.Contains(r.Text, `"title": "first q"`) {
		t.Fatalf("call: %+v", r)
	}
	if r := call("mcp_call", `{"server":"gh","tool":"fail"}`); !strings.Contains(r.Error, "kaboom") || !strings.Contains(r.Text, `"isError": true`) {
		t.Fatalf("failed call: %+v", r)
	}
	if r := call("mcp_call", `{"server":"zz","tool":"x"}`); !strings.Contains(r.Error, `no server "zz"`) {
		t.Fatalf("unknown server: %+v", r)
	}
	if d := names["mcp_call"].Detail(json.RawMessage(`{"server":"gh","tool":"greet","args":{"name":"bo"}}`)); d != `gh/greet {"name":"bo"}` {
		t.Fatalf("detail %q", d)
	}
}

func TestIdentAndTypes(t *testing.T) {
	t.Parallel()
	for in, want := range map[string]string{"issue-search": "issueSearch", "get_user": "get_user", "greet": "greet", "9lives": "_9lives", "a.b": "aB"} {
		if got := ident(in); got != want {
			t.Errorf("ident(%q) = %q, want %q", in, got, want)
		}
	}
	sig, req := signature("s", "t", map[string]any{"type": "object", "required": []any{"b"}, "properties": map[string]any{
		"a": map[string]any{"type": "array", "items": map[string]any{"type": "integer"}},
		"b": map[string]any{"enum": []any{"x", "y"}},
		"c": map[string]any{"type": "object", "properties": map[string]any{"z": map[string]any{"type": "boolean"}}},
	}})
	if sig != `tools.mcp.s.t({b: "x" | "y", a?: number[], c?: {z: boolean}})` || len(req) != 1 {
		t.Fatalf("signature %q %v", sig, req)
	}
}
