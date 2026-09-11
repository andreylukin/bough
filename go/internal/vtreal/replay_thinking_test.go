package vtreal

// Reasoning on a real terminal: a tape whose assistant replies carry
// <thinking> spans. The ui folds each span into its own collapsed
// block above the reply (plugins/ui/blocks.go splitAssistant), so the
// reasoning never lands in the transcript as prose, opens on a click,
// and closes again on the next one.
//
// The status-bar thinking indicator is checked as far as a replayed
// session can: the replay Model is not an llm.Efforter, so shift+tab
// at rest must SAY the provider has no level rather than silently
// doing nothing, and no "think …" chip may appear. Cycling a real
// level end-to-end needs an Efforter in the "llm" row, which no
// keyless plugin provides — see the report.

import (
	"fmt"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// thinkingTape is the fixture recording.
func thinkingTape(t *testing.T) string {
	t.Helper()
	p, err := filepath.Abs("testdata/replay/thinking.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	return p
}

// thinkingStart boots bough on the fixture tape and plays n turns.
func thinkingStart(t *testing.T, cols, rows, n int) *app {
	t.Helper()
	a := startCfg(t, cols, rows, replayConfig(thinkingTape(t)))
	a.check("boot")
	inputs := []string{"why is the watcher test flaky", "and the directory one up"}
	for i := range n {
		a.typeText(inputs[i])
		a.key(uv.KeyEnter, 0)
		if !a.waitDone(i+1, 60*time.Second) {
			t.Fatalf("turn %d never finished:\n%s", i+1, a.text())
		}
		a.check(fmt.Sprintf("turn %d", i+1))
	}
	return a
}

// thinkingHeaderRow returns the index of the last row that is a
// thinking-block header with the given glyph, or -1.
func thinkingHeaderRow(lines []string, glyph string) int {
	row := -1
	for i, l := range lines {
		if strings.Contains(l, glyph) && strings.Contains(l, "thinking (") {
			row = i
		}
	}
	return row
}

// A reply's reasoning becomes a collapsed header, not transcript prose.
func TestThinkingFoldIsCollapsed(t *testing.T) {
	t.Parallel()
	a := thinkingStart(t, 100, 30, 1)
	s := a.settled()
	if thinkingHeaderRow(strings.Split(s, "\n"), "▸") < 0 {
		t.Fatalf("no collapsed thinking header (▸ … thinking):\n%s", s)
	}
	// A collapsed header shows only a one-line preview: the rest of
	// the reasoning must not be in the transcript.
	if strings.Contains(s, "BODYONE") {
		t.Fatalf("the reasoning body is on screen while folded:\n%s", s)
	}
	if !strings.Contains(s, "The watcher reads the file") {
		t.Fatalf("the reply prose is missing:\n%s", s)
	}
	if strings.Contains(s, "<thinking>") || strings.Contains(s, "</thinking>") {
		t.Fatalf("raw thinking tags leaked into the transcript:\n%s", s)
	}
}

// Clicking the header opens the fold and shows the reasoning; a second
// click closes it again.
func TestThinkingFoldExpandsOnClick(t *testing.T) {
	t.Parallel()
	a := thinkingStart(t, 100, 30, 1)
	s := a.settled()
	row := thinkingHeaderRow(strings.Split(s, "\n"), "▸")
	if row < 0 {
		t.Fatalf("no collapsed thinking header to click:\n%s", s)
	}
	a.click(2, row)
	a.waitUntil(func(s string) bool {
		return strings.Contains(s, "BODYONE") && thinkingHeaderRow(strings.Split(s, "\n"), "▾") >= 0
	}, "the thinking fold to open on a click (▾ + body)")
	a.check("thinking expanded")

	s = a.settled()
	row = thinkingHeaderRow(strings.Split(s, "\n"), "▾")
	if row < 0 {
		t.Fatalf("expanded header gone after settling:\n%s", s)
	}
	a.click(2, row)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "BODYONE") }, "the thinking fold to close on a second click")
	if s := a.settled(); thinkingHeaderRow(strings.Split(s, "\n"), "▸") < 0 {
		t.Fatalf("no collapsed thinking header after the second click:\n%s", s)
	}
	a.check("thinking collapsed again")
}

// Two replies with reasoning fold independently: each turn keeps its
// own header and one open fold does not open the other.
func TestThinkingFoldsAreIndependent(t *testing.T) {
	t.Parallel()
	a := thinkingStart(t, 100, 40, 2)
	s := a.settled()
	ls := strings.Split(s, "\n")
	n := 0
	for _, l := range ls {
		if strings.Contains(l, "▸") && strings.Contains(l, "thinking (") {
			n++
		}
	}
	if n != 2 {
		t.Fatalf("%d collapsed thinking headers, want 2:\n%s", n, s)
	}
	row := thinkingHeaderRow(ls, "▸") // the newest
	a.click(2, row)
	a.waitFor("BODYTWO")
	if s := a.settled(); strings.Contains(s, "BODYONE") {
		t.Fatalf("opening the second fold also opened the first:\n%s", s)
	}
	a.check("second fold open")
}

// shift+tab at rest is the thinking-level control. The replay provider
// has no level, so the status bar must say so — and never show a
// "think …" chip it cannot back up.
func TestThinkingShiftTabReportsNoLevel(t *testing.T) {
	t.Parallel()
	a := thinkingStart(t, 100, 30, 1)
	before := a.settled()
	if strings.Contains(before, "think ") {
		t.Fatalf("a think chip without a provider that reasons:\n%s", before)
	}
	a.key(uv.KeyTab, uv.ModShift)
	a.waitUntil(func(s string) bool { return strings.Contains(s, "no thinking level") },
		"shift+tab to report that this provider has no thinking level")
	a.check("after shift+tab")
	if s := a.settled(); strings.Contains(s, "think off") || strings.Contains(s, "think high") {
		t.Fatalf("a level chip appeared although the provider has none:\n%s", s)
	}
}
