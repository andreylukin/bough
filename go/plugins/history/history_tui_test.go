// TUI-integration tests: a real history.Store (own temp JSONL per
// test) provided as the "history" service, read live by the real ui
// model's status bar and inspector overlay.
package history_test

import (
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"

	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/llm"
	_ "github.com/andreylukin/bough/plugins/loop"
)

func mountWithStore(t *testing.T, name string) *uitest.Driver {
	t.Helper()
	store, err := history.Open(filepath.Join(t.TempDir(), name))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	return uitest.Mount(t, func(c *kernel.Context) { c.Provide("history", store) },
		"codemode", "llm-echo", "loop")
}

// The inspector's entry list grows turn over turn: the loop appends
// to the real store and the renderer reads it live (the status bar no
// longer counts entries; /sessions and the inspector own that).
func TestInspectorEntryCountAcrossTurns(t *testing.T) {
	t.Parallel()
	d := mountWithStore(t, "session-a.jsonl")
	d.Say("first")
	d.WaitFor("echo: first")
	d.Press("ctrl+o")
	d.WaitFor("   3 ") // input + assistant + done
	d.Press("ctrl+o")
	d.Say("second")
	d.WaitFor("echo: second")
	d.Press("ctrl+o")
	d.WaitFor("   6 ")
}

// The inspector overlay lists the real entries and closes again.
func TestInspectorOverlayListsEntries(t *testing.T) {
	t.Parallel()
	d := mountWithStore(t, "session-b.jsonl")
	d.Say("overlay fodder")
	d.WaitFor("echo: overlay fodder")
	d.Press("ctrl+o")
	frame := d.Frame()
	// The full store path in the overlay header is clipped at the
	// viewport width (temp paths are long), so assert the stable parts.
	for _, want := range []string{"inspecting", "history", "input", "assistant"} {
		if !strings.Contains(frame, want) {
			t.Fatalf("overlay missing %q:\n%s", want, frame)
		}
	}
	d.Press("ctrl+o")
	if strings.Contains(d.Frame(), "inspecting") {
		t.Fatalf("overlay did not close:\n%s", d.Frame())
	}
}
