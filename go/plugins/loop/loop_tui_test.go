// TUI-integration tests: real kernel + loop + codemode + a
// deterministic llm, driven through the real ui model in-process.
package loop_test

import (
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"

	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/tools"
)

// The multi-step CODE! flow renders code box, result box, and the
// final plain reply, in order.
func TestCodemodeFlowRenders(t *testing.T) {
	t.Parallel()
	d := uitest.Mount(t, nil, "codemode", "tools-basic", "llm-echo", "loop")
	d.Say("CODE!")
	// Step 2's echo reply quotes the fed-back tool output, so its
	// arrival proves the full llm -> code -> result -> llm round trip.
	d.WaitFor("[tool output]")
	d.Press("tab", "enter", "tab", "enter") // expand the code block, then the result
	frame := d.Frame()
	for _, want := range []string{"▾ Ran: echo hi from codemode", "▾ result", "hi from codemode"} {
		if !strings.Contains(frame, want) {
			t.Fatalf("frame missing %q:\n%s", want, frame)
		}
	}
	code := strings.Index(frame, "▾ Ran: echo hi from codemode")
	result := strings.Index(frame, "▾ result")
	final := strings.Index(frame, "[tool output]")
	if !(code < result && result < final) {
		t.Fatalf("blocks out of order (js@%d result@%d final@%d):\n%s", code, result, final, frame)
	}
}

// Only the FIRST js block of a reply runs: the second is dropped with
// a marker, so one runaway reply cannot fire a hundred commands.
func TestMultiBlockReplyRenders(t *testing.T) {
	t.Parallel()
	step := 0
	parrot := uitest.LLMFunc(func(string, []llm.Message) string {
		step++ // the loop serializes Complete calls; no lock needed
		if step == 1 {
			return "two blocks:\n```js\nconsole.log('OUT_ONE')\n```\nand\n```js\nconsole.log('OUT_TWO')\n```"
		}
		return "all done here"
	})
	d := uitest.Mount(t, func(c *kernel.Context) { c.Provide("llm", parrot) },
		"codemode", "loop")
	d.Say("go")
	d.WaitFor("all done here")
	// Blocks arrive collapsed; expand both result blocks (tab starts at
	// the newest and walks older: result2, code2, result1, code1).
	d.Press("tab", "enter")
	frame := d.Frame()
	if got := strings.Count(frame, "▾ result"); got != 1 {
		t.Fatalf("want 1 result box, got %d:\n%s", got, frame)
	}
	if !strings.Contains(frame, "OUT_ONE") {
		t.Fatalf("missing the first block's output:\n%s", frame)
	}
	if strings.Contains(frame, "OUT_TWO") {
		t.Fatalf("the second block ran:\n%s", frame)
	}
}
