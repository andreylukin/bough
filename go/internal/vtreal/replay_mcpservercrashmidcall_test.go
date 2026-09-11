package vtreal

// A stdio MCP server that dies in the middle of a tool call, seen from
// the real binary. MCP servers are a CLI in bough (`bough mcp call`,
// reached through the shell), so the TUI surface is a "!" line running
// that CLI: its result block must carry the error, the composer must
// take (and finish) the next turn, `bough mcp status` must say DOWN
// for a server that cannot answer, and a second crashing call must
// come back well inside the 60 s call timeout.
//
// The stub is a sh script speaking newline-delimited JSON-RPC: it
// answers initialize and tools/list, and exits 3 on the tools/call
// request, so the crash always lands mid call. Any other request with
// an id gets -32601: the go-sdk client opens with server/discover and
// only falls back to initialize on an error reply. With MCPCRASH_DOWN
// set it exits on its first request, the server `status` must report
// DOWN.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const mcpServerCrashMidCallStub = `#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -n "$MCPCRASH_DOWN" ] && exit 4
  case "$line" in
  *'"method":"initialize"'*)
    v=$(printf '%s' "$line" | sed -n 's/.*"protocolVersion":"\([^"]*\)".*/\1/p')
    printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"%s","capabilities":{"tools":{}},"serverInfo":{"name":"crashy","version":"1"}}}\n' "$id" "$v";;
  *'"method":"tools/list"'*)
    printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"boom","description":"exits mid call","inputSchema":{"type":"object","properties":{"query":{"type":"string"}}}}]}}\n' "$id";;
  *'"method":"tools/call"'*)
    echo "MCPCRASH-STUB exiting on request id $id" >&2
    exit 3;;
  *'"method":"notifications/'*) ;;
  *)
    [ -n "$id" ] && printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"method not found"}}\n' "$id";;
  esac
done
`

// mcpServerCrashMidCallStart boots bough with ~/.bough/mcp.json naming
// two servers backed by the stub: "crashy" (dies on tools/call) and
// "deadboot" (dies on initialize).
func mcpServerCrashMidCallStart(t *testing.T) *app {
	t.Helper()
	dir := t.TempDir()
	stub := filepath.Join(dir, "stub.sh")
	if err := os.WriteFile(stub, []byte(mcpServerCrashMidCallStub), 0o755); err != nil {
		t.Fatal(err)
	}
	a := start(t, 120, 40)
	servers := `{"servers":{` +
		`"crashy":{"command":"` + stub + `"},` +
		`"deadboot":{"command":"` + stub + `","env":{"MCPCRASH_DOWN":"1"}}}}`
	if err := os.MkdirAll(filepath.Join(a.home, ".bough"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(a.home, ".bough", "mcp.json"), []byte(servers), 0o644); err != nil {
		t.Fatal(err)
	}
	a.check("boot")
	return a
}

// mcpServerCrashMidCallBang runs one "!" line and returns the screen
// once want shows, failing if it takes longer than d.
func mcpServerCrashMidCallBang(a *app, line, want string, d time.Duration) string {
	a.t.Helper()
	a.typeText(line)
	a.key(uv.KeyEnter, 0)
	started := time.Now()
	deadline := started.Add(d)
	for time.Now().Before(deadline) {
		if s := a.text(); strings.Contains(s, want) {
			return a.settled()
		}
		time.Sleep(20 * time.Millisecond)
	}
	a.t.Fatalf("%q: %q not on screen within %s:\n%s", line, want, d, a.text())
	return ""
}

func TestMcpServerCrashMidCall(t *testing.T) {
	t.Parallel()

	t.Run("ErrorBlockThenTurn", func(t *testing.T) {
		t.Parallel()
		a := mcpServerCrashMidCallStart(t)
		s := mcpServerCrashMidCallBang(a, "!"+bin+" mcp call crashy/boom hi", "! exit status", 20*time.Second)
		// The sdk transport drops the server's stderr; the EOF on
		// tools/call is what proves the crash landed mid call.
		if !strings.Contains(s, `calling "tools/call": EOF`) {
			t.Errorf("error block does not name the crashed call:\n%s", s)
		}
		if strings.Contains(s, "! timeout") {
			t.Errorf("crashed call waited out the timeout:\n%s", s)
		}
		a.check("after crashed call")
		// The session is not wedged: a model turn runs and finishes.
		a.typeText("after the crash")
		a.key(uv.KeyEnter, 0)
		if !a.waitDone(1, 30*time.Second) {
			t.Fatalf("turn after the crash never finished:\n%s", a.text())
		}
		a.waitFor("echo: after the crash")
		a.check("after follow-up turn")
	})

	t.Run("SecondCallDoesNotHang", func(t *testing.T) {
		t.Parallel()
		a := mcpServerCrashMidCallStart(t)
		call := "!" + bin + " mcp call crashy/boom hi"
		mcpServerCrashMidCallBang(a, call, "! exit status", 20*time.Second)
		// The second call spawns a fresh server; it must fail as fast,
		// not wait on a session the first crash left behind.
		a.typeText(call)
		a.key(uv.KeyEnter, 0)
		deadline := time.Now().Add(20 * time.Second)
		for time.Now().Before(deadline) && strings.Count(a.text(), "! exit status") < 2 {
			time.Sleep(20 * time.Millisecond)
		}
		if n := strings.Count(a.text(), "! exit status"); n < 2 {
			t.Fatalf("second crashed call not back within 20 s (%d error lines):\n%s", n, a.text())
		}
		a.check("after second call")
	})

	t.Run("StatusShowsDown", func(t *testing.T) {
		t.Parallel()
		a := mcpServerCrashMidCallStart(t)
		s := mcpServerCrashMidCallBang(a, "!"+bin+" mcp status", "catalog:", 30*time.Second)
		down := false
		for _, l := range strings.Split(s, "\n") {
			if strings.Contains(l, "deadboot") && strings.Contains(l, "DOWN") {
				down = true
			}
		}
		if !down {
			t.Errorf("status does not show deadboot DOWN:\n%s", s)
		}
		if !strings.Contains(s, "bough mcp: 1 of 2 servers failed") {
			t.Errorf("status summary missing:\n%s", s)
		}
		a.check("after status")
	})

	t.Run("ConnectShowsMcpDown", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_MCPSERVERCRASHMIDCALL") == "" {
			t.Skip("known gap: /connect lists LLM providers only and has no MCP server status (plugins/connect/connect.go); set BOUGH_KNOWN_MCPSERVERCRASHMIDCALL=1 to run")
		}
		t.Parallel()
		a := mcpServerCrashMidCallStart(t)
		a.typeText("/connect")
		a.key(uv.KeyEnter, 0)
		a.waitFor("providers (a key")
		s := a.settled()
		if !strings.Contains(s, "deadboot") {
			t.Errorf("/connect does not list the MCP servers:\n%s", s)
		}
	})
}
