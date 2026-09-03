// TUI-integration tests: real kernel + commands + prompts + loop +
// llm-echo + the real ui model. TestMain sandboxes HOME
// ($HOME/.bough/prompts is a scanned dir); parallel tests each own a
// uniquely named template.
package prompts_test

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"

	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/commands"
	_ "github.com/andreylukin/bough/plugins/llm"
	_ "github.com/andreylukin/bough/plugins/loop"
	_ "github.com/andreylukin/bough/plugins/prompts"
)

func TestMain(m *testing.M) {
	home, err := os.MkdirTemp("", "bough-prompts-tui-home-*")
	if err != nil {
		panic(err)
	}
	os.Setenv("HOME", home)
	code := m.Run()
	os.RemoveAll(home)
	os.Exit(code)
}

// writeTemplate installs $HOME/.bough/prompts/<name>.md.
func writeTemplate(t *testing.T, name, body string) {
	t.Helper()
	home, err := os.UserHomeDir()
	if err != nil {
		t.Fatal(err)
	}
	dir := filepath.Join(home, ".bough", "prompts")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, name+".md"), []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

// "/name args" submits the expanded template as the user prompt: the
// user block and the echoed reply carry the expansion, never the "/"
// line.
func TestTemplateSubmitsExpansion(t *testing.T) {
	t.Parallel()
	writeTemplate(t, "zzgreet", "say hello to $ARGUMENTS, starting with $1\n")
	d := uitest.Mount(t, nil, "codemode", "llm-echo", "commands", "prompts", "loop")
	d.Type("/zzgreet")
	if f := d.Frame(); !strings.Contains(f, "template: say hello to $ARGUMENTS") {
		t.Fatalf("palette should list the template with its first line:\n%s", f)
	}
	d.Type(" bob smith")
	d.Press("enter")
	d.WaitFor("echo: say hello to bob smith, starting with bob")
	f := d.Frame()
	if !strings.Contains(f, "❯ say hello to bob smith, starting with bob") {
		t.Fatalf("user block should show the expansion:\n%s", f)
	}
	if strings.Contains(f, "echo: /zzgreet") {
		t.Fatalf("the / line must not reach the loop:\n%s", f)
	}
}

// The startup header names the loaded templates.
func TestTemplateInStartupHeader(t *testing.T) {
	t.Parallel()
	writeTemplate(t, "zzheader", "body")
	d := uitest.Mount(t, nil, "codemode", "llm-echo", "commands", "prompts", "loop")
	if f := d.Frame(); !strings.Contains(f, "templates: ") || !strings.Contains(f, "/zzheader") {
		t.Fatalf("header should list /zzheader:\n%s", f)
	}
}
