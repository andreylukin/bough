// TUI-integration tests: an in-process (in-memory transport) MCP
// server's tools bound through registerSession into a real codemode
// row, exercised by the loop and rendered by the real ui model.
// Internal test package: registerSession is the seam under test.
package mcp

import (
	"context"
	"strings"
	"testing"

	sdk "github.com/modelcontextprotocol/go-sdk/mcp"

	"github.com/andreylukin/bough/internal/uitest"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"

	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/loop"
)

// bindInMemoryTool spins up an in-memory MCP server exposing one tool
// and binds it into the mounted codemode service as
// tools.mcp_<server>_<tool>.
func bindInMemoryTool(t *testing.T, d *uitest.Driver, server, tool string, result *sdk.CallToolResult) {
	t.Helper()
	ct, st := sdk.NewInMemoryTransports()
	srv := sdk.NewServer(&sdk.Implementation{Name: server, Version: "0"}, nil)
	sdk.AddTool(srv, &sdk.Tool{Name: tool, Description: "test tool"},
		func(context.Context, *sdk.CallToolRequest, map[string]any) (*sdk.CallToolResult, any, error) {
			return result, nil, nil
		})
	ss, err := srv.Connect(context.Background(), st, nil)
	if err != nil {
		t.Fatalf("server connect: %v", err)
	}
	t.Cleanup(func() { _ = ss.Close() })
	client := sdk.NewClient(&sdk.Implementation{Name: "bough-test", Version: "0"}, nil)
	cs, err := client.Connect(context.Background(), ct, nil)
	if err != nil {
		t.Fatalf("client connect: %v", err)
	}
	t.Cleanup(func() { _ = cs.Close() })
	reg, err := kernel.Get[registry](d.Ctx, "codemode")
	if err != nil {
		t.Fatalf("codemode service: %v", err)
	}
	n, err := registerSession(reg, server, cs)
	if err != nil || n != 1 {
		t.Fatalf("registerSession: bound %d, err %v", n, err)
	}
}

// oneShotParrot emits one js block, then finishes.
func oneShotParrot(code, finish string) uitest.LLMFunc {
	step := 0
	return func(string, []llm.Message) string {
		step++
		if step == 1 {
			return "```js\n" + code + "\n```"
		}
		return finish
	}
}

// A bound MCP tool's text result renders as a result block.
func TestBoundToolResultRenders(t *testing.T) {
	t.Parallel()
	d := uitest.Mount(t, func(c *kernel.Context) {
		c.Provide("llm", oneShotParrot(`console.log(tools.mcp_memsrv_greet({}))`, "mcp turn over"))
	}, "codemode", "loop")
	bindInMemoryTool(t, d, "memsrv", "greet", &sdk.CallToolResult{
		Content: []sdk.Content{&sdk.TextContent{Text: "MCP_GREETING_OK"}},
	})
	d.Say("go")
	d.WaitFor("mcp turn over")
	frame := d.Frame()
	if !strings.Contains(frame, "MCP_GREETING_OK") {
		t.Fatalf("MCP result missing:\n%s", frame)
	}
	if !strings.Contains(frame, "╭─ result") {
		t.Fatalf("result box missing:\n%s", frame)
	}
}

// A tool result flagged IsError surfaces as a rendered error block,
// and the loop keeps going to a clean finish.
func TestToolErrorRendersAsError(t *testing.T) {
	t.Parallel()
	d := uitest.Mount(t, func(c *kernel.Context) {
		c.Provide("llm", oneShotParrot(`console.log(tools.mcp_errsrv_boom({}))`, "recovered fine"))
	}, "codemode", "loop")
	bindInMemoryTool(t, d, "errsrv", "boom", &sdk.CallToolResult{
		IsError: true,
		Content: []sdk.Content{&sdk.TextContent{Text: "MCPERR_KAPOW"}},
	})
	d.Say("go")
	d.WaitFor("recovered fine")
	frame := d.Frame()
	if !strings.Contains(frame, "MCPERR_KAPOW") {
		t.Fatalf("MCP error text missing:\n%s", frame)
	}
	if !strings.Contains(frame, "✗") {
		t.Fatalf("error block glyph missing:\n%s", frame)
	}
}
