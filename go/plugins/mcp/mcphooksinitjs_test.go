package mcp

import (
	"context"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"

	sdk "github.com/modelcontextprotocol/go-sdk/mcp"
)

// mcphooksinitjsServer is a fake stdio MCP server: a shell script that
// answers initialize and tools/list, then misbehaves on tools/call per
// $1. It writes its pid to $2 so tests can check it is reaped.
const mcphooksinitjsServer = `#!/bin/sh
MODE=$1
echo $$ > "$2"
case $MODE in
  die) exit 3 ;;
  garbage) echo "hello, not json" ;;
esac
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$2.log"
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
  *'"method":"initialize"'*)
    [ "$MODE" = noinit ] && continue
    printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1"}}}\n' "$id" ;;
  *'"method":"tools/list"'*)
    printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"t","description":"fake tool","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
  *'"method":"tools/call"'*)
    case $MODE in
    exit) exit 7 ;;
    rpcerr) printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32000,"message":"FAKE_RPC_FAIL"}}\n' "$id" ;;
    malformed) printf '{"jsonrpc":"2.0","id":%s,"result":{not json\n' "$id" ;;
    iserror) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"TOOL_SAID_NO"}],"isError":true}}\n' "$id" ;;
    hang) : ;;
    *) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"FAKE_OK"}]}}\n' "$id" ;;
    esac ;;
  *)
    # An older server: unknown requests (server/discover) are
    # method-not-found, as a real one answers them.
    [ -n "$id" ] && printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"Method not found"}}\n' "$id" ;;
  esac
done
`

// mcphooksinitjsFake writes the fake server and returns its config and
// the pid file it writes.
func mcphooksinitjsFake(t *testing.T, mode string) (ServerConfig, string) {
	t.Helper()
	dir := t.TempDir()
	script := filepath.Join(dir, "fake.sh")
	if err := os.WriteFile(script, []byte(mcphooksinitjsServer), 0o755); err != nil {
		t.Fatal(err)
	}
	pidf := filepath.Join(dir, "pid")
	return ServerConfig{Command: "/bin/sh", Args: []string{script, mode, pidf}}, pidf
}

// mcphooksinitjsReaped fails if the fake server process outlives its
// session by more than a few seconds.
func mcphooksinitjsReaped(t *testing.T, pidf string) {
	t.Helper()
	data, err := os.ReadFile(pidf)
	if err != nil {
		return // never started
	}
	pid, _ := strconv.Atoi(strings.TrimSpace(string(data)))
	deadline := time.Now().Add(8 * time.Second)
	for time.Now().Before(deadline) {
		if syscall.Kill(pid, 0) != nil {
			return
		}
		time.Sleep(50 * time.Millisecond)
	}
	_ = syscall.Kill(pid, syscall.SIGKILL)
	t.Errorf("fake MCP server pid %d still alive after its session closed", pid)
}

// mcphooksinitjsBounded runs fn and fails if it takes longer than d.
func mcphooksinitjsBounded(t *testing.T, d time.Duration, fn func()) {
	t.Helper()
	done := make(chan struct{})
	go func() { fn(); close(done) }()
	select {
	case <-done:
	case <-time.After(d):
		t.Fatalf("did not return within %v", d)
	}
}

// Servers that fail before or during the handshake: connect errors, in
// bounded time, and no process is left behind.
func TestMcpHooksInitjsConnectFailures(t *testing.T) {
	t.Run("missing binary", func(t *testing.T) {
		_, err := connect(ServerConfig{Command: filepath.Join(t.TempDir(), "nope")})
		if err == nil {
			t.Fatal("connect to a missing binary succeeded")
		}
	})
	for _, mode := range []string{"die", "garbage"} {
		t.Run(mode, func(t *testing.T) {
			sc, pidf := mcphooksinitjsFake(t, mode)
			var err error
			var s *sdk.ClientSession
			mcphooksinitjsBounded(t, 15*time.Second, func() { s, err = connect(sc) })
			if mode == "die" && err == nil {
				s.Close()
				t.Fatal("connect to a dying server succeeded")
			}
			if s != nil {
				s.Close()
			}
			mcphooksinitjsReaped(t, pidf)
		})
	}
	t.Run("never answers initialize", func(t *testing.T) {
		if os.Getenv("BOUGH_SOAK_MCP_HOOKS_INITJS") != "1" {
			t.Skip("waits the 10s connect timeout; BOUGH_SOAK_MCP_HOOKS_INITJS=1")
		}
		sc, pidf := mcphooksinitjsFake(t, "noinit")
		var err error
		mcphooksinitjsBounded(t, connectTimeout+5*time.Second, func() { _, err = connect(sc) })
		if err == nil {
			t.Fatal("connect succeeded without an initialize reply")
		}
		mcphooksinitjsReaped(t, pidf)
	})
}

