// Package theme is the "theme" plugin: bundled color palettes and the
// /theme command, maki-style (`ui.theme = "tokyonight"` there).
//
// The row's `name` picks a bundled palette (default "forest"); the
// plugin provides it as the "palette" service, which the ui applies
// under the init.js "theme" overrides — so a whole scheme comes from
// here and a one-token tweak still comes from init.js. /theme lists
// the palettes; /theme <name> swaps the row's config through the
// launcher's config-set service, so the ui remounts with the new
// colors at once (the pick lasts the session; put `name:` in the
// theme row to keep it).
package theme

import (
	"fmt"
	"sort"
	"strings"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
)

const defaultName = "forest"

type plugin struct{}

func init() {
	kernel.Register("theme", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "theme" }
func (plugin) Inject() []string { return []string{"commands"} }

// Names lists the bundled palettes, sorted.
func Names() []string {
	out := make([]string, 0, len(palettes))
	for n := range palettes {
		out = append(out, n)
	}
	sort.Strings(out)
	return out
}

// Palette returns a copy of the named palette; ok=false when unknown.
func Palette(name string) (map[string]string, bool) {
	p, ok := palettes[name]
	if !ok {
		return nil, false
	}
	out := make(map[string]string, len(p))
	for k, v := range p {
		if v != "" {
			out[k] = v
		}
	}
	return out, true
}

// nameOf reads the row's `name`, defaulting to forest.
func nameOf(cfg map[string]any) (string, error) {
	v, has := cfg["name"]
	if !has {
		return defaultName, nil
	}
	s, ok := v.(string)
	if !ok || strings.TrimSpace(s) == "" {
		return "", fmt.Errorf("theme: name must be a non-empty string, got %v", v)
	}
	return s, nil
}

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	name, err := nameOf(cfg)
	if err != nil {
		return err
	}
	p, ok := Palette(name)
	if !ok {
		return fmt.Errorf("theme: unknown palette %q (have: %s)", name, strings.Join(Names(), ", "))
	}
	ctx.Provide("palette", p)

	reg, err := kernel.Get[*commands.Registry](ctx, "commands")
	if err != nil {
		return err
	}
	if err := reg.Register(
		commands.CommandInfo{Name: "theme", Usage: "[name]", Summary: "list color themes or switch to one"},
		func(args string) (string, error) { return runTheme(ctx, name, args) },
	); err != nil {
		return err
	}
	ctx.Effect(func() { reg.Unregister("theme") })
	return nil
}

// list renders the palettes with the current one marked.
func list(current string) string {
	var b strings.Builder
	b.WriteString("themes:\n")
	for _, n := range Names() {
		mark := "  "
		if n == current {
			mark = "● "
		}
		b.WriteString("  " + mark + n + "\n")
	}
	b.WriteString("usage: /theme <name> (this session; set `name:` on the theme row to keep it)")
	return b.String()
}

func runTheme(ctx *kernel.Context, current, args string) (string, error) {
	want := strings.TrimSpace(args)
	if want == "" {
		return list(current), nil
	}
	if _, ok := palettes[want]; !ok {
		return "", fmt.Errorf("theme: unknown palette %q\n%s", want, list(current))
	}
	if want == current {
		return "theme: " + want + " (already)", nil
	}
	row := ""
	for _, r := range ctx.Desired() {
		if r.Plugin == "theme" {
			row = r.ID
			break
		}
	}
	if row == "" {
		return "", fmt.Errorf("theme: no theme row in the config tree")
	}
	set, err := kernel.Get[func(...string) error](ctx, "config-set")
	if err != nil {
		return "", fmt.Errorf("theme: no config-set service (a live switch needs the bough launcher)")
	}
	if err := set(row + ".name=" + want); err != nil {
		return "", err
	}
	for _, rs := range ctx.Rows() {
		if rs.ID == row && rs.State == kernel.StateFailed {
			return "", fmt.Errorf("theme: switch failed: %v", rs.Err)
		}
	}
	return "theme: " + want, nil
}
