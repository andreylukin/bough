package uitest_test

import (
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"
)

// The live sequence seen 2026-09-03 through the real loop + workers
// with a streaming provider: the parent's ONE reply carries two fences
// (spawn, then a verification) with prose between. Every fence runs in
// order, so the opening prose, the card and its result, the prose
// between the fences and the verification render in emission order.
// Prose after the last fence narrates output the model had not seen
// ("Verification confirms.") and is dropped rather than shown as fact.
func TestSubagentTurnStreamsInEmissionOrder(t *testing.T) {
	t.Parallel()
	stub := &uitest.Streaming{Replies: []string{
		"I'll spawn.\n" + uitest.JS(`console.log(tools.spawn("make notes"))`) +
			"\nThe subagent has finished. Let me verify:\n" + uitest.Bash("printf 'x'") + "\nVerification confirms.",
		"Writing.\n" + uitest.Bash("printf 'x'"),
		"Findings: wrote notes.md",
		"Done, verified.",
	}, Chunk: uitest.ByN(7)}
	d := mountLLM(t, stub, "workers")
	d.Say("go")
	turnDone(d, "Done, verified.")
	f := d.Frame()
	last := -1
	for _, want := range []string{"I'll spawn.", "subagent 1 ·", "[subagent 1 · task", "Let me verify:", "Ran: printf"} {
		i := strings.Index(f, want)
		if i < 0 || i < last {
			t.Fatalf("%q missing or out of order:\n%s", want, f)
		}
		last = i
	}
	if strings.Contains(f, "Verification confirms") {
		t.Fatalf("prose after the last block should be dropped:\n%s", f)
	}
}
