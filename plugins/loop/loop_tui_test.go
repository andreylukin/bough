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
	frame := d.Frame()
	for _, want := range []string{"╭─ js", "╭─ result", "hi from codemode"} {
		if !strings.Contains(frame, want) {
			t.Fatalf("frame missing %q:\n%s", want, frame)
		}
	}
	code := strings.Index(frame, "╭─ js")
	result := strings.Index(frame, "╭─ result")
	final := strings.Index(frame, "[tool output]")
	if !(code < result && result < final) {
		t.Fatalf("blocks out of order (js@%d result@%d final@%d):\n%s", code, result, final, frame)
	}
}

// Two js blocks in one reply run in order and render two result boxes.
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
	frame := d.Frame()
	if got := strings.Count(frame, "╭─ result"); got != 2 {
		t.Fatalf("want 2 result boxes, got %d:\n%s", got, frame)
	}
	if !strings.Contains(frame, "OUT_ONE") || !strings.Contains(frame, "OUT_TWO") {
		t.Fatalf("missing tool outputs:\n%s", frame)
	}
}
