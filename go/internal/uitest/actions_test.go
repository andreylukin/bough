package uitest_test

import (
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"
	"github.com/andreylukin/bough/kernel"

	_ "github.com/andreylukin/bough/plugins/commands"
)

// The action palette and the leader chords against the real commands
// registry and a real turn: "/" lists the built-in commands above the
// action rows, enter on an action row runs it without dispatching or
// submitting, and ctrl+x chords run the same actions.
func TestActionPaletteAndChords(t *testing.T) {
	t.Parallel()
	stub := &uitest.Script{Replies: []string{"```js\nconsole.log(41 + 1)\n```", "done"}}
	d := uitest.Mount(t, func(c *kernel.Context) { c.Provide("llm", stub) }, "codemode", "commands", "loop")
	d.Say("go")
	d.WaitFor("▸ result (1 line): 42") // collapsed by default

	d.Type("/")
	d.Step()
	f := d.Frame()
	iCmd, iAct := strings.Index(f, "/help"), strings.Index(f, "action · ")
	if iCmd < 0 || iAct < 0 || iAct < iCmd {
		t.Fatalf("\"/\" should list the commands above the action rows:\n%s", f)
	}
	d.Type("expand_all")
	d.Step()
	if !strings.Contains(d.Frame(), "action · expand all blocks") {
		t.Fatalf("the query should narrow to the action row:\n%s", d.Frame())
	}
	d.Press("enter")
	d.WaitFor("▾ result (1 line): 42")
	if f := d.Frame(); strings.Contains(f, "❯ /expand_all") || strings.Contains(f, "unknown command") {
		t.Fatalf("an action row never dispatches:\n%s", f)
	}
	if stub.Calls != 2 {
		t.Fatalf("an action row never reaches the loop: %d llm calls, want 2", stub.Calls)
	}

	d.Press("ctrl+x", "c")
	d.WaitFor("▸ result (1 line): 42")
	d.Press("ctrl+x", "k")
	d.WaitFor("chords (ctrl+x, then a key)")
	d.Press("ctrl+x", "z")
	d.WaitFor("ctrl+x z: no such chord")
}
