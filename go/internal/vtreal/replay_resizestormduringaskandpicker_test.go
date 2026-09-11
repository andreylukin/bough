package vtreal

// A resize storm with the "@" picker open over a pending tools.ask:
// the ask_resize_mid tape asks with four options, "@" opens the file
// picker in the composer, the highlight moves off its first row, then
// 30 seeded random widths (20..200) fire within a second before the
// pane settles at 80x24. Nothing may panic, the ask box and the picker
// must both still be on screen and inside the pane, and the picker's
// highlight must still be the row it was moved to.

import (
	"math/rand"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestResizeStormDuringAskAndPicker(t *testing.T) {
	t.Parallel()
	tape, err := filepath.Abs("testdata/replay/ask_resize_mid.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	tm, home := resizeTmuxStart(t, 100, 30, askConfig(tape))
	for _, name := range []string{"alpha.md", "beta.md", "gamma.md"} {
		if err := os.WriteFile(filepath.Join(home, name), []byte("x\n"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	resizeTmuxSend(tm, "pick a route")
	tm.waitFor("DELTAEND")
	tm.keys("@")
	tm.waitFor("@alpha.md")
	first := pickerResizeSel(pickerResizeSettled(tm, 30, "> "), "> ")
	tm.keys("Down")
	tm.waitUntil(func(string) bool {
		s := pickerResizeSel(resizeTmuxScreen(tm), "> ")
		return s != "" && s != first
	}, "the highlight to move off "+first)
	sel := pickerResizeSel(pickerResizeSettled(tm, 30, "> "), "> ")

	rng := rand.New(rand.NewSource(20260911))
	start := time.Now()
	for range 30 {
		tm.resize(20+rng.Intn(181), 24)
		time.Sleep(25 * time.Millisecond)
	}
	if d := time.Since(start); d > 2*time.Second {
		t.Logf("storm took %v (slow tmux), still checking", d)
	}
	tm.resize(80, 24)
	s := pickerResizeSettled(tm, 24, "> ")
	ls := strings.Split(s, "\n")
	if panicky.MatchString(s) {
		t.Fatalf("crash text on screen:\n%s", s)
	}
	for i, l := range ls {
		if w := len([]rune(l)); w > 80 {
			t.Errorf("row %d is %d cells wide in an 80-column pane:\n%s", i, w, s)
		}
	}
	// "type a number" is the composer placeholder, gone once "@" is
	// typed; the ask box is its question and all four options.
	for _, want := range []string{"Pick a route", "ALPHAEND", "DELTAEND", "@alpha.md", "@beta.md"} {
		if !strings.Contains(s, want) {
			t.Errorf("%q not visible after the storm:\n%s", want, s)
		}
	}
	if got := pickerResizeSel(s, "> "); !pickerResizeSame(got, sel) {
		t.Errorf("highlighted row %q after the storm, want %q:\n%s", got, sel, s)
	}
	if composerRow(ls) < 0 {
		t.Errorf("composer gone after the storm:\n%s", s)
	}
}
