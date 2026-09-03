package uitest_test

// The workers plugin end to end: a parent turn spawns a child, the
// child runs a tool and reports, the parent finishes. The UI shows one
// card per worker, updated in place, and the child's transcript stays
// behind it.

import (
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"

	_ "github.com/andreylukin/bough/plugins/workers"
)

// The one llm serves parent and child in call order: parent → child
// (two steps) → parent.
func TestSubagentCardThroughRealWorkers(t *testing.T) {
	t.Parallel()
	stub := &uitest.Script{Replies: []string{
		uitest.JS(`console.log(tools.spawn("count go files"))`),
		"Looking.\n" + uitest.Bash("printf 'a.go\\nb.go\\n'"),
		"Status: ok\nFindings: 2 go files (a.go, b.go)\nFiles: none\nOpen: none",
		"parent sees two files",
	}}
	d := mountLLM(t, stub, "workers")
	d.Say("how many go files?")
	turnDone(d, "parent sees two files")
	fits(t, d)
	f := d.Frame()
	if n := strings.Count(f, "sub 1 ·"); n != 1 {
		t.Fatalf("want exactly one card row for worker 1, got %d:\n%s", n, f)
	}
	if !strings.Contains(f, "✔") || !strings.Contains(f, "1 call") {
		t.Fatalf("finished card should show ✔ and the call count:\n%s", f)
	}
	// The child's own rows never interleave with the parent's story.
	for _, leak := range []string{"Looking.", "a.go\nb.go"} {
		if strings.Contains(f, leak) {
			t.Fatalf("child activity %q leaked into the parent transcript:\n%s", leak, f)
		}
	}
	// The parent's tool output carries provenance for the delegated report.
	d.Press("tab", "enter") // expand the newest collapsible: the spawn's result
	if f := d.Frame(); !strings.Contains(f, "[subagent 1 · task: count go files]") {
		// the result box may be further up; expand the rest too
		for i := 0; i < 3 && !strings.Contains(d.Frame(), "[subagent 1"); i++ {
			d.Press("tab", "enter")
		}
		if !strings.Contains(d.Frame(), "[subagent 1 · task: count go files]") {
			t.Fatalf("spawn result lacks the provenance line:\n%s", d.Frame())
		}
	}
	if stub.Calls != 4 {
		t.Fatalf("llm calls = %d, want 4 (parent, child ×2, parent)", stub.Calls)
	}
}

// A child that errors out shows ✗ on its card and the parent's turn
// still ends (the spawn error is the parent's tool error).
func TestSubagentCardOnChildError(t *testing.T) {
	t.Parallel()
	stub := &uitest.Script{Replies: []string{
		uitest.JS(`try { tools.spawn("break") } catch (e) { console.log("caught: " + e) }`),
		uitest.JS(`throw new Error("child exploded")`),
		uitest.JS(`throw new Error("child exploded")`),
		uitest.JS(`throw new Error("child exploded")`),
		uitest.JS(`throw new Error("child exploded")`),
		uitest.JS(`throw new Error("child exploded")`),
		uitest.JS(`throw new Error("child exploded")`),
		"parent recovered",
	}}
	d := mountLLM(t, stub, "workers")
	d.Say("go")
	turnDone(d, "parent recovered")
	fits(t, d)
	f := d.Frame()
	if !strings.Contains(f, "✗") || !strings.Contains(f, "sub 1 ·") {
		t.Fatalf("card should show ✗ for a failed child:\n%s", f)
	}
}
