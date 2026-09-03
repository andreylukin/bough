package uitest_test

import (
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"
)

// The live sequence seen 2026-09-03 through the real loop + workers
// with a streaming provider: the parent's ONE reply carries two fences
// (spawn, then a verification) with prose between. The prose between
// them must render below the card and the spawn result (once the
// spawn has actually returned), and the guess under the last fence
// ("Verification confirms.") is superseded by the final reply.
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
	iCard, iRes, iReply := strings.Index(f, "subagent 1 · done"), strings.Index(f, "[subagent 1 · task"), strings.Index(f, "The subagent has finished")
	if iCard < 0 || iRes < 0 || iReply < 0 || !(iCard < iRes && iRes < iReply) {
		t.Fatalf("out of order (card@%d result@%d reply@%d):\n%s", iCard, iRes, iReply, f)
	}
	if strings.Contains(f, "Verification confirms") {
		t.Fatalf("pre-result guess should be superseded:\n%s", f)
	}
}
