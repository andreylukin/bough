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

func TestBashTimeoutMessage(t *testing.T) {
	saved := bashTimeout
	bashTimeout = 200 * time.Millisecond
	t.Cleanup(func() { bashTimeout = saved })
	st := &Stats{}
	_, err := st.bash("sleep 5")
	if err == nil {
		t.Fatal("want error")
	}
	if !strings.HasPrefix(err.Error(), "bash: killed after 200ms: sleep 5") {
		t.Errorf("timeout message = %q", err.Error())
	}
	if _, exit, ran := st.Take(); !ran || exit != -1 {
		t.Errorf("exit = %d ran = %v, want -1 true", exit, ran)
	}
}

func TestTurnStats(t *testing.T) {
	ctx := kernel.NewContext()
	ctx.Provide("codemode", codemode.New(5*time.Second))
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	cm, _ := kernel.Get[*codemode.CodeMode](ctx, "codemode")
	st, err := kernel.Get[*Stats](ctx, "turn-stats")
	if err != nil {
		t.Fatal(err)
	}
	if files, _, ran := st.Take(); len(files) != 0 || ran {
		t.Fatalf("fresh stats = %v %v", files, ran)
	}
	path := filepath.Join(t.TempDir(), "f.txt")
	if _, err := cm.Run(`tools.writeFile(` + jsStr(path) + `, "x"); try { tools.bash("exit 3") } catch (e) {}`); err != nil {
		t.Fatal(err)
	}
	files, exit, ran := st.Take()
	if len(files) != 1 || files[0] != path || exit != 3 || !ran {
		t.Errorf("Take = %v %d %v", files, exit, ran)
	}
	if files, _, ran := st.Take(); len(files) != 0 || ran {
		t.Errorf("Take did not reset: %v %v", files, ran)
	}
}

func jsStr(s string) string {
	return `"` + strings.ReplaceAll(s, `"`, `\"`) + `"`
}
