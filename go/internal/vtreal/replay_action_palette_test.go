package vtreal

// The action palette and the leader key on a real PTY: ctrl+x arms a
// pending chord, "p" opens the "/" palette over the action rows alone,
// typing filters, enter runs the row (collapse_all is observable),
// esc closes and gives back the draft the "/" displaced.

import (
	"strings"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"
)

// actionPaletteLeader presses the leader, waits for the pending flash,
// then sends the chord key.
func actionPaletteLeader(a *app, chord rune) {
	a.t.Helper()
	a.key('x', uv.ModCtrl)
	a.waitFor("ctrl+x …")
	a.key(chord, 0)
}

// actionPaletteCode runs one echo code turn; its block starts collapsed.
func actionPaletteCode(a *app) {
	a.t.Helper()
	a.typeText("CODE!")
	a.key(uv.KeyEnter, 0)
	a.waitFor("hi from codemode")
	a.waitFor("▸ Ran")
}

// actionPaletteComposer is the composer's row text, "" when off screen.
func actionPaletteComposer(s string) string {
	ls := strings.Split(s, "\n")
	if r := composerRow(ls); r >= 0 {
		return ls[r]
	}
	return ""
}

func TestActionPaletteOpensOnActionRows(t *testing.T) {
	t.Parallel()
	a := start(t, 100, 30)
	actionPaletteLeader(a, 'p')
	a.waitFor("collapse_all")
	s := a.settled()
	// Action rows: "name key" plus the dim "action · " tag.
	for _, want := range []string{"action · ", "clear_input", "ctrl+l"} {
		if !strings.Contains(s, want) {
			t.Errorf("palette misses %q:\n%s", want, s)
		}
	}
	// Actions mode lists actions alone: no command rows.
	if strings.Contains(s, "/help") {
		t.Errorf("actions mode lists commands:\n%s", s)
	}
	if c := actionPaletteComposer(s); !strings.HasPrefix(c, "> /") {
		t.Errorf("composer does not hold the palette's \"/\" (%q):\n%s", c, s)
	}
}

func TestActionPaletteFilters(t *testing.T) {
	t.Parallel()
	a := start(t, 100, 30)
	actionPaletteLeader(a, 'p')
	a.waitFor("collapse_all")
	a.typeText("expa")
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "collapse_all") }, "filter to drop collapse_all")
	s := a.settled()
	if !strings.Contains(s, "expand_all ctrl+x e") {
		t.Errorf("filter lost expand_all (with its chord as the key):\n%s", s)
	}
	for _, gone := range []string{"sessions", "clear_input", "todo_toggle"} {
		if strings.Contains(s, gone) {
			t.Errorf("filter %q kept %q:\n%s", "expa", gone, s)
		}
	}
	// A query matching no action: enter says so and sends nothing.
	a.typeText("zzz")
	a.key(uv.KeyEnter, 0)
	a.waitFor(`no action matches "expazzz"`)
}

func TestActionPaletteEnterRunsCollapseAll(t *testing.T) {
	t.Parallel()
	a := start(t, 100, 30)
	actionPaletteCode(a)
	// The chord first: expand everything.
	actionPaletteLeader(a, 'e')
	a.waitFor("expanded 3 blocks")
	a.waitFor("▾ Ran")
	// Then the palette row: collapse_all folds it back.
	actionPaletteLeader(a, 'p')
	a.waitFor("collapse_all")
	a.typeText("collapse_a")
	a.key(uv.KeyEnter, 0)
	a.waitUntil(func(s string) bool { return strings.Contains(s, " collapsed 3 blocks") }, "collapse_all flash")
	s := a.settled()
	if strings.Contains(s, "▾ Ran") || !strings.Contains(s, "▸ Ran") {
		t.Errorf("collapse_all from the palette left the block open:\n%s", s)
	}
	if strings.Contains(s, "action · ") {
		t.Errorf("palette still open after enter:\n%s", s)
	}
	if c := actionPaletteComposer(s); strings.Contains(c, "/") {
		t.Errorf("the palette's \"/\" stayed in the composer (%q):\n%s", c, s)
	}
}

func TestActionPaletteEscRestoresDraft(t *testing.T) {
	t.Parallel()
	a := start(t, 100, 30)
	a.typeText("half a thought")
	a.waitFor("> half a thought")
	actionPaletteLeader(a, 'p')
	a.waitFor("collapse_all")
	if s := a.settled(); strings.Contains(s, "half a thought") {
		t.Errorf("the \"/\" did not displace the draft:\n%s", s)
	}
	a.key(uv.KeyEscape, 0)
	a.waitFor("> half a thought")
	if s := a.settled(); strings.Contains(s, "action · ") {
		t.Errorf("palette still open after esc:\n%s", s)
	}
}

func TestActionPaletteLeaderChords(t *testing.T) {
	t.Parallel()
	a := start(t, 100, 30)
	// Steps share one app, so they run in order rather than as t.Run
	// subtests (a.waitFor fails through the parent t).
	actionPaletteLeader(a, 'z') // not a chord: the flash names it
	a.waitFor("ctrl+x z: no such chord")

	actionPaletteLeader(a, uv.KeyEscape) // esc backs out quietly
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "ctrl+x …") }, "pending leader to clear")
	if s := a.settled(); strings.Contains(s, "no such chord") {
		t.Errorf("esc after the leader flashed:\n%s", s)
	}

	actionPaletteCode(a) // c runs collapse_all; the block is already folded
	actionPaletteLeader(a, 'c')
	a.waitFor("nothing to collapse")

	actionPaletteLeader(a, 'k') // k lists the chords
	for _, want := range []string{"ctrl+x c", "ctrl+x p", "open the action palette"} {
		a.waitFor(want)
	}
}
