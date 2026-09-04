package uitest_test

import (
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"
)

// The live sequence seen 2026-09-03 through the real loop + workers
// with a streaming provider: the parent's ONE reply carries two fences
// (spawn, then a verification) with prose between. Only the first
// fence runs, so the card and its result render under the opening
// prose, and everything the model wrote after that fence — narration
// of results it had not seen ("The subagent has finished.",
// "Verification confirms.") — is dropped rather than shown as fact.
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
	iOpen, iCard, iRes := strings.Index(f, "I'll spawn."), strings.Index(f, "subagent 1 ·"), strings.Index(f, "[subagent 1 · task")
	if iOpen < 0 || iCard < 0 || iRes < 0 || !(iOpen < iCard && iCard < iRes) {
		t.Fatalf("out of order (prose@%d card@%d result@%d):\n%s", iOpen, iCard, iRes, f)
	}
	for _, guess := range []string{"The subagent has finished", "Verification confirms"} {
		if strings.Contains(f, guess) {
			t.Fatalf("prose after the first block should be dropped, found %q:\n%s", guess, f)
		}
	}
}
