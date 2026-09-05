package prompts

import (
	"errors"
	"os"
	"path/filepath"
	"slices"
	"testing"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
	"strings"
)

// addTemplate writes <dir>/<name>.md with body.
func addTemplate(t *testing.T, dir, name, body string) {
	t.Helper()
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, name+".md"), []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

// onDisk drops the templates that ship in the binary, so the tests
// about scanning directories stay about scanning directories.
func onDisk(ts []Template) []Template {
	var out []Template
	for _, t := range ts {
		if t.Path != "" {
			out = append(out, t)
		}
	}
	return out
}

func TestScanProjectShadowsGlobal(t *testing.T) {
	global, project := t.TempDir(), t.TempDir()
	addTemplate(t, global, "review", "# Review (global)\nbody")
	addTemplate(t, global, "greet", "hello $ARGUMENTS")
	addTemplate(t, project, "review", "# Review (project)\nbody")
	if err := os.WriteFile(filepath.Join(project, "notes.txt"), []byte("not a template"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Mkdir(filepath.Join(project, "dir.md"), 0o755); err != nil {
		t.Fatal(err)
	}

	got := onDisk(New(global, project).Scan())
	if len(got) != 2 {
		t.Fatalf("Scan = %+v, want greet and review", got)
	}
	if got[0].Name != "greet" || got[0].Summary != "hello $ARGUMENTS" {
		t.Errorf("greet = %+v", got[0])
	}
	if got[1].Name != "review" || got[1].Path != filepath.Join(project, "review.md") ||
		got[1].Summary != "Review (project)" {
		t.Errorf("review should come from the project dir: %+v", got[1])
	}
}

func TestScanMissingDir(t *testing.T) {
	if got := onDisk(New(filepath.Join(t.TempDir(), "nope")).Scan()); len(got) != 0 {
		t.Errorf("Scan = %+v, want none", got)
	}
}

func TestSummaryHeadingBeatsFirstLine(t *testing.T) {
	dir := t.TempDir()
	addTemplate(t, dir, "a", "\n\nfirst line\n## Heading here\nmore")
	addTemplate(t, dir, "b", "\nplain first\nsecond")
	addTemplate(t, dir, "c", "")
	got := New(dir).Scan()
	want := []string{"Heading here", "plain first", ""}
	for i, s := range want {
		if got[i].Summary != s {
			t.Errorf("%s summary = %q, want %q", got[i].Name, got[i].Summary, s)
		}
	}
}

func TestExpand(t *testing.T) {
	cases := []struct{ body, args, want string }{
		{"say hi to $ARGUMENTS", "bob smith", "say hi to bob smith"},
		{"first $1, second $2, missing $3.", "bob smith", "first bob, second smith, missing ."},
		{"all: $ARGUMENTS / one: $1", "  x  y ", "all: x  y / one: x"},
		{"no args: [$ARGUMENTS] [$1]", "", "no args: [] []"},
		{"cost is $5 or $ten", "", "cost is  or $ten"},
		{"$1 then $10 then $12x", "a", "a then $10 then $12x"},
		{"plain body", "ignored", "plain body"},
	}
	for _, c := range cases {
		if got := Expand(c.body, c.args); got != c.want {
			t.Errorf("Expand(%q, %q) = %q, want %q", c.body, c.args, got, c.want)
		}
	}
}

// Registered templates are "/name" commands whose Run submits the
// expanded body; the file is read at dispatch, and an unmount
// unregisters them.
func TestRegisterCommands(t *testing.T) {
	dir := t.TempDir()
	addTemplate(t, dir, "greet", "# Greet someone\nsay hello to $ARGUMENTS\n")
	addTemplate(t, dir, "empty", "")
	ctx := kernel.NewContext()
	reg := commands.NewRegistry()
	New(dir).registerCommands(ctx, reg)

	var infos []commands.CommandInfo
	for _, in := range reg.List() {
		if in.Name != "init" { // ships in the binary; see TestBuiltinInit
			infos = append(infos, in)
		}
	}
	if len(infos) != 2 || infos[0].Name != "empty" || infos[1].Name != "greet" {
		t.Fatalf("List = %+v", infos)
	}
	if in := infos[1]; !in.IsTemplate() || in.Summary != "template: Greet someone" || in.Usage != "[args]" {
		t.Errorf("greet info = %+v", in)
	}

	_, err := reg.Run("greet", "bob")
	act, ok := errors.AsType[commands.UIAction](err)
	if !ok {
		t.Fatalf("Run err = %v, want a UIAction", err)
	}
	if text, ok := commands.SubmitText(act); !ok || text != "# Greet someone\nsay hello to bob" {
		t.Errorf("submit text = %q, %v", text, ok)
	}

	_, err = reg.Run("empty", "")
	if _, isAct := errors.AsType[commands.UIAction](err); err == nil || isAct {
		t.Errorf("empty template should error, got %v", err)
	}

	ctx.Unmount()
	if _, err := reg.Run("greet", ""); err == nil || err.Error() != "unknown command: /greet (try /help)" {
		t.Errorf("after unmount: %v", err)
	}
}

// A remount (the reload path) rescans: a changed summary, a template
// added since, and a removed one all show in the new set.
func TestRegisterCommandsRescansOnRemount(t *testing.T) {
	dir := t.TempDir()
	addTemplate(t, dir, "greet", "# Greet\nhi $1\n")
	addTemplate(t, dir, "old", "gone soon\n")
	reg := commands.NewRegistry()
	tp := New(dir)
	ctx := kernel.NewContext()
	tp.registerCommands(ctx, reg)
	ctx.Unmount()

	addTemplate(t, dir, "greet", "# Wave\nhi $1\n")
	addTemplate(t, dir, "review", "look at $ARGUMENTS\n")
	if err := os.Remove(filepath.Join(dir, "old.md")); err != nil {
		t.Fatal(err)
	}
	ctx = kernel.NewContext()
	tp.registerCommands(ctx, reg)
	var got []string
	for _, in := range reg.List() {
		if in.Name == "init" {
			continue // ships in the binary; see TestBuiltinInit
		}
		got = append(got, in.Name+": "+in.Summary)
	}
	want := []string{"greet: template: Wave", "review: template: look at $ARGUMENTS"}
	if !slices.Equal(got, want) {
		t.Fatalf("List after remount = %v, want %v", got, want)
	}
	if _, err := reg.Run("old", ""); err == nil {
		t.Error("removed template should be gone after remount")
	}
	ctx.Unmount()
}

// /init ships in the binary: a fresh install has no prompt files, and
// "write an AGENTS.md for this project" is the one every project wants
// on day one. Both opencode and Claude Code have it.
func TestBuiltinInit(t *testing.T) {
	got := New(t.TempDir()).Scan()
	var init *Template
	for i := range got {
		if got[i].Name == "init" {
			init = &got[i]
		}
	}
	if init == nil {
		t.Fatal("/init should be available with no prompt files at all")
	}
	if init.Path != "" {
		t.Errorf("a bundled template has no path, got %q", init.Path)
	}
	body, err := init.Read()
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"AGENTS.md", "build, test and run", "verified by\n  running them"} {
		if !strings.Contains(body, want) {
			t.Errorf("the init template should mention %q", want)
		}
	}
	if init.Summary == "" {
		t.Error("a bundled template needs a palette summary")
	}
}

// A user's own init.md wins: bundled templates are a starting point,
// not a fixture.
func TestUserTemplateShadowsBuiltin(t *testing.T) {
	dir := t.TempDir()
	addTemplate(t, dir, "init", "# my own init\ndo it my way\n")
	for _, tp := range New(dir).Scan() {
		if tp.Name != "init" {
			continue
		}
		if tp.Path == "" {
			t.Fatal("a user template should shadow the bundled one")
		}
		body, _ := tp.Read()
		if !strings.Contains(body, "do it my way") {
			t.Errorf("wrong body: %q", body)
		}
		return
	}
	t.Fatal("init missing")
}
