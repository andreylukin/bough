package initjs

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
	"github.com/andreylukin/bough/plugins/loop"
)

// apply points HOME and cwd at temp dirs holding the fixture init
// files, mounts a real codemode VM, and runs the plugin's Apply.
func apply(t *testing.T, globalJS, projectJS string) (*kernel.Context, *codemode.CodeMode, error) {
	t.Helper()
	home := t.TempDir()
	proj := t.TempDir()
	t.Setenv("HOME", home)
	t.Chdir(proj)
	for _, f := range []struct{ dir, body string }{{home, globalJS}, {proj, projectJS}} {
		if f.body == "" {
			continue
		}
		if err := os.MkdirAll(filepath.Join(f.dir, ".bough"), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(f.dir, ".bough", "init.js"), []byte(f.body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	ctx := kernel.NewContext()
	cm := codemode.New(5 * time.Second)
	ctx.Provide("codemode", cm)
	ctx.Provide("commands", commands.NewRegistry())
	err := plugin{}.Apply(ctx, nil)
	return ctx, cm, err
}

func TestSetupTypoFailsApply(t *testing.T) {
	_, _, err := apply(t, "", `bough.setup({ui: {theem: {}}})`)
	if err == nil || !strings.Contains(err.Error(), "theem") {
		t.Fatalf("want error naming the typo key, got %v", err)
	}
}

func TestSetupTwicePerFileFails(t *testing.T) {
	_, _, err := apply(t, "", `bough.setup({}); bough.setup({})`)
	if err == nil || !strings.Contains(err.Error(), "twice") {
		t.Fatalf("want called-twice error, got %v", err)
	}
}

func TestBadStyleFailsApply(t *testing.T) {
	_, _, err := apply(t, "", `bough.setup({ui: {theme: {user: "reddish"}}})`)
	if err == nil || !strings.Contains(err.Error(), "reddish") {
		t.Fatalf("want bad-style error, got %v", err)
	}
}

func TestJSToolCallableViaRun(t *testing.T) {
	_, cm, err := apply(t, "", `bough.tool("greet", function(n) { return "hi " + n })`)
	if err != nil {
		t.Fatal(err)
	}
	out, err := cm.Run(`tools.greet("bough")`)
	if err != nil {
		t.Fatal(err)
	}
	if out != "hi bough" {
		t.Fatalf("tools.greet = %q, want %q", out, "hi bough")
	}
}

func TestJSProviderRoundTrip(t *testing.T) {
	ctx, _, err := apply(t, "", `
bough.provider("fake", function(system, messages) {
  return system + "|" + messages.length + "|" + messages[0].role + ":" + messages[0].content
})
bough.setup({provider: {default: "fake"}})
`)
	if err != nil {
		t.Fatal(err)
	}
	prov, err := kernel.Get[llm.LLM](ctx, "llm")
	if err != nil {
		t.Fatal(err)
	}
	got, err := prov.Complete(context.Background(), "sys", []llm.Message{{Role: "user", Content: "x"}})
	if err != nil {
		t.Fatal(err)
	}
	if got != "sys|1|user:x" {
		t.Fatalf("Complete = %q", got)
	}
}

func TestDefaultProviderUnregisteredFails(t *testing.T) {
	_, _, err := apply(t, "", `bough.setup({provider: {default: "nope"}})`)
	if err == nil || !strings.Contains(err.Error(), "nope") {
		t.Fatalf("want unknown-provider error, got %v", err)
	}
}

func TestProjectionOverride(t *testing.T) {
	ctx, _, err := apply(t, "", `
bough.project(function(entries) {
  return [{role: "user", content: "n=" + entries.length + " k=" + entries[0].kind}]
})
`)
	if err != nil {
		t.Fatal(err)
	}
	proj, err := kernel.Get[loop.Projection](ctx, "projection")
	if err != nil {
		t.Fatal(err)
	}
	msgs := proj.Project([]history.Entry{
		{Seq: 1, At: time.Now(), Kind: "input", Data: map[string]any{"text": "hello"}},
		{Seq: 2, At: time.Now(), Kind: "assistant", Data: map[string]any{"text": "hey"}},
	})
	if len(msgs) != 1 || msgs[0].Role != "user" || msgs[0].Content != "n=2 k=input" {
		t.Fatalf("Project = %+v", msgs)
	}
}

func TestThemeKeymapServices(t *testing.T) {
	ctx, _, err := apply(t,
		`bough.setup({ui: {theme: {user: "#ff0000:bold", accent: "213"}}})`,
		`bough.setup({ui: {theme: {user: "#00ff00"}, keymap: {quit: "q", scroll_up: "ctrl+u"}}})`)
	if err != nil {
		t.Fatal(err)
	}
	theme, err := kernel.Get[map[string]string](ctx, "theme")
	if err != nil {
		t.Fatal(err)
	}
	// project file overrides global; untouched global tokens survive
	if theme["user"] != "#00ff00" || theme["accent"] != "213" {
		t.Fatalf("theme = %v", theme)
	}
	keymap, err := kernel.Get[map[string]string](ctx, "keymap")
	if err != nil {
		t.Fatal(err)
	}
	if keymap["quit"] != "q" || keymap["scroll_up"] != "ctrl+u" {
		t.Fatalf("keymap = %v", keymap)
	}
}

// ui.keymap.leader and the nested ui.keymap.chords {key: action}
// object land in the keymap service, chords flattened to "chord:<key>".
func TestKeymapLeaderAndChords(t *testing.T) {
	ctx, _, err := apply(t, "",
		`bough.setup({ui: {keymap: {leader: "ctrl+g", chords: {x: "expand_all", q: "quit"}}}})`)
	if err != nil {
		t.Fatal(err)
	}
	keymap, err := kernel.Get[map[string]string](ctx, "keymap")
	if err != nil {
		t.Fatal(err)
	}
	if keymap["leader"] != "ctrl+g" || keymap["chord:x"] != "expand_all" || keymap["chord:q"] != "quit" {
		t.Fatalf("keymap = %v", keymap)
	}
	if _, _, err := apply(t, "", `bough.setup({ui: {keymap: {chords: "x"}}})`); err == nil ||
		!strings.Contains(err.Error(), "ui.keymap.chords is not an object") {
		t.Fatalf("a non-object chords should fail loud, got %v", err)
	}
}

func TestSystemAppendCognition(t *testing.T) {
	ctx, _, err := apply(t, "", `bough.setup({system: {append: "Always answer in haiku."}})`)
	if err != nil {
		t.Fatal(err)
	}
	cog, err := kernel.Get[loop.Cognition](ctx, "cognition")
	if err != nil {
		t.Fatal(err)
	}
	if got := cog.System("base"); got != "base\n\nAlways answer in haiku." {
		t.Fatalf("System = %q", got)
	}
}

func TestCognitionFn(t *testing.T) {
	ctx, _, err := apply(t, "", `bough.cognition(function(base) { return "PRE\n" + base })`)
	if err != nil {
		t.Fatal(err)
	}
	cog, err := kernel.Get[loop.Cognition](ctx, "cognition")
	if err != nil {
		t.Fatal(err)
	}
	if got := cog.System("base"); got != "PRE\nbase" {
		t.Fatalf("System = %q", got)
	}
}

func TestNoFilesProvidesNothing(t *testing.T) {
	ctx, _, err := apply(t, "", "")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := kernel.Get[map[string]string](ctx, "theme"); err == nil {
		t.Fatal("theme provided with no init files")
	}
	if _, err := kernel.Get[loop.Cognition](ctx, "cognition"); err == nil {
		t.Fatal("cognition provided with no init files")
	}
}

func TestSealedAfterInit(t *testing.T) {
	_, cm, err := apply(t, "", `bough.provider("p", function() { return "" })`)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := cm.Run(`bough.provider("late", function() { return "" })`); err == nil {
		t.Fatal("bough.provider after init should throw")
	}
	// live tool registration stays open
	if _, err := cm.Run(`bough.tool("late", function() { return "ok" }); tools.late()`); err != nil {
		t.Fatalf("live bough.tool: %v", err)
	}
}

func TestJSCommandRegistersAndRuns(t *testing.T) {
	ctx, _, err := apply(t, "", `
bough.command("shout", "<text>", "make it loud", function(args) {
  return args.toUpperCase() + "!"
})
`)
	if err != nil {
		t.Fatal(err)
	}
	r, err := kernel.Get[*commands.Registry](ctx, "commands")
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, in := range r.List() {
		if in.Name == "shout" {
			found = true
			if in.Usage != "<text>" || in.Summary != "make it loud" {
				t.Fatalf("shout info = %+v", in)
			}
		}
	}
	if !found {
		t.Fatalf("shout not listed: %v", r.List())
	}
	out, err := r.Run("shout", "hey there")
	if err != nil || out != "HEY THERE!" {
		t.Fatalf("Run shout = (%q, %v)", out, err)
	}
}

func TestJSCommandErrorSurfaces(t *testing.T) {
	ctx, _, err := apply(t, "", `
bough.command("boom", "", "always fails", function() { throw new Error("kaboom-742") })
`)
	if err != nil {
		t.Fatal(err)
	}
	r, err := kernel.Get[*commands.Registry](ctx, "commands")
	if err != nil {
		t.Fatal(err)
	}
	_, runErr := r.Run("boom", "")
	if runErr == nil || !strings.Contains(runErr.Error(), "kaboom-742") || !strings.Contains(runErr.Error(), "/boom") {
		t.Fatalf("Run boom error = %v", runErr)
	}
}

func TestJSCommandNonStringReturnErrors(t *testing.T) {
	ctx, _, err := apply(t, "", `
bough.command("num", "", "returns a number", function() { return 42 })
`)
	if err != nil {
		t.Fatal(err)
	}
	r, err := kernel.Get[*commands.Registry](ctx, "commands")
	if err != nil {
		t.Fatal(err)
	}
	if _, runErr := r.Run("num", ""); runErr == nil || !strings.Contains(runErr.Error(), "not string") {
		t.Fatalf("Run num error = %v", runErr)
	}
}

func TestJSCommandBadArgsFailApply(t *testing.T) {
	if _, _, err := apply(t, "", `bough.command("x", "usage")`); err == nil {
		t.Fatal("bough.command with 2 args should fail apply")
	}
	if _, _, err := apply(t, "", `bough.command("", "", "", function(){})`); err == nil {
		t.Fatal("bough.command with empty name should fail apply")
	}
}

func TestJSCommandDuplicateBuiltinFailsApply(t *testing.T) {
	_, _, err := apply(t, "", `bough.command("dup", "", "", function(){return ""})
bough.command("dup", "", "", function(){return ""})`)
	if err == nil || !strings.Contains(err.Error(), "already registered") {
		t.Fatalf("duplicate command error = %v", err)
	}
}
