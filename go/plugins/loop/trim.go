package loop

// Trimming stale tool output out of the projection.
//
// bough re-sends the whole conversation on every step, which is what
// makes resume, provider swaps and inspection free — but a long turn
// pays for it. Measured on a real 55-step task in this repo: 150KB
// projected, 89% of it tool output, ~37.5k tokens, and $0.113 for each
// further step, compounding. Worse than the money, the model is asked
// to find the thread through fifty old file dumps.
//
// This is NOT compaction. Nothing is summarised, no model is called,
// and the history log is untouched — /export, resume and `bough log`
// still show every byte. Only what the model is SHOWN changes, and only
// for output it has already had several turns to act on: the most
// recent results stay whole, older ones keep their head and say plainly
// what was cut and how to get it back.
//
// The agent can always read a file again. It cannot un-drown in
// forty screens of grep output.

import (
	"fmt"
	"strings"

	"github.com/andreylukin/bough/plugins/llm"
)

const (
	// keepWholeResults is how many of the most recent tool outputs are
	// shown in full. The model is usually working from the last one or
	// two; this is generous enough to cover a read-then-edit-then-test
	// sequence without trimming anything it is mid-thought about.
	keepWholeResults = 8
	// trimHead is how much of an older result survives. Enough to
	// recognise what the command was and what it found — a directory
	// listing, the first failures of a test run — without the body.
	trimHead = 600
	// trimFloor is the size below which trimming is not worth it: the
	// marker would cost nearly as much as the text.
	trimFloor = trimHead + 200
)

// trimNote tells the model what happened and what to do about it. It
// names the tool output as still reachable, because the alternative is
// a model that thinks the file no longer exists.
const trimNote = "\n\n[%d characters of this output were trimmed to keep the conversation readable. It is older tool output, not new information — run the command again if you need the rest.]"

// trimProjection shortens tool outputs older than the most recent
// keepWholeResults of them. Messages are otherwise untouched, and the
// order never changes.
//
// keep <= 0 disables trimming entirely (the loop row's
// `keep_whole_results: 0`), which is the escape hatch for anyone who
// would rather pay than have the model shown less.
func trimProjection(msgs []llm.Message, keep int) []llm.Message {
	if keep <= 0 {
		return msgs
	}
	// Find the indices of tool-output messages, newest last.
	var results []int
	for i, m := range msgs {
		if m.Role == "user" && strings.HasPrefix(m.Content, toolOutputPrefix) {
			results = append(results, i)
		}
	}
	if len(results) <= keep {
		return msgs
	}
	out := make([]llm.Message, len(msgs))
	copy(out, msgs)
	for _, i := range results[:len(results)-keep] {
		out[i].Content = trimOne(out[i].Content)
	}
	return out
}

// trimOne shortens one tool-output message, keeping its prefix line.
func trimOne(content string) string {
	body := strings.TrimPrefix(content, toolOutputPrefix)
	if len(body) <= trimFloor {
		return content
	}
	// Cut on a line boundary when there is one nearby, so the head
	// reads as output rather than as a severed line.
	cut := trimHead
	if nl := strings.LastIndexByte(body[:trimHead], '\n'); nl > trimHead/2 {
		cut = nl
	}
	return toolOutputPrefix + body[:cut] + fmt.Sprintf(trimNote, len(body)-cut)
}
