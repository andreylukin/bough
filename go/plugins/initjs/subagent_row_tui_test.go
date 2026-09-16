package initjs_test

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	_ "github.com/andreylukin/bough/plugins/tools"
	_ "github.com/andreylukin/bough/plugins/workers"
)

// A subagent that finishes shows as done: no row in the view carries
// the failure mark.
func TestSpawnSuccessNoFailRow(t *testing.T) {
	t.Parallel()
	script := `
bough.provider("parrot", function(system, messages) {
	var last = String(messages[messages.length-1].content)
	if (last.indexOf("[subagent 1") >= 0) return "parrot finished"
	if (last === "go") {
		return "spawning:\n` + "```" + `js\nconsole.log(tools.spawn('check the widget'))\n` + "```" + `"
	}
	return "Status: ok\nwidget fine"
})
bough.setup({provider: {default: "parrot"}})
`
	d := mountInit(t, script, "codemode", "commands", "init-js", "loop", "workers")
	d.Say("go")
	d.WaitFor("parrot finished")
	for _, row := range strings.Split(d.Frame(), "\n") {
		if strings.Contains(row, "✗") {
			t.Fatalf("successful spawn rendered a failure row %q:\n%s", row, d.Frame())
		}
	}
}

// A failed bash call run from a long cwd: the closed error row leads
// with what the command said, not the path.
func TestBashFailureRowLeadsWithMessage(t *testing.T) {
	t.Parallel()
	dir := filepath.Join(t.TempDir(), "very", "long", "path", "that", "keeps", "going", "into", "a", "deeply", "nested", "project", "checkout")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	script := `
bough.provider("parrot", function(system, messages) {
	var last = String(messages[messages.length-1].content)
	if (last === "go") {
		return "running:\n` + "```" + `js\ntools.bash('cd ` + dir + ` && echo usage: foo >&2 && exit 1')\n` + "```" + `"
	}
	return "parrot finished"
})
bough.setup({provider: {default: "parrot"}})
`
	d := mountInit(t, script, "codemode", "commands", "init-js", "loop", "tools-basic")
	d.Say("go")
	d.WaitFor("parrot finished")
	for _, row := range strings.Split(d.Frame(), "\n") {
		if strings.Contains(row, "✗") {
			if !strings.Contains(row, "usage: foo") {
				t.Fatalf("error row lost the message %q:\n%s", row, d.Frame())
			}
			return
		}
	}
	t.Fatalf("no error row:\n%s", d.Frame())
}
