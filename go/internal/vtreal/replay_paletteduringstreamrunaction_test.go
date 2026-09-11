package vtreal

// The action palette opened (leader, p) while a reply streams, with
// collapse_all run from it. The stream must keep landing after the
// palette closes (every word once, the turn finishes), collapse_all
// leaves the finished code block folded, and nothing typed into the
// palette leaks into the composer.
//
// The tape paces the llm's words (delay_ms); the palette opens on
// seeing the reply's HALFWAY word, so it always lands mid-stream.

import (
	"fmt"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

func paletteDuringStreamRunActionStart(t *testing.T) *app {
	t.Helper()
	tape, err := filepath.Abs("testdata/replay/paletteduringstreamrunaction.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	a := startCfg(t, 100, 60, cancelConfig(tape, 40)+`
- id: ui
  plugin: ui
  config: {collapse: none}
`)
	// Turn 1 finishes with an open code block.
	a.typeText("first turn")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn 1 never finished:\n%s", a.text())
	}
	a.waitFor("First done.")
	a.waitFor(`console.log("early")`)
	// Turn 2 streams; wait for the midpoint.
	a.typeText("stream now")
	a.key(uv.KeyEnter, 0)
	a.waitFor("HALFWAY")
	if strings.Contains(a.text(), "p160") {
		t.Fatalf("turn 2 finished before the palette; the tape is not paced:\n%s", a.text())
	}
	return a
}

// paletteDuringStreamRunActionStream checks every word of turn 2 is
// on screen exactly once.
func paletteDuringStreamRunActionStream(t *testing.T, s string) {
	t.Helper()
	count := map[string]int{}
	for _, w := range strings.Fields(s) {
		count[w]++
	}
	for i := 1; i <= 160; i++ {
		if w := fmt.Sprintf("p%03d", i); count[w] != 1 {
			t.Errorf("%s is on screen %d times, want 1", w, count[w])
		}
	}
	if count["HALFWAY"] != 1 {
		t.Errorf("HALFWAY is on screen %d times, want 1", count["HALFWAY"])
	}
	if t.Failed() {
		t.Logf("screen:\n%s", s)
	}
}

func TestPaletteDuringStreamRunAction(t *testing.T) {
	t.Parallel()
	a := paletteDuringStreamRunActionStart(t)
	actionPaletteLeader(a, 'p')
	a.waitFor("collapse_all")
	a.typeText("collapse_a")
	a.key(uv.KeyEnter, 0)
	a.waitFor("collapsed")
	if strings.Contains(a.text(), "p160") {
		t.Fatalf("collapse_all landed after the stream ended; the scenario is vacuous:\n%s", a.text())
	}
	if !a.waitDone(2, 60*time.Second) {
		t.Fatalf("turn 2 never finished after the palette:\n%s", a.text())
	}
	a.waitFor("Second done.")
	s := a.settled()

	t.Run("stream_complete_once", func(t *testing.T) {
		paletteDuringStreamRunActionStream(t, s)
	})
	t.Run("finished_block_collapsed", func(t *testing.T) {
		// A folded block keeps its first line in the header; the open
		// box row is what must be gone.
		if strings.Contains(s, `│ console.log("early")`) || !strings.Contains(s, `▸ code js (1 line): console.log("early")`) {
			t.Errorf("turn 1's code block is still open after collapse_all:\n%s", s)
		}
		if strings.Contains(s, "│ early") || !strings.Contains(s, "▸ result (1 line): early") {
			t.Errorf("turn 1's result block is still open after collapse_all:\n%s", s)
		}
	})
	t.Run("no_leak_into_composer", func(t *testing.T) {
		c := actionPaletteComposer(s)
		if strings.Contains(c, "collapse") || strings.Contains(c, "/") {
			t.Errorf("palette input leaked into the composer (%q):\n%s", c, s)
		}
		if strings.Contains(s, "action · ") {
			t.Errorf("palette still open:\n%s", s)
		}
	})
	a.check("after the palette")
}

// Typing into the palette then esc mid-stream: the query is dropped,
// not handed to the composer, and the stream still completes.
func TestPaletteDuringStreamRunActionEsc(t *testing.T) {
	t.Parallel()
	a := paletteDuringStreamRunActionStart(t)
	actionPaletteLeader(a, 'p')
	a.waitFor("collapse_all")
	a.typeText("expa")
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "collapse_all") }, "filter to drop collapse_all")
	a.key(uv.KeyEscape, 0)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "action · ") }, "palette to close")
	if !a.waitDone(2, 60*time.Second) {
		t.Fatalf("turn 2 never finished after esc:\n%s", a.text())
	}
	a.waitFor("Second done.")
	s := a.settled()
	t.Run("stream_complete_once", func(t *testing.T) {
		paletteDuringStreamRunActionStream(t, s)
	})
	t.Run("no_leak_into_composer", func(t *testing.T) {
		if c := actionPaletteComposer(s); strings.Contains(c, "expa") || strings.Contains(c, "/") {
			t.Errorf("palette query leaked into the composer (%q):\n%s", c, s)
		}
	})
	a.check("after esc")
}
