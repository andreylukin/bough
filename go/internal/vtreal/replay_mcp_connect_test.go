package vtreal

// /connect and MCP with no servers configured, on the real binary in a
// fresh $HOME: the empty state says so, a bogus /connect argument is an
// error block within 5 s, and the ui keeps answering afterwards.
//
// There is no mcpStatus host function in the Go tree; the MCP status
// surface is `bough mcp status` and the "mcp" prompt section, so those
// are what the empty-state checks read.

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// mcpConnectWait polls the screen for substr for at most d.
func (a *app) mcpConnectWait(substr string, d time.Duration) bool {
	deadline := time.Now().Add(d)
	for time.Now().Before(deadline) {
		if strings.Contains(a.text(), substr) {
			return true
		}
		time.Sleep(20 * time.Millisecond)
	}
	return false
}

func TestMcpConnectStatusEmpty(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, bin, "mcp", "status")
	cmd.Dir = home
	cmd.Env = append(os.Environ(), "HOME="+home)
	out, err := cmd.CombinedOutput()
	if ctx.Err() != nil {
		t.Fatalf("bough mcp status hung past 5 s:\n%s", out)
	}
	if err == nil {
		t.Errorf("bough mcp status with no servers exited 0:\n%s", out)
	}
	if !strings.Contains(string(out), "no MCP servers configured") {
		t.Errorf("empty-state text missing:\n%s", out)
	}
}

func TestMcpConnectPromptEmpty(t *testing.T) {
	t.Parallel()
	a := start(t, 100, 30)
	a.typeText("SYSTEM!")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 30*time.Second) {
		t.Fatalf("SYSTEM! turn never finished:\n%s", a.text())
	}
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var system string
	for _, p := range paths {
		entries, _ := history.Read(p)
		for _, e := range entries {
			if s, ok := e.Data["text"].(string); ok && e.Kind == "assistant" {
				system += s
			}
		}
	}
	if system == "" {
		t.Fatalf("no assistant entry carrying the prompt:\n%s", a.text())
	}
	if strings.Contains(system, "MCP servers are reachable") {
		t.Errorf("mcp prompt section present with no servers configured:\n%s", a.text())
	}
	a.check("after SYSTEM!")
}

func TestMcpConnectList(t *testing.T) {
	t.Parallel()
	a := start(t, 100, 30)
	a.typeText("/connect")
	a.key(uv.KeyEnter, 0)
	if !a.mcpConnectWait("providers (a key", 5*time.Second) {
		t.Fatalf("/connect listing not shown within 5 s:\n%s", a.text())
	}
	a.check("after /connect")
}

func TestMcpConnectBogus(t *testing.T) {
	t.Parallel()
	a := start(t, 100, 30)
	a.typeText("/connect bogus-provider sk-test")
	a.key(uv.KeyEnter, 0)
	if !a.mcpConnectWait(`unknown provider "bogus-provider"`, 5*time.Second) {
		t.Fatalf("/connect error not shown within 5 s:\n%s", a.text())
	}
	if strings.Contains(a.text(), "sk-test") {
		t.Errorf("the key argument was echoed:\n%s", a.text())
	}
	if _, err := os.Stat(filepath.Join(a.home, ".bough", "env")); err == nil {
		t.Errorf("a bogus provider wrote ~/.bough/env:\n%s", a.text())
	}
	a.check("after /connect bogus")
	// The ui is not hung: the composer takes a turn and answers it.
	a.typeText("still here")
	a.key(uv.KeyEnter, 0)
	if !a.mcpConnectWait("echo: still here", 10*time.Second) {
		t.Fatalf("composer did not come back after /connect error:\n%s", a.text())
	}
	a.check("after follow-up turn")
}
