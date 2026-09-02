package tools

import (
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
)

func TestToolsViaCodemode(t *testing.T) {
	ctx := kernel.NewContext()
	ctx.Provide("codemode", codemode.New(5*time.Second))
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	cm, err := kernel.Get[*codemode.CodeMode](ctx, "codemode")
	if err != nil {
		t.Fatal(err)
	}

	out, err := cm.Run(`tools.bash("echo hi")`)
	if err != nil {
		t.Fatalf("bash: %v", err)
	}
	if !strings.Contains(out, "hi") {
		t.Errorf("bash output: %q", out)
	}

	path := filepath.Join(t.TempDir(), "f.txt")
	code := `tools.writeFile(` + jsStr(path) + `, "abc"); tools.readFile(` + jsStr(path) + `)`
	out, err = cm.Run(code)
	if err != nil {
		t.Fatalf("write/read: %v", err)
	}
	if out != "abc" {
		t.Errorf("readFile: %q", out)
	}
}

func jsStr(s string) string {
	return `"` + strings.ReplaceAll(s, `"`, `\"`) + `"`
}
