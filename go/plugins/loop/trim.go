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
	// keepWholeResults is the FLOOR: this many recent tool outputs stay
	// whole no matter how large they are, so the model never loses what
	// it is working from.
	keepWholeResults = 8
	// projectionBudget is what the trimming is actually for. Older
	// results are shortened until the projection fits, and a session
	// that never gets big is never touched at all. The floor above wins
	// when it has to: if the most recent results are themselves larger
	// than this, the projection stays over budget rather than taking
	// what the model is working from.
	//
	// A fixed count was the first attempt and it was wrong: at eight,
	// a long task lost the files it had read and read them AGAIN — 70
	// tools.view calls across 7 files in one run, 19 of them the same
	// file, against 12 in the run before. Trimming to a budget keeps
	// far more of the recent work and still bounds the growth.
	projectionBudget = 60_000
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
	var results []int
	total := 0
	for i, m := range msgs {
		total += len(m.Content)
		if m.Role == "user" && strings.HasPrefix(m.Content, toolOutputPrefix) {
			results = append(results, i)
		}
	}
	if total <= projectionBudget || len(results) <= keep {
		return msgs
	}
	// Oldest first, stopping as soon as the projection is under budget.
	// The newest `keep` are never touched however far over it we are:
	// they are what the model is working from right now, and taking
	// them is what made it re-read the same file nineteen times.
	protected := len(results) - keep
	spent := total
	trim := map[int]bool{}
	for n := 0; n < protected && spent > projectionBudget; n++ {
		i := results[n]
		short := trimOne(msgs[i].Content)
		saved := len(msgs[i].Content) - len(short)
		if saved <= 0 {
			continue // already small; trimming it buys nothing
		}
		trim[i] = true
		spent -= saved
	}
	if len(trim) == 0 {
		return msgs
	}
	out := make([]llm.Message, len(msgs))
	copy(out, msgs)
	for i := range trim {
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
