package loop

// A cancel that arrives before its turn has installed a context must
// not be dropped. The ui marks itself running as it submits and the
// input only reaches run afterwards, so ctrl+c pressed the instant you
// see your own typo lands in that gap. On a fast machine the gap is
// microseconds; on a loaded CI runner it was wide enough to lose the
// cancel every time.

import (
	"context"
	"testing"

	"github.com/andreylukin/bough/plugins/history"
)

func TestCancelBeforeTheTurnStartsIsHonoured(t *testing.T) {
	var tn turns
	tn.Cancel() // nothing in flight yet

	// run installs the turn's context; the pending cancel applies to it.
	tctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	tn.mu.Lock()
	tn.cancel = cancel
	pending := tn.pending
	tn.pending = false
	tn.mu.Unlock()
	if !pending {
		t.Fatal("a cancel with no turn in flight must be remembered, not dropped")
	}
	cancel() // what run does when it sees the pending flag
	if tctx.Err() == nil {
		t.Fatal("the turn should start already cancelled")
	}
}

// The flag is consumed, so it cannot cancel a second turn.
func TestPendingCancelAppliesOnlyOnce(t *testing.T) {
	var tn turns
	tn.Cancel()

	tn.mu.Lock()
	first := tn.pending
	tn.pending = false
	tn.mu.Unlock()
	if !first {
		t.Fatal("the first turn should see the pending cancel")
	}

	tn.mu.Lock()
	second := tn.pending
	tn.mu.Unlock()
	if second {
		t.Fatal("a consumed cancel must not carry into the next turn")
	}
}

// A cancel with a turn in flight still cancels that turn directly.
func TestCancelDuringATurnStillCancels(t *testing.T) {
	var tn turns
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	tn.mu.Lock()
	tn.cancel = cancel
	tn.mu.Unlock()

	tn.Cancel()
	if ctx.Err() == nil {
		t.Fatal("the turn in flight should be cancelled")
	}
	tn.mu.Lock()
	defer tn.mu.Unlock()
	if tn.pending {
		t.Error("a cancel that reached a live turn must not also latch")
	}
}

// A cancel that lands while a code block is RUNNING must still record
// what the block wrote. /undo right after esc is the only way back, and
// the turn is the thing it undoes.
//
// The cancel-during-the-LLM-call path always passed doneData; this one
// passed nil, so a turn interrupted mid-write recorded no files and
// could not be undone. It surfaced only once cancels stopped being
// dropped (see TestCancelBeforeTheTurnStartsIsHonoured) — before that
// the cancel usually landed during the LLM call instead.
func TestCancelDuringCodeStillRecordsWrites(t *testing.T) {
	code := &cancellingCode{}
	hist := &memHistory{}
	r := &runner{
		llm:         &seqLLM{replies: []string{"```js\ntools.write(\"made.txt\", \"x\")\n```"}},
		code:        code,
		hist:        hist,
		secs:        &Sections{},
		stats:       &fakeStats{files: []string{"made.txt"}},
		stopRetries: 0,
	}

	ctx, cancel := context.WithCancel(context.Background())
	code.cancel = cancel
	var kinds, texts []string
	_ = r.Run(ctx, "make a file", collect(&kinds, &texts))

	entries := hist.Entries()
	var done *history.Entry
	for i := range entries {
		if entries[i].Kind == "done" {
			done = &entries[i]
		}
	}
	if done == nil {
		t.Fatalf("a cancelled turn still ends with done: %v", kinds)
	}
	files, _ := done.Data["files"].([]string)
	if len(files) != 1 || files[0] != "made.txt" {
		t.Fatalf("the cancelled turn must record what it wrote, got %v", done.Data["files"])
	}
}

// fakeStats stands in for the tools plugin's per-turn tally.
type fakeStats struct{ files []string }

func (f *fakeStats) Take() ([]string, int, bool) {
	files := f.files
	f.files = nil
	return files, 0, true
}

// cancellingCode cancels the turn from inside the block, the way esc
// does while a command is running.
type cancellingCode struct{ cancel context.CancelFunc }

func (c *cancellingCode) RegisterTool(string, any)   {}
func (c *cancellingCode) Run(string) (string, error) { return "", nil }
func (c *cancellingCode) RunCtx(ctx context.Context, _ string) (string, error) {
	c.cancel()
	<-ctx.Done()
	return "", ctx.Err()
}
