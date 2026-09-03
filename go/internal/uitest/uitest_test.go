package uitest_test

import (
	"strings"
	"sync/atomic"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"

	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/llm"
	_ "github.com/andreylukin/bough/plugins/loop"
)

// Harness smoke: a real echo turn travels input chan -> loop -> llm ->
// loop/event -> model -> rendered frame.
func TestHarnessEchoTurn(t *testing.T) {
	t.Parallel()
	d := uitest.Mount(t, nil, "codemode", "llm-echo", "loop")
	d.Say("hello harness")
	d.WaitFor("echo: hello harness")
	if !strings.Contains(d.Frame(), "❯ hello harness") {
		t.Fatalf("user line not rendered:\n%s", d.Frame())
	}
}

// A real turn through the loop renders in emission order: the prose
// before a fence, then the code and result rows, then the reply that
// followed the result — never the whole reply above the tool rows it
// triggered. Prose the model wrote under the fence before seeing the
// result ("Done.") is superseded by that reply and not shown.
func TestTurnRendersInEmissionOrder(t *testing.T) {
	t.Parallel()
	var calls atomic.Int32
	stub := uitest.LLMFunc(func(string, []llm.Message) string {
		if calls.Add(1) == 1 {
			return "Looking…\n```js\nconsole.log(1)\n```\nDone."
		}
		return "All good."
	})
	d := uitest.Mount(t, func(c *kernel.Context) { c.Provide("llm", stub) }, "codemode", "loop")
	d.Say("go")
	d.WaitFor("All good.")
	f := d.Frame()
	iL, iC, iR := strings.Index(f, "Looking…"), strings.Index(f, "▸ code js (1 line)"), strings.Index(f, "▸ result (1 line): 1")
	iD, iA := strings.Index(f, "Done."), strings.Index(f, "All good.")
	if iL < 0 || iC < 0 || iR < 0 || iA < 0 || !(iL < iC && iC < iR && iR < iA) {
		t.Fatalf("not in emission order (prose@%d code@%d result@%d next@%d):\n%s", iL, iC, iR, iA, f)
	}
	if iD >= 0 {
		t.Fatalf("pre-result prose should be superseded by the next reply:\n%s", f)
	}
}

// Harness smoke: the default quit binding resolves through the real
// keymap service and surfaces as tea.Quit.
func TestHarnessDefaultQuitKey(t *testing.T) {
	t.Parallel()
	d := uitest.Mount(t, nil, "codemode", "llm-echo", "loop")
	d.Press("ctrl+c", "ctrl+c") // once arms, twice quits
	d.WaitQuit()
}
