package vtreal

// One block per reply: a tape reply carrying five js fences runs only
// the first; the result carries the loop's note, and the next tape
// result still lines up with the next block (had blocks 2-5 run, they
// would have eaten foxtrot6's result out of order).

import (
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// firstBlockOnlyTexts groups data.text of the run's own session by kind.
func firstBlockOnlyTexts(a *app) map[string][]string {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	out := map[string][]string{}
	for _, p := range paths {
		es, err := history.Read(p)
		if err != nil {
			continue
		}
		for _, e := range es {
			s, _ := e.Data["text"].(string)
			out[e.Kind] = append(out[e.Kind], s)
		}
	}
	return out
}

// A reply's code blocks all run, in order, up to loop.MaxBlocks. This
// tape is five blocks in one reply and a sixth in the next: under the
// old first-block-only rule two ran, and the four in between were
// dropped with a marker. All six run now, and nothing is dropped
// because five is under the cap.
func TestMultiBlockReplyRunsEveryBlock(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/first-block-only.jsonl")
	a := startCfg(t, 100, 40, replayConfig(tape))
	a.check("boot")
	a.typeText("do five things")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	screen := a.check("after turn")
	h := firstBlockOnlyTexts(a)
	codes, results, assistant := h["code"], h["result"], h["assistant"]

	t.Run("every block runs, in reply order", func(t *testing.T) {
		want := []string{"alpha1", "bravo2", "charlie3", "delta4", "echo5", "foxtrot6"}
		if len(codes) != len(want) {
			t.Fatalf("want %d code entries, got %d: %q\n%s", len(want), len(codes), codes, screen)
		}
		for i, w := range want {
			if !strings.Contains(codes[i], w) {
				t.Errorf("code %d = %q, want %s\n%s", i, codes[i], w, screen)
			}
		}
	})

	t.Run("nothing is dropped under the cap", func(t *testing.T) {
		if len(assistant) == 0 {
			t.Fatalf("no assistant entry\n%s", screen)
		}
		if strings.Contains(assistant[0], "dropped") {
			t.Errorf("five blocks are under the cap and must not be dropped: %q\n%s", assistant[0], screen)
		}
		for _, r := range results {
			if strings.Contains(r, "only the first") {
				t.Errorf("stale first-block-only note survived: %q", r)
			}
		}
	})

	t.Run("each block gets its own result", func(t *testing.T) {
		if len(results) != len(codes) {
			t.Errorf("want one result per block (%d), got %d: %q\n%s",
				len(codes), len(results), results, screen)
		}
	})
}
