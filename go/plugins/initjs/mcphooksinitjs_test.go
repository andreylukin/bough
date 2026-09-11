package initjs

import (
	"os"
	"strings"
	"testing"
	"time"
)

// mcphooksinitjsKnown gates subtests that pin a known product bug.
func mcphooksinitjsKnown(t *testing.T, bug string) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_TOOLS_MCP_HOOKS_INITJS") != "1" {
		t.Skip("known bug (BOUGH_KNOWN_TOOLS_MCP_HOOKS_INITJS=1 to run): " + bug)
	}
}

const mcphooksinitjsTools = `
bough.tool("boom", function () { throw new Error("TOOL_BOOM") })
bough.tool("rej", async function () { throw new Error("ASYNC_BOOM") })
bough.tool("rejp", function () { return Promise.reject(new Error("PROMISE_BOOM")) })
bough.tool("res", async function () { return "ASYNC_OK" })
bough.tool("spin", function () { for (;;) {} })
`

// A throwing init.js tool is a block error naming the message, and the
// VM keeps serving the next block.
func TestMcpHooksInitjsToolThrows(t *testing.T) {
	_, cm, err := apply(t, "", mcphooksinitjsTools)
	if err != nil {
		t.Fatal(err)
	}
	out, err := cm.Run(`console.log("before"); tools.boom()`)
	if err == nil || !strings.Contains(err.Error(), "TOOL_BOOM") {
		t.Fatalf("Run = (%q, %v), want TOOL_BOOM error", out, err)
	}
	if !strings.Contains(out, "before") {
		t.Fatalf("output before the throw lost: %q", out)
	}
	if out, err := cm.Run(`1+1`); err != nil || out != "2" {
		t.Fatalf("VM unusable after a throwing tool: (%q, %v)", out, err)
	}
}

// A tool that never returns is cut by the VM timeout; the VM survives.
func TestMcpHooksInitjsToolSpins(t *testing.T) {
	_, cm, err := apply(t, "", mcphooksinitjsTools)
	if err != nil {
		t.Fatal(err)
	}
	start := time.Now()
	_, err = cm.Run(`tools.spin()`)
	if err == nil || !strings.Contains(err.Error(), "timeout") {
		t.Fatalf("spin err = %v, want timeout", err)
	}
	if d := time.Since(start); d > 10*time.Second {
		t.Fatalf("spin took %v", d)
	}
	if out, err := cm.Run(`"alive"`); err != nil || out != "alive" {
		t.Fatalf("VM unusable after timeout: (%q, %v)", out, err)
	}
}

// An async init.js tool: the rejection must reach the model as an
// error, and a resolved value as the value — not "[object Promise]".
func TestMcpHooksInitjsToolRejectedPromise(t *testing.T) {
	_, cm, err := apply(t, "", mcphooksinitjsTools)
	if err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct{ code, want string }{
		{`tools.rej()`, "ASYNC_BOOM"},
		{`tools.rejp()`, "PROMISE_BOOM"},
		{`tools.res()`, "ASYNC_OK"},
	} {
		t.Run(tc.code, func(t *testing.T) {
			out, err := cm.Run(tc.code)
			got := out
			if err != nil {
				got += err.Error()
			}
			if !strings.Contains(got, tc.want) {
				t.Fatalf("Run(%s) = (%q, %v), want %q", tc.code, out, err, tc.want)
			}
		})
	}
}

// Misregistration in init.js is a loud mount error, not a silent drop.
func TestMcpHooksInitjsToolBadRegistration(t *testing.T) {
	for _, js := range []string{`bough.tool("x")`, `bough.tool("", function(){})`, `bough.tool("x", 5)`} {
		if _, _, err := apply(t, "", js); err == nil || !strings.Contains(err.Error(), "bough.tool") {
			t.Fatalf("%s: err = %v, want bough.tool error", js, err)
		}
	}
}
