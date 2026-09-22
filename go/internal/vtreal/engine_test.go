package vtreal

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"
)

// engineConfig is config with the loop row on engine-unreal and the
// agent-tools row the engine's native tools register into.
const engineConfig = `
- id: llm
  plugin: llm-echo
- id: codemode
  plugin: codemode
- id: commands
  plugin: commands
- id: agent-tools
  plugin: agent-tools
- id: tools
  plugin: tools-basic
- id: history
  plugin: history
- id: loop
  plugin: engine-unreal
- id: ui
  plugin: ui
- id: session-title
  plugin: session-title
  disabled: true
- id: activity
  plugin: activity
  disabled: true
`

// The engine on a real terminal: echo's CODE! is a native bash call,
// its row shows the command, and the reply to its result lands.
func TestEngineCallRowOnRealTerminal(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(engineConfig), 0o644); err != nil {
		t.Fatal(err)
	}
	rows := exec.Command(bin, "rows", "-config", cfg)
	rows.Dir = home
	rows.Env = append(os.Environ(), "HOME="+home)
	out, _ := rows.CombinedOutput()
	if strings.Contains(string(out), "engine-unreal: not built yet") {
		t.Skip("the engine-unreal row is the bootstrap stub")
	}

	a := startCfg(t, 100, 30, engineConfig)
	a.typeText("CODE!")
	a.key(uv.KeyEnter, 0)
	a.waitFor("ran: hi from codemode")
	if s := a.settled(); !strings.Contains(s, "echo hi from codemode") {
		t.Fatalf("no call row naming the command:\n%s", s)
	}
}
