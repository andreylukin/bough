package vtreal

// /theme switched through the slash picker while a long reply is still
// streaming: the ui remounts on the new palette mid-turn. After the
// turn is done no cell may still carry the old palette, the user
// marker and the code box carry the new one, and the streamed text is
// on screen once — complete, not duplicated by the remount.
//
// The tape paces its words (delay_ms) and the switch fires on seeing
// the reply's MIDPOINT word, so the switch always lands mid-stream.

import (
	"fmt"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// themeSwitchDuringStreamForest is every forest colour nord does not
// share: any of these left on screen after the switch is a stale row.
var themeSwitchDuringStreamForest = map[string]bool{
	"#a7c080": true, "#9da9a0": true, "#e67e80": true, "#83c092": true,
	"#859289": true, "#56635f": true, "#d3c6aa": true, "#3d484d": true,
	"#dbbc7f": true, "#475258": true,
}

func themeSwitchDuringStreamTape(t *testing.T) string {
	t.Helper()
	p, err := filepath.Abs("testdata/replay/themeswitchduringstream.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	return p
}

func TestThemeSwitchDuringStream(t *testing.T) {
	t.Parallel()
	a := themeStart(t, 100, 60, cancelConfig(themeSwitchDuringStreamTape(t), 40)+`
- id: theme
  plugin: theme
  config: {name: forest}
- id: ui
  plugin: ui
  config: {collapse: none}
`)
	a.typeText("paint something")
	a.key(uv.KeyEnter, 0)
	a.waitFor("MIDPOINT")
	if strings.Contains(a.text(), "w160") {
		t.Fatalf("the reply finished before the switch; the tape is not paced:\n%s", a.text())
	}

	// The picker: "/them" opens the palette, Tab completes "/theme ",
	// the arg is typed and Enter accepts through the palette.
	a.typeText("/them")
	a.waitFor("list color themes")
	a.key(uv.KeyTab, 0)
	a.typeText("nord")
	a.key(uv.KeyEnter, 0)
	a.waitFor("theme: nord")
	if strings.Contains(a.text(), "w160") {
		t.Fatalf("the switch landed after the stream ended; the scenario is vacuous:\n%s", a.text())
	}

	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished after the switch:\n%s", a.text())
	}
	a.waitFor("Painted.")
	s := a.settled()

	t.Run("stream_complete_once", func(t *testing.T) {
		count := map[string]int{}
		for _, w := range strings.Fields(s) {
			count[w]++
		}
		for i := 1; i <= 160; i++ {
			if w := fmt.Sprintf("w%03d", i); count[w] != 1 {
				t.Errorf("%s is on screen %d times, want 1", w, count[w])
			}
		}
		if count["MIDPOINT"] != 1 {
			t.Errorf("MIDPOINT is on screen %d times, want 1", count["MIDPOINT"])
		}
		if t.Failed() {
			t.Logf("screen:\n%s", s)
		}
	})

	t.Run("palette", func(t *testing.T) {
		a.waitUntil(func(string) bool {
			c, ok := themeMarker(a)
			hex, _ := themeHex(c.Style.Fg)
			return ok && hex == "#a3be8c"
		}, "the ❯ marker in nord user #a3be8c")
		snap := a.term.Snapshot()
		lines := strings.Split(a.text(), "\n")
		row := -1
		for y, l := range lines {
			if strings.HasPrefix(l, `│ console.log("swatch")`) {
				row = y
				break
			}
		}
		if row < 1 {
			t.Fatalf("no open code box on screen:\n%s", a.text())
		}
		for _, c := range []uv.Cell{snap.Cells[row-1][0], snap.Cells[row][0]} {
			if hex, _ := themeHex(c.Style.Fg); hex != "#4c566a" {
				t.Errorf("box border %q is %q, want nord border #4c566a", c.Content, hex)
			}
		}
		if hex, _ := themeHex(snap.Cells[row][2].Style.Fg); hex != "#d8dee9" {
			t.Errorf("code text is %q, want nord code #d8dee9", hex)
		}
		for y, cells := range snap.Cells {
			for x, c := range cells {
				fg, _ := themeHex(c.Style.Fg)
				bg, _ := themeHex(c.Style.Bg)
				if themeSwitchDuringStreamForest[fg] || themeSwitchDuringStreamForest[bg] {
					t.Errorf("row %d col %d %q still forest fg=%s bg=%s: %q", y, x, c.Content, fg, bg, lines[y])
					break
				}
			}
		}
		if t.Failed() {
			t.Logf("screen:\n%s", a.text())
		}
	})
	a.check("after the switch")
}
