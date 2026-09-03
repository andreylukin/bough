package theme

import (
	"strings"
	"testing"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
)

// Every palette sets the same tokens in a parseable style syntax, so
// switching never leaves a token unstyled or fails the ui mount.
func TestPalettesAreComplete(t *testing.T) {
	want := []string{"user", "assistant", "code", "result", "error", "accent", "dim", "border", "status", "focus", "select", "system", "markdown"}
	for name, p := range palettes {
		for _, tok := range want {
			if _, ok := p[tok]; !ok {
				t.Errorf("%s: missing token %q", name, tok)
			}
		}
		if len(p) != len(want) {
			t.Errorf("%s: %d tokens, want %d", name, len(p), len(want))
		}
		if md := p["markdown"]; md != "dark" && md != "light" && md != "" {
			t.Errorf("%s: markdown = %q", name, md)
		}
	}
	if _, ok := palettes[defaultName]; !ok {
		t.Fatalf("default palette %q missing", defaultName)
	}
}

func mount(t *testing.T, cfg map[string]any) (*kernel.Context, *commands.Registry) {
	t.Helper()
	ctx := kernel.NewContext()
	reg := commands.NewRegistry()
	ctx.Provide("commands", reg)
	if err := (plugin{}).Apply(ctx, cfg); err != nil {
		t.Fatal(err)
	}
	return ctx, reg
}

func TestMountProvidesPaletteAndThemeCommand(t *testing.T) {
	ctx, reg := mount(t, nil)
	p, err := kernel.Get[map[string]string](ctx, "palette")
	if err != nil {
		t.Fatal(err)
	}
	if p["user"] != palettes["forest"]["user"] || p["markdown"] != "dark" {
		t.Fatalf("palette = %v", p)
	}
	out, err := reg.Run("theme", "")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out, "● forest") || !strings.Contains(out, "  nord") {
		t.Fatalf("list = %q", out)
	}
	if _, err := reg.Run("theme", "neon"); err == nil || !strings.Contains(err.Error(), "unknown palette") {
		t.Fatalf("unknown name err = %v", err)
	}
	if out, _ := reg.Run("theme", "forest"); !strings.Contains(out, "already") {
		t.Fatalf("same name = %q", out)
	}
	// A switch on a bare mount (no config tree, no config-set) is a loud error.
	if _, err := reg.Run("theme", "nord"); err == nil || !strings.Contains(err.Error(), "theme row") {
		t.Fatalf("switch without config-set err = %v", err)
	}
}

func TestUnknownNameFailsMount(t *testing.T) {
	ctx := kernel.NewContext()
	ctx.Provide("commands", commands.NewRegistry())
	err := (plugin{}).Apply(ctx, map[string]any{"name": "neon"})
	if err == nil || !strings.Contains(err.Error(), "unknown palette") {
		t.Fatalf("err = %v", err)
	}
	if err := (plugin{}).Apply(ctx, map[string]any{"name": 3}); err == nil {
		t.Fatal("non-string name should fail")
	}
}
