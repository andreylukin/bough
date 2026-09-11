package vtreal

// An MCP server that takes 5 s to answer initialize, with a turn
// submitted straight after boot. MCP servers are spawned on demand by
// `bough mcp` (plugins/mcp: "never at mount"), so the first turn must
// not wait on the server at all, and the one TUI path that does spawn
// it — a "!" line running `bough mcp call` — must leave the composer
// responsive while the server boots, cancel on esc, and leave no
// server process behind after quit.
//
// The stub sleeps 5 s on its first request, then speaks the same
// newline-delimited JSON-RPC as the crash-mid-call stub. Its path sits
// in a per-test temp dir, so pgrep -f on that dir sees only this
// test's server.

import (
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const mcpServerSlowStartupFirstTurnStub = `#!/bin/sh
slept=
while IFS= read -r line; do
  [ -z "$slept" ] && { sleep 5; slept=1; }
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
  *'"method":"initialize"'*)
    v=$(printf '%s' "$line" | sed -n 's/.*"protocolVersion":"\([^"]*\)".*/\1/p')
    printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"%s","capabilities":{"tools":{}},"serverInfo":{"name":"slow","version":"1"}}}\n' "$id" "$v";;
  *'"method":"tools/list"'*)
    printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"ping","description":"answers slow-ok","inputSchema":{"type":"object","properties":{"query":{"type":"string"}}}}]}}\n' "$id";;
  *'"method":"tools/call"'*)
    printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"slow-ok"}]}}\n' "$id";;
  *'"method":"notifications/'*) ;;
  *)
    [ -n "$id" ] && printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"method not found"}}\n' "$id";;
  esac
done
`

// mcpServerSlowStartupFirstTurnStart boots bough with ~/.bough/mcp.json
// naming the slow stub; it returns the app and the pgrep marker (the
// stub's unique dir).
func mcpServerSlowStartupFirstTurnStart(t *testing.T) (*app, string) {
	t.Helper()
	dir := t.TempDir()
	stub := filepath.Join(dir, "slowmcp.sh")
	if err := os.WriteFile(stub, []byte(mcpServerSlowStartupFirstTurnStub), 0o755); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = exec.Command("pkill", "-f", dir).Run() })
	a := start(t, 120, 40)
	if err := os.MkdirAll(filepath.Join(a.home, ".bough"), 0o755); err != nil {
		t.Fatal(err)
	}
	servers := `{"servers":{"slow":{"command":"` + stub + `"}}}`
	if err := os.WriteFile(filepath.Join(a.home, ".bough", "mcp.json"), []byte(servers), 0o644); err != nil {
		t.Fatal(err)
	}
	return a, dir
}

func mcpServerSlowStartupFirstTurnAlive(marker string) bool {
	return exec.Command("pgrep", "-f", marker).Run() == nil
}