// Failures during tools/call: each is an error naming what went wrong,
// returned promptly, with the server reaped after Close.
func TestMcpHooksInitjsCallFailures(t *testing.T) {
	cases := []struct {
		mode, want string
		within     time.Duration
	}{
		{"ok", "", 10 * time.Second},
		{"rpcerr", "FAKE_RPC_FAIL", 10 * time.Second},
		{"iserror", "TOOL_SAID_NO", 10 * time.Second},
		{"exit", "", 10 * time.Second},
		{"malformed", "", 10 * time.Second},
	}
	for _, tc := range cases {
		t.Run(tc.mode, func(t *testing.T) {
			sc, pidf := mcphooksinitjsFake(t, tc.mode)
			s, err := connect(sc)
			if err != nil {
				seen, _ := os.ReadFile(pidf + ".log")
				t.Fatalf("connect: %v\nserver saw:\n%s", err, seen)
			}
			var out string
			mcphooksinitjsBounded(t, tc.within, func() { out, err = callOn(s, "t", "x") })
			s.Close()
			if tc.mode == "ok" {
				if err != nil || out != "FAKE_OK" {
					t.Fatalf("ok call = (%q, %v)", out, err)
				}
			} else if err == nil {
				t.Fatalf("%s: call succeeded with %q", tc.mode, out)
			} else if !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("%s: err %q lacks %q", tc.mode, err, tc.want)
			}
			mcphooksinitjsReaped(t, pidf)
		})
	}
	t.Run("hang", func(t *testing.T) {
		if os.Getenv("BOUGH_SOAK_MCP_HOOKS_INITJS") != "1" {
			t.Skip("waits the 60s call timeout; BOUGH_SOAK_MCP_HOOKS_INITJS=1")
		}
		sc, pidf := mcphooksinitjsFake(t, "hang")
		s, err := connect(sc)
		if err != nil {
			t.Fatal(err)
		}
		mcphooksinitjsBounded(t, callTimeout+10*time.Second, func() { _, err = callOn(s, "t", "x") })
		s.Close()
		if err == nil {
			t.Fatal("a call that never answers succeeded")
		}
		mcphooksinitjsReaped(t, pidf)
	})
}

// The CLI path the model uses (bough mcp call via Call): a failing
// server is an error, not a hang or a panic.
func TestMcpHooksInitjsCLICall(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Chdir(t.TempDir())
	sc, _ := mcphooksinitjsFake(t, "rpcerr")
	dead, _ := mcphooksinitjsFake(t, "die")
	cfg := `{"servers":{"bad":{"command":"/bin/sh","args":["` + sc.Args[0] + `","rpcerr","` + filepath.Join(home, "p1") +
		`"]},"dead":{"command":"/bin/sh","args":["` + dead.Args[0] + `","die","` + filepath.Join(home, "p2") + `"]}}}`
	if err := os.MkdirAll(filepath.Join(home, ".bough"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(home, ".bough", "mcp.json"), []byte(cfg), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := Call("bad", "t", "x"); err == nil || !strings.Contains(err.Error(), "FAKE_RPC_FAIL") {
		t.Fatalf("Call bad = %v", err)
	}
	if _, err := Call("dead", "t", "x"); err == nil || !strings.Contains(err.Error(), "connect") {
		t.Fatalf("Call dead = %v", err)
	}
	if err := runCLI(nil, []string{"call", "nosuch/t"}); err == nil {
		t.Fatal("call on an unknown server succeeded")
	}
	if err := runCLI(nil, []string{"call", "bad"}); err == nil {
		t.Fatal("call without /tool succeeded")
	}
	if err := runCLI(nil, []string{"status"}); err == nil || !strings.Contains(err.Error(), "1 of 2") {
		t.Fatalf("status with one dead server = %v", err)
	}
}

// Arguments failing the tool's input schema, and a tool list that
// changes mid-session.
func TestMcpHooksInitjsSchemaAndListChange(t *testing.T) {
	server := sdk.NewServer(&sdk.Implementation{Name: "test", Version: "0.1"}, nil)
	type args struct {
		N int `json:"n"`
	}
	sdk.AddTool(server, &sdk.Tool{Name: "num", Description: "takes n"},
		func(_ context.Context, _ *sdk.CallToolRequest, a args) (*sdk.CallToolResult, any, error) {
			return &sdk.CallToolResult{Content: []sdk.Content{&sdk.TextContent{Text: "n=" + strconv.Itoa(a.N)}}}, nil, nil
		})
	ct, st := sdk.NewInMemoryTransports()
	ss, err := server.Connect(t.Context(), st, nil)
	if err != nil {
		t.Fatal(err)
	}
	defer ss.Close()
	cs, err := sdk.NewClient(&sdk.Implementation{Name: "c", Version: "0.1"}, nil).Connect(t.Context(), ct, nil)
	if err != nil {
		t.Fatal(err)
	}
	defer cs.Close()

	if out, err := callOn(cs, "num", `{"n": 3}`); err != nil || out != "n=3" {
		t.Fatalf("good call = (%q, %v)", out, err)
	}
	if out, err := callOn(cs, "num", `{"n": "three"}`); err == nil {
		t.Fatalf("schema-violating args succeeded: %q", out)
	}
	if out, err := callOn(cs, "num", "plain text"); err == nil {
		t.Fatalf("plain text into an int field succeeded: %q", out)
	}

	server.RemoveTools("num")
	if out, err := callOn(cs, "num", `{"n": 1}`); err == nil {
		t.Fatalf("call to a removed tool succeeded: %q", out)
	}
	sdk.AddTool(server, &sdk.Tool{Name: "fresh", Description: "new"},
		func(_ context.Context, _ *sdk.CallToolRequest, _ struct{}) (*sdk.CallToolResult, any, error) {
			return &sdk.CallToolResult{Content: []sdk.Content{&sdk.TextContent{Text: "FRESH"}}}, nil, nil
		})
	tools, err := listTools(cs)
	if err != nil || len(tools) != 1 || tools[0].Name != "fresh" {
		t.Fatalf("listTools after change = %v, %v", tools, err)
	}
	if out, err := callOn(cs, "fresh", ""); err != nil || out != "FRESH" {
		t.Fatalf("new tool = (%q, %v)", out, err)
	}
}
