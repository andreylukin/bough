package commands_test

// /undo over a turn that edited through the shell: the path the
// checkpoint diff exists for, end to end. The write tools never saw
// these files, so before the diff the turn recorded nothing and /undo
// had nothing to revert.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/uitest"
	"github.com/andreylukin/bough/kernel"

	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/history"
	_ "github.com/andreylukin/bough/plugins/loop"
	_ "github.com/andreylukin/bough/plugins/tools"
)

func TestUndoRevertsWhatTheShellWrote(t *testing.T) {
	repo := newRepo(t)
	t.Chdir(repo)
	t.Setenv("HOME", t.TempDir())

	// keep.txt comes committed as "orig\n" from newRepo, so the revert
	// has content to put back rather than only a file to delete.
	tracked := filepath.Join(repo, "keep.txt")
	script := &uitest.Script{Replies: []string{
		uitest.JS(`tools.bash("printf 'after\n' > keep.txt")`),
		"edited it",
	}}
	d := uitest.Mount(t, func(c *kernel.Context) { c.Provide("llm", script) },
		"history", "codemode", "tools-basic", "commands", "loop")

	start := time.Now()
	d.Say("edit through the shell")
	// The done row, not the reply: the turn ends after the checkpoint
	// diff runs, and /undo refuses while a turn is still running.
	// Waiting on the assistant's text races that by the width of the
	// diff.
	d.WaitFor("wrote keep.txt")
	if got, _ := os.ReadFile(tracked); string(got) != "after\n" {
		t.Fatalf("the shell edit did not land: %q", got)
	}

	// The turn must end promptly: the diff runs at the end of every
	// shell turn, and /undo refuses while a turn is still running.
	d.Say("/undo")
	d.WaitFor("reverted 1 file from turn 2")
	if d := time.Since(start); d > 20*time.Second {
		t.Errorf("shell turn plus undo took %s", d)
	}
	if !strings.Contains(d.Frame(), "keep.txt") {
		t.Fatalf("the system block should name the file:\n%s", d.Frame())
	}
	if got, _ := os.ReadFile(tracked); string(got) != "orig\n" {
		t.Errorf("undo left %q, want the checkpoint's content", got)
	}
}
