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

func TestFirstBlockOnly(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/first-block-only.jsonl")
	a := startCfg(t, 100, 40, replayConfig(tape))
	a.check("boot")
	a.typeText("do five things")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.check("after turn")
	screen := a.settled()
	h := firstBlockOnlyTexts(a)
	codes, results, assistant := h["code"], h["result"], h["assistant"]

	t.Run("TestFirstBlockOnlyRunsOneBlock", func(t *testing.T) {
		if len(codes) != 2 || !strings.Contains(codes[0], "alpha1") || !strings.Contains(codes[1], "foxtrot6") {
			t.Errorf("want code entries [alpha1 foxtrot6], got %q\n%s", codes, screen)
		}
		// The finished turn folds its Ran blocks into one summary row;
		// all five running would read "6 steps · ran 6 commands".
		if !strings.Contains(screen, "2 steps · ran 2 commands") {
			t.Errorf("want the turn folded as 2 steps, 2 commands:\n%s", screen)
		}
		for _, w := range []string{"bravo2", "charlie3", "delta4", "echo5"} {
			if strings.Contains(screen, "echo "+w) {
				t.Errorf("dropped block %s is on screen:\n%s", w, screen)
			}
		}
	})

	t.Run("TestFirstBlockOnlyReplyRecordsDropMarker", func(t *testing.T) {
		if len(assistant) == 0 || !strings.Contains(assistant[0], "[4 further code block(s) dropped") {
			t.Fatalf("first assistant entry lacks the drop marker: %q\n%s", assistant, screen)
		}
		if strings.Contains(assistant[0], "bravo2") {
			t.Errorf("dropped blocks re-entered the recorded reply: %q\n%s", assistant[0], screen)
		}
	})

	t.Run("TestFirstBlockOnlyResultCarriesNote", func(t *testing.T) {
		if len(results) == 0 {
			t.Fatalf("no result entries\n%s", screen)
		}
		if !strings.Contains(results[0], "OUT-alpha1") ||
			!strings.Contains(results[0], "[only the first of your 5 code blocks ran.") {
			t.Errorf("first result lacks output or note: %q\n%s", results[0], screen)
		}
	})

	t.Run("TestFirstBlockOnlyNextResultAligns", func(t *testing.T) {
		if len(results) != 2 || !strings.Contains(results[1], "OUT-foxtrot6") {
			t.Fatalf("want second result OUT-foxtrot6, got %q\n%s", results, screen)
		}
		if strings.Contains(results[1], "only the first") {
			t.Errorf("single-block reply got the note: %q\n%s", results[1], screen)
		}
		if !strings.Contains(screen, "First-block-only finished.") {
			t.Errorf("final answer missing (tape out of step):\n%s", screen)
		}
	})
}
