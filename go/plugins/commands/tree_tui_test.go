package commands_test

// TUI smoke for /undo: a real turn (history row, tools-basic, loop) in
// a temp git repo writes a file; /undo renders the system block and
// the file is gone. Sequential: it chdirs into the repo.

import (
	"os"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"
	"github.com/andreylukin/bough/kernel"

	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/history"
	_ "github.com/andreylukin/bough/plugins/loop"
	_ "github.com/andreylukin/bough/plugins/tools"
)

func TestUndoRendersSystemBlock(t *testing.T) {
	repo := newRepo(t)
	t.Chdir(repo)
	t.Setenv("HOME", t.TempDir()) // the history row's fresh session file
	script := &uitest.Script{Replies: []string{uitest.JS(`tools.write("made.txt", "hello")`), "wrote it"}}
	d := uitest.Mount(t, func(c *kernel.Context) { c.Provide("llm", script) },
		"history", "codemode", "tools-basic", "commands", "loop")

	d.Say("make a file")
	d.WaitFor("wrote it")
	if _, err := os.Stat("made.txt"); err != nil {
		t.Fatalf("the turn should have written made.txt: %v", err)
	}

	d.Say("/undo")
	d.WaitFor("reverted 1 file from turn 2")
	if !strings.Contains(d.Frame(), "made.txt") {
		t.Fatalf("system block should name the file:\n%s", d.Frame())
	}
	if _, err := os.Stat("made.txt"); !os.IsNotExist(err) {
		t.Fatalf("made.txt should be deleted by /undo: %v", err)
	}
}
