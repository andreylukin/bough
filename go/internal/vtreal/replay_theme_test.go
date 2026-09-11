package vtreal

// Themes and colour on a real terminal: the palette a theme row picks
// reaches the cells (the user marker, the code box text and border),
// /theme repaints a session that is already on screen, and a NO_COLOR
// boot with no COLORTERM draws the same layout with no colour at all.
//
// The tape is replayed, so the turn under the colours is deterministic.

import (
	"fmt"
	"image/color"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// themeConfig is replayConfig plus a theme row and open code blocks
// (a collapsed block never draws the box this scenario asserts on).
func themeConfig(tape, name string) string {
	return replayConfig(tape) + fmt.Sprintf(`
- id: theme
  plugin: theme
  config: {name: %s}
- id: ui
  plugin: ui
  config: {collapse: none}
`, name)
}

// themeStart is startCfg with control over the colour environment:
// startCfg always boots with COLORTERM=truecolor and NO_COLOR empty.
// env entries are "K=V"; a "K=" entry unsets K.
func themeStart(t *testing.T, cols, rows int, yml string, env ...string) *app {
	t.Helper()
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, cols, rows)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	// Build the environment by hand: execve on most systems takes the
	// FIRST of a duplicated key, so appending an override is not enough.
	want := map[string]string{
		"HOME": home, "TERM": "xterm-256color", "COLORTERM": "truecolor",
		"NO_COLOR": "", "BOUGH_VERBOSE": "",
	}
	for _, e := range env {
		k, v, _ := strings.Cut(e, "=")
		want[k] = v
	}
	var out []string
	for _, e := range os.Environ() {
		k, _, _ := strings.Cut(e, "=")
		if _, override := want[k]; !override {
			out = append(out, e)
		}
	}
	for k, v := range want {
		if v != "" {
			out = append(out, k+"="+v)
		}
	}
	cmd.Env = out
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: cols, rows: rows, home: home}
	t.Cleanup(func() {
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	a.waitFor("say something")
	return a
}

// themeTape is the fixture this file drives.
func themeTape(t *testing.T) string {
	t.Helper()
	p, err := filepath.Abs("testdata/replay/theme.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	return p
}

// themeRunTurn submits the tape's one prompt and waits for the turn.
func themeRunTurn(a *app) {
	a.t.Helper()
	a.typeText("paint something")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 60*time.Second) {
		a.t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.settled()
}

// themeHex renders a cell colour as "#rrggbb"; ok is false for an
// unset colour.
func themeHex(c color.Color) (string, bool) {
	if c == nil {
		return "", false
	}
	r, g, b, _ := c.RGBA()
	return fmt.Sprintf("#%02x%02x%02x", r>>8, g>>8, b>>8), true
}

// themeIs24Bit reports whether a cell colour came from a 24-bit SGR
// (38;2;r;g;b). x/vt keeps indexed colours as their own types and only
// true colour as a concrete RGBA value.
func themeIs24Bit(c color.Color) bool {
	if c == nil {
		return false
	}
	switch c.(type) {
	case color.RGBA, *color.RGBA, color.NRGBA, *color.NRGBA:
		return true
	}
	return false
}

// themeMarker returns the ❯ cell of the user line "❯ paint something"
// (a "/theme" line has a ❯ too, drawn dim).
func themeMarker(a *app) (uv.Cell, bool) {
	snap := a.term.Snapshot()
	for y, l := range strings.Split(a.text(), "\n") {
		if strings.HasPrefix(l, "❯ paint something") && y < len(snap.Cells) {
			return snap.Cells[y][0], true
		}
	}
	return uv.Cell{}, false
}

// themeScrub removes what legitimately differs between two runs of the
// same tape: elapsed times and token/cost counters.
var themeScrub = regexp.MustCompile(`\d+(\.\d+)?\s?(ms|s|k|%|\$)`)

func themeNormalize(s string) string {
	var out []string
	for _, l := range strings.Split(s, "\n") {
		out = append(out, themeScrub.ReplaceAllString(l, "#"))
	}
	return strings.Join(out, "\n")
}

// The user marker carries the palette's "user" colour, bold, and it is
// the palette's — not some default.
func TestThemeUserMarkerUsesPalette(t *testing.T) {
	t.Parallel()
	a := themeStart(t, 100, 60, themeConfig(themeTape(t), "dracula"))
	themeRunTurn(a)
	c, ok := themeMarker(a)
	if !ok {
		t.Fatalf("no ❯ cell on screen:\n%s", a.text())
	}
	hex, has := themeHex(c.Style.Fg)
	if !has || hex != "#50fa7b" {
		t.Fatalf("❯ foreground is %q (%T), want dracula user #50fa7b:\n%s", hex, c.Style.Fg, a.text())
	}
	if c.Style.Attrs&uv.AttrBold == 0 {
		t.Fatalf("❯ is not bold (attrs=%b) though dracula user is \"#50fa7b:bold\":\n%s", c.Style.Attrs, a.text())
	}
}

// The code box draws its text in the palette's "code" colour and its
// rounded border in "border".
func TestThemeCodeBoxUsesPalette(t *testing.T) {
	t.Parallel()
	a := themeStart(t, 100, 60, themeConfig(themeTape(t), "nord"))
	themeRunTurn(a)
	snap := a.term.Snapshot()
	row := -1
	for y, l := range strings.Split(a.text(), "\n") {
		if strings.HasPrefix(l, `│ console.log("swatch")`) {
			row = y
			break
		}
	}
	if row < 1 {
		t.Fatalf("no open code box on screen:\n%s", a.text())
	}
	corner, edge, code := snap.Cells[row-1][0], snap.Cells[row][0], snap.Cells[row][2]
	for _, c := range []uv.Cell{corner, edge} {
		if hex, has := themeHex(c.Style.Fg); !has || hex != "#4c566a" {
			t.Fatalf("box border %q is %q, want nord border #4c566a:\n%s", c.Content, hex, a.text())
		}
	}
	if hex, has := themeHex(code.Style.Fg); !has || hex != "#d8dee9" {
		t.Fatalf("code text is %q, want nord code #d8dee9:\n%s", hex, a.text())
	}
}

// /theme lists the palettes and switching repaints the transcript that
// is already on screen — the user marker changes colour in place.
func TestThemeSwitchMidSession(t *testing.T) {
	t.Parallel()
	a := themeStart(t, 100, 60, themeConfig(themeTape(t), "forest"))
	themeRunTurn(a)
	marker := func(where string) uv.Cell {
		a.t.Helper()
		c, ok := themeMarker(a)
		if !ok {
			t.Fatalf("%s: no ❯ cell on screen:\n%s", where, a.text())
		}
		return c
	}
	if hex, _ := themeHex(marker("before").Style.Fg); hex != "#a7c080" {
		t.Fatalf("before the switch ❯ is %q, want forest user #a7c080:\n%s", hex, a.text())
	}

	a.typeText("/theme")
	a.key(uv.KeyEnter, 0)
	a.waitFor("themes:")
	if s := a.settled(); !strings.Contains(s, "dracula") || !strings.Contains(s, "● forest") {
		t.Fatalf("/theme did not list the palettes with forest current:\n%s", s)
	}

	a.typeText("/theme nord")
	a.key(uv.KeyEnter, 0)
	a.waitFor("theme: nord")
	a.settled()
	a.waitUntil(func(string) bool {
		hex, _ := themeHex(marker("after").Style.Fg)
		return hex == "#a3be8c"
	}, "the ❯ marker to repaint in nord user #a3be8c")
	if !strings.Contains(a.text(), "paint something") {
		t.Fatalf("the transcript did not survive the theme switch:\n%s", a.text())
	}
}

// NO_COLOR=1 with no COLORTERM: bough boots and runs a turn with no
// 24-bit colour anywhere on screen, and the layout is the one the
// coloured run draws.
func TestThemeNoColorBoot(t *testing.T) {
	t.Parallel()
	tape := themeTape(t)
	cfg := themeConfig(tape, "dracula")

	plain := themeStart(t, 100, 60, cfg, "NO_COLOR=1", "COLORTERM=")
	themeRunTurn(plain)
	snap := plain.term.Snapshot()
	for y, row := range snap.Cells {
		for x, c := range row {
			if themeIs24Bit(c.Style.Fg) || themeIs24Bit(c.Style.Bg) {
				fg, _ := themeHex(c.Style.Fg)
				bg, _ := themeHex(c.Style.Bg)
				t.Fatalf("NO_COLOR=1: cell (%d,%d) %q carries 24-bit colour fg=%s bg=%s:\n%s",
					x, y, c.Content, fg, bg, plain.text())
			}
		}
	}
	plain.check("no-color turn")

	colored := themeStart(t, 100, 60, cfg)
	themeRunTurn(colored)
	// The detector is not vacuous: the coloured run does paint 24-bit.
	if c, ok := themeMarker(colored); !ok || !themeIs24Bit(c.Style.Fg) {
		t.Fatalf("coloured run: ❯ is not a 24-bit colour (%T); the NO_COLOR check proves nothing:\n%s", c.Style.Fg, colored.text())
	}
	if got, want := themeNormalize(plain.text()), themeNormalize(colored.text()); got != want {
		t.Fatalf("NO_COLOR changed the layout.\nno-color:\n%s\n\ncoloured:\n%s", plain.text(), colored.text())
	}
}
