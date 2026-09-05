package commands

// /export tests: rendering to a real history.Store, default path
// under a temp HOME, an explicit path argument, and the missing-
// service error.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
)

// mountExport provides store as the history service and mounts the
// commands row over it.
func mountExport(t *testing.T, store *history.Store) *Registry {
	t.Helper()
	ctx := kernel.NewContext()
	ctx.Provide("history", store)
	if err := ctx.Mount([]kernel.Row{{ID: "commands", Plugin: "commands"}}); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(ctx.Unmount)
	reg, err := kernel.Get[*Registry](ctx, "commands")
	if err != nil {
		t.Fatal(err)
	}
	return reg
}

func TestExportWritesMarkdownToDefaultPath(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	store, err := history.Open(filepath.Join(t.TempDir(), "sess.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	store.Append("input", map[string]any{"text": "hello"})
	store.Append("assistant", map[string]any{"text": "hi there"})
	store.Append("code", map[string]any{"text": "console.log(1)"})
	store.Append("result", map[string]any{"text": "1"})

	reg := mountExport(t, store)
	out, err := reg.Run("export", "")
	if err != nil {
		t.Fatalf("/export: %v", err)
	}
	want := filepath.Join(home, ".bough", "exports", "sess.md")
	if out != "exported to "+want {
		t.Fatalf("/export = %q, want %q", out, "exported to "+want)
	}
	b, err := os.ReadFile(want)
	if err != nil {
		t.Fatalf("reading %s: %v", want, err)
	}
	md := string(b)
	for _, want := range []string{"## You", "hello", "## bough", "hi there", "```js", "console.log(1)", "```\n1\n```"} {
		if !strings.Contains(md, want) {
			t.Fatalf("export missing %q in:\n%s", want, md)
		}
	}
}

func TestExportWithExplicitPath(t *testing.T) {
	store, err := history.Open(filepath.Join(t.TempDir(), "sess.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	store.Append("input", map[string]any{"text": "hello"})

	reg := mountExport(t, store)
	dst := filepath.Join(t.TempDir(), "nested", "out.md")
	out, err := reg.Run("export", dst)
	if err != nil {
		t.Fatalf("/export %s: %v", dst, err)
	}
	if out != "exported to "+dst {
		t.Fatalf("/export = %q", out)
	}
	if _, err := os.Stat(dst); err != nil {
		t.Fatalf("stat %s: %v", dst, err)
	}
}

func TestExportNoHistoryService(t *testing.T) {
	ctx := kernel.NewContext()
	if err := ctx.Mount([]kernel.Row{{ID: "commands", Plugin: "commands"}}); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(ctx.Unmount)
	reg, err := kernel.Get[*Registry](ctx, "commands")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := reg.Run("export", ""); err == nil || !strings.Contains(err.Error(), "no history service") {
		t.Fatalf("/export without history = %v", err)
	}
}

// The two properties the rendering exists for: the exchange comes out
// in the order it happened, and the bookkeeping kinds nobody wants to
// read back are left out.
func TestExportKeepsOrderAndDropsMachinery(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	store, err := history.Open(filepath.Join(t.TempDir(), "sess.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	store.Append("meta", map[string]any{"cwd": "/repo"})
	store.Append("input", map[string]any{"text": "FIRST_PROMPT"})
	store.Append("code", map[string]any{"text": `tools.bash("ls")`})
	store.Append("result", map[string]any{"text": "RESULT_TEXT"})
	store.Append("nudge", map[string]any{"text": "NUDGE_TEXT"})
	store.Append("assistant", map[string]any{"text": "SECOND_REPLY"})
	store.Append("done", map[string]any{})
	store.Append("input", map[string]any{"text": "THIRD_PROMPT"})

	reg := mountExport(t, store)
	if _, err := reg.Run("export", ""); err != nil {
		t.Fatal(err)
	}
	files, _ := filepath.Glob(filepath.Join(home, ".bough", "exports", "*.md"))
	if len(files) != 1 {
		t.Fatalf("want one export, got %v", files)
	}
	b, err := os.ReadFile(files[0])
	if err != nil {
		t.Fatal(err)
	}
	md := string(b)

	last := -1
	for _, want := range []string{"FIRST_PROMPT", `tools.bash("ls")`, "RESULT_TEXT", "SECOND_REPLY", "THIRD_PROMPT"} {
		i := strings.Index(md, want)
		if i < 0 {
			t.Fatalf("%q missing from the export:\n%s", want, md)
		}
		if i < last {
			t.Errorf("%q is out of order:\n%s", want, md)
		}
		last = i
	}
	// Session bookkeeping: the reader never asked for it.
	for _, unwanted := range []string{"NUDGE_TEXT", `"cwd"`} {
		if strings.Contains(md, unwanted) {
			t.Errorf("%q is machinery and should not be exported:\n%s", unwanted, md)
		}
	}
}
