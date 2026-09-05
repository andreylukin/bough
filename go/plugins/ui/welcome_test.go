package ui

// The startup header: what a fresh session says it has loaded
// (context files, skills, templates) and the keys line, each row
// omitted when empty and clipped to the width.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/plugins/commands"
)

type fakeContext []string

func (f fakeContext) Loaded() []string { return f }

type fakeSkills []string

func (f fakeSkills) Names() []string { return f }

// templateReg is a registry with a built-in and the named templates.
func templateReg(t *testing.T, names ...string) *commands.Registry {
	t.Helper()
	r := commands.NewRegistry()
	if err := r.Register(commands.CommandInfo{Name: "help", Kind: "builtin"},
		func(string) (string, error) { return "h", nil }); err != nil {
		t.Fatal(err)
	}
	for _, n := range names {
		if err := r.Register(commands.CommandInfo{Name: n, Kind: "template", Summary: "template: " + n},
			func(string) (string, error) { return "", commands.SubmitAction("expanded " + n) }); err != nil {
			t.Fatal(err)
		}
	}
	return r
}

// welcomeRows are the header's non-blank frame rows. The mark under
// the header (splash.go) is drawn from the density ramp alone, so its
// rows are dropped here: they are not the header under test.
func welcomeRows(d *drv) []string {
	var rows []string
	for _, l := range strings.Split(d.plain(), "\n") {
		if l = strings.TrimRight(l, " "); l != "" && strings.Trim(l, " .:-=+*#") != "" {
			rows = append(rows, l)
		}
	}
	return rows
}

func TestWelcomeHeaderListsLoaded(t *testing.T) {
	t.Parallel()
	home, err := os.UserHomeDir()
	if err != nil {
		t.Fatal(err)
	}
	cfg := cfgWith(t, nil, nil, nil)
	cfg.ctxmd = fakeContext{"AGENTS.md", filepath.Join(home, ".bough", "AGENTS.md")}
	cfg.skills = fakeSkills{"a", "b", "c", "d", "e", "f", "g"}
	cfg.cmds = templateReg(t, "review", "greet")
	d := newDrv(t, 80, 24, cfg)
	rows := welcomeRows(d)
	want := []string{
		"● bough — a coding agent",
		"  context: AGENTS.md, ~/.bough/AGENTS.md",
		"  skills: 7 (a, b, c, d, e, …)",
		"  templates: /greet /review",
		"  keys: ? for the list · / for commands · ! for shell",
		"  ask me to do something — I act by running code",
	}
	for i, w := range want {
		if i >= len(rows)-2 || rows[i] != w {
			t.Fatalf("header row %d = %q, want %q\n%s", i, rows[min(i, len(rows)-1)], w, d.plain())
		}
	}
	if n := len(rows) - 2; n > 8 { // minus status bar + composer
		t.Errorf("header is %d rows, want under 8", n)
	}
}

// Empty seams leave no line behind: no context files, no skills, a
// registry without templates.
func TestWelcomeHeaderOmitsEmptyLines(t *testing.T) {
	t.Parallel()
	cfg := cfgWith(t, nil, nil, nil)
	cfg.ctxmd = fakeContext{}
	cfg.skills = fakeSkills{}
	cfg.cmds = templateReg(t)
	d := newDrv(t, 80, 24, cfg)
	p := d.plain()
	for _, no := range []string{"context:", "skills:", "templates:"} {
		if strings.Contains(p, no) {
			t.Errorf("empty %q line should be omitted:\n%s", no, p)
		}
	}
	rows := welcomeRows(d)
	if len(rows) < 3 || rows[1] != "  keys: ? for the list · / for commands · ! for shell" {
		t.Errorf("keys line should follow the title:\n%s", p)
	}
}

// A narrow terminal clips each header line with an ellipsis rather
// than wrapping it onto extra rows.
func TestWelcomeHeaderClipsToWidth(t *testing.T) {
	t.Parallel()
	cfg := cfgWith(t, nil, nil, nil)
	cfg.skills = fakeSkills{"agent-browser", "exa", "host", "monarch", "parallel"}
	d := newDrv(t, 30, 24, cfg)
	rows := welcomeRows(d)
	if len(rows)-2 != 4 {
		t.Fatalf("want 4 header rows at width 30, got %d:\n%s", len(rows)-2, d.plain())
	}
	skills := rows[1]
	if !strings.HasPrefix(skills, "  skills: 5 (agent-browser") || !strings.HasSuffix(skills, "…") ||
		len([]rune(skills)) > 30 {
		t.Errorf("skills row should be clipped to 30 columns with …, got %q", skills)
	}
}