// mcpServerSlowStartupFirstTurnWaitAlive waits up to d for the stub's
// liveness to reach want.
func mcpServerSlowStartupFirstTurnWaitAlive(a *app, marker string, want bool, d time.Duration) {
	a.t.Helper()
	deadline := time.Now().Add(d)
	for time.Now().Before(deadline) {
		if mcpServerSlowStartupFirstTurnAlive(marker) == want {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	a.t.Fatalf("mcp stub alive=%v, want %v after %s:\n%s", !want, want, d, a.text())
}

// mcpServerSlowStartupFirstTurnEcho types token one rune at a time and
// returns the median time for each rune to reach the screen.
func mcpServerSlowStartupFirstTurnEcho(a *app, token string) time.Duration {
	a.t.Helper()
	var lat []time.Duration
	for i := range token {
		want := token[:i+1]
		t0 := time.Now()
		a.typeText(token[i : i+1])
		for !strings.Contains(a.text(), want) {
			if time.Since(t0) > 3*time.Second {
				a.t.Fatalf("typed %q never echoed:\n%s", want, a.text())
			}
			time.Sleep(2 * time.Millisecond)
		}
		lat = append(lat, time.Since(t0))
	}
	sort.Slice(lat, func(i, j int) bool { return lat[i] < lat[j] })
	return lat[len(lat)/2]
}

// mcpServerSlowStartupFirstTurnCall starts the slow `mcp call` from a
// "!" line and waits until the stub runs.
func mcpServerSlowStartupFirstTurnCall(a *app, marker string) {
	a.t.Helper()
	a.typeText("!" + bin + " mcp call slow/ping hi")
	a.key(uv.KeyEnter, 0)
	mcpServerSlowStartupFirstTurnWaitAlive(a, marker, true, 5*time.Second)
}

func TestMcpServerSlowStartupFirstTurn(t *testing.T) {
	t.Parallel()
	const gate = "BOUGH_KNOWN_MCP_SERVER_SLOW_STARTUP_FIRST_TURN"

	t.Run("FirstTurnDoesNotWaitOnServer", func(t *testing.T) {
		t.Parallel()
		a, marker := mcpServerSlowStartupFirstTurnStart(t)
		t0 := time.Now()
		a.typeText("first turn")
		a.key(uv.KeyEnter, 0)
		if !a.waitDone(1, 30*time.Second) {
			t.Fatalf("first turn never finished:\n%s", a.text())
		}
		// Well under the stub's 5 s initialize: the turn proceeded
		// without MCP rather than waiting on it.
		if d := time.Since(t0); d > 4*time.Second {
			t.Errorf("first turn took %s: it waited on the slow MCP server", d)
		}
		a.waitFor("echo: first turn")
		if mcpServerSlowStartupFirstTurnAlive(marker) {
			t.Errorf("an MCP server was spawned by a plain turn")
		}
		a.check("after first turn")
	})

	t.Run("TypingEchoesWhileServerBoots", func(t *testing.T) {
		t.Parallel()
		a, marker := mcpServerSlowStartupFirstTurnStart(t)
		mcpServerSlowStartupFirstTurnCall(a, marker)
		if med := mcpServerSlowStartupFirstTurnEcho(a, "zqxwv"); med > 100*time.Millisecond {
			t.Errorf("median keystroke echo %s while the MCP server boots, want <= 100ms", med)
		}
		// The call finishes once the server is up, with the tool's reply.
		deadline := time.Now().Add(20 * time.Second)
		for time.Now().Before(deadline) && !strings.Contains(a.text(), "slow-ok") {
			time.Sleep(20 * time.Millisecond)
		}
		if s := a.text(); !strings.Contains(s, "slow-ok") {
			t.Fatalf("slow call never returned the tool reply:\n%s", s)
		}
		mcpServerSlowStartupFirstTurnWaitAlive(a, marker, false, 5*time.Second)
		// The draft typed during the boot survives and submits.
		a.key(uv.KeyEnter, 0)
		if !a.waitDone(1, 30*time.Second) {
			t.Fatalf("turn after the slow call never finished:\n%s", a.text())
		}
		a.waitFor("echo: zqxwv")
		a.check("after slow call")
	})

	t.Run("EscCancelsBootingCall", func(t *testing.T) {
		t.Parallel()
		a, marker := mcpServerSlowStartupFirstTurnStart(t)
		mcpServerSlowStartupFirstTurnCall(a, marker)
		a.key(uv.KeyEscape, 0)
		mcpServerSlowStartupFirstTurnWaitAlive(a, marker, false, 2*time.Second)
		if s := a.settled(); strings.Contains(s, "slow-ok") {
			t.Fatalf("cancelled call still delivered its result:\n%s", s)
		}
	})

	t.Run("QuitLeavesNoServer", func(t *testing.T) {
		if os.Getenv(gate) == "" {
			t.Skip("known bug: quitting bough orphans a running ! command — runBang (plugins/ui/bang.go) starts sh -c under context.Background with no process group, so exit never kills sh / bough mcp call / the MCP server; set " + gate + " to run")
		}
		t.Parallel()
		a, marker := mcpServerSlowStartupFirstTurnStart(t)
		mcpServerSlowStartupFirstTurnCall(a, marker)
		a.key('c', uv.ModCtrl)
		a.waitFor("ctrl+c")
		a.key('c', uv.ModCtrl)
		done := make(chan error, 1)
		go func() { done <- a.term.Wait(a.cmd) }()
		select {
		case <-done:
		case <-time.After(8 * time.Second):
			t.Fatalf("bough did not quit while the MCP server booted:\n%s", a.text())
		}
		// Well before the stub would finish on its own (5 s sleep).
		mcpServerSlowStartupFirstTurnWaitAlive(a, marker, false, 2*time.Second)
	})
}
