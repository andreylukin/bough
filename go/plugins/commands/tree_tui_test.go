package commands_test

// TUI smoke for /undo: a real turn (history row, tools-basic, loop) in
// a temp git repo writes a file; /undo renders the system block and
// the file is gone. Sequential: it chdirs into the repo.

import (
	"context"
	"os"
	"strings"
	"sync"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"

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

// writeThenHang answers the first completion with a write and hangs
// on the next until the turn is cancelled (esc).
type writeThenHang struct {
	mu    sync.Mutex
	calls int
}

func (w *writeThenHang) Complete(ctx context.Context, _ string, _ []llm.Message) (string, error) {
	w.mu.Lock()
	w.calls++
	first := w.calls == 1
	w.mu.Unlock()
	if first {
		return uitest.JS(`tools.write("made.txt", "hello")`), nil
	}
	<-ctx.Done()
	return "", ctx.Err()
}

// A turn cancelled with esc still records what it wrote, so /undo
// right after reverts that turn — not the one before it.
func TestUndoRevertsCancelledTurn(t *testing.T) {
	repo := newRepo(t)
	t.Chdir(repo)
	t.Setenv("HOME", t.TempDir())
	d := uitest.Mount(t, func(c *kernel.Context) { c.Provide("llm", &writeThenHang{}) },
		"history", "codemode", "tools-basic", "commands", "loop")

	d.Say("make a file")
	d.WaitUntil(func(string) bool { _, err := os.Stat("made.txt"); return err == nil }, "made.txt to be written")
	d.Press("esc")
	d.WaitFor("cancelled")

	d.Say("/undo")
	d.WaitFor("reverted 1 file from turn 2")
	if _, err := os.Stat("made.txt"); !os.IsNotExist(err) {
		t.Fatalf("made.txt should be deleted by /undo: %v", err)
	}
}
