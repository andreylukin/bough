// TUI-integration tests: real kernel + init-js + codemode + loop + the
// real ui model. TestMain sandboxes HOME; init-js reads the fixed path
// $HOME/.bough/init.js only during Apply, so parallel tests serialize
// just the write+mount window on a package mutex and then run
// concurrently.
package initjs_test

import (
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"

	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/llm"
	_ "github.com/andreylukin/bough/plugins/loop"
)

func TestMain(m *testing.M) {
	home, err := os.MkdirTemp("", "bough-initjs-tui-home-*")
	if err != nil {
		panic(err)
	}
	os.Setenv("HOME", home)
	code := m.Run()
	os.RemoveAll(home)
	os.Exit(code)
}

var initMu sync.Mutex

// mountInit writes $HOME/.bough/init.js, mounts the tree while it is
// in place, then removes it — all under the package mutex.
func mountInit(t *testing.T, script string, plugins ...string) *uitest.Driver {
	t.Helper()
	initMu.Lock()
	defer initMu.Unlock()
	home, err := os.UserHomeDir()
	if err != nil {
		t.Fatal(err)
	}
	dir := filepath.Join(home, ".bough")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(dir, "init.js")
	if err := os.WriteFile(path, []byte(script), 0o644); err != nil {
		t.Fatal(err)
	}
	defer os.Remove(path)
	return uitest.Mount(t, nil, plugins...)
}

// A theme override from init.js restyles the rendered frame: the user
// block carries the configured truecolor escape.
func TestThemeAccentChangesFrame(t *testing.T) {
	t.Parallel()
	d := mountInit(t, `bough.setup({ui: {theme: {user: "#ff0000:bold"}}})`,
		"codemode", "commands", "llm-echo", "init-js", "loop")
	d.Say("styled line")
	d.WaitFor("echo: styled line")
	raw := d.RawFrame()
	if !strings.Contains(raw, "38;2;255;0;0") {
		t.Fatalf("themed user color missing from raw frame:\n%q", raw)
	}
}

// A rebound quit key works through the real keymap service.
func TestReboundQuitKey(t *testing.T) {
	t.Parallel()
	d := mountInit(t, `bough.setup({ui: {keymap: {quit: "ctrl+q"}}})`,
		"codemode", "commands", "llm-echo", "init-js", "loop")
	d.Press("ctrl+q")
	d.WaitQuit()
}

// A JS provider (bough.provider + setup.provider.default) drives the
// loop, and a JS-registered tool's result block renders.
func TestJSProviderAndToolRender(t *testing.T) {
	t.Parallel()
	script := `
bough.tool("marker", function() { return "JSTOOL_RESULT_77" })
var calls = 0
bough.provider("parrot", function(system, messages) {
	calls++
	if (calls === 1) {
		return "running the tool:\n` + "```" + `js\nconsole.log(tools.marker())\n` + "```" + `"
	}
	return "parrot finished"
})
bough.setup({provider: {default: "parrot"}})
`
	d := mountInit(t, script, "codemode", "commands", "init-js", "loop")
	d.Say("go")
	d.WaitFor("parrot finished")
	d.Press("tab", "tab", "enter") // focus past code to result, expand it
	frame := d.Frame()
	if !strings.Contains(frame, "JSTOOL_RESULT_77") {
		t.Fatalf("JS tool result missing:\n%s", frame)
	}
	if !strings.Contains(frame, "▾ result") {
		t.Fatalf("result block missing:\n%s", frame)
	}
}
