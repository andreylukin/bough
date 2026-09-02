package commands

// /model tests: listing, live provider swap through a launcher-shaped
// "config-set" service, shorthand model change, loud errors. Two stub
// llm-* plugins registered here stand in for real providers — the
// "llm" service they provide is a plain string, so the tests can
// assert exactly which provider (and model config) is live.

import (
	"strings"
	"testing"

	"github.com/andreylukin/bough/kernel"
)

// stubLLM is a fake provider plugin: provides "llm" as the string
// "<val>" or "<val>:<model>" when the row config has a model.
type stubLLM struct{ name, val string }

func (s stubLLM) Name() string     { return s.name }
func (s stubLLM) Inject() []string { return nil }
func (s stubLLM) Apply(ctx *kernel.Context, cfg map[string]any) error {
	v := s.val
	if m, ok := cfg["model"].(string); ok && m != "" {
		v += ":" + m
	}
	ctx.Provide("llm", v)
	return nil
}

func init() {
	kernel.Register("llm-stub", func() kernel.Plugin { return stubLLM{name: "llm-stub", val: "stub-one"} })
	kernel.Register("llm-stub2", func() kernel.Plugin { return stubLLM{name: "llm-stub2", val: "stub-two"} })
}

// applyTestSets is the test's copy of the launcher's override applier:
// "id.key=value", with key "plugin" swapping the row's plugin.
func applyTestSets(rows []kernel.Row, sets []string) {
	for _, s := range sets {
		eq := strings.IndexByte(s, '=')
		path, value := s[:eq], s[eq+1:]
		dot := strings.IndexByte(path, '.')
		id, key := path[:dot], path[dot+1:]
		for i := range rows {
			if rows[i].ID != id {
				continue
			}
			if key == "plugin" {
				rows[i].Plugin = value
			} else {
				if rows[i].Config == nil {
					rows[i].Config = map[string]any{}
				}
				rows[i].Config[key] = value
			}
		}
	}
}

// mountModelTree mounts llm-stub + commands with a launcher-shaped
// "config-set" service (base rows + accumulated sets -> Reconcile).
func mountModelTree(t *testing.T) (*kernel.Context, *Registry) {
	t.Helper()
	base := []kernel.Row{
		{ID: "llm", Plugin: "llm-stub", Config: map[string]any{"model": "m1"}},
		{ID: "commands", Plugin: "commands"},
	}
	ctx := kernel.NewContext()
	var recorded []string
	ctx.Provide("config-set", func(sets ...string) error {
		all := append(append([]string(nil), recorded...), sets...)
		next := make([]kernel.Row, len(base))
		for i, r := range base {
			next[i] = r
			cp := make(map[string]any, len(r.Config))
			for k, v := range r.Config {
				cp[k] = v
			}
			next[i].Config = cp
		}
		applyTestSets(next, all)
		if err := ctx.Reconcile(next); err != nil {
			return err
		}
		recorded = all
		return nil
	})
	if err := ctx.Mount(base); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(ctx.Unmount)
	r, err := kernel.Get[*Registry](ctx, "commands")
	if err != nil {
		t.Fatal(err)
	}
	return ctx, r
}

func liveLLM(t *testing.T, ctx *kernel.Context) string {
	t.Helper()
	v, err := kernel.Get[string](ctx, "llm")
	if err != nil {
		t.Fatalf("live llm service: %v", err)
	}
	return v
}

func TestModelShowsCurrentAndProviders(t *testing.T) {
	_, r := mountModelTree(t)
	out, err := r.Run("model", "")
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"model: llm-stub · m1", "llm-stub2", "usage: /model"} {
		if !strings.Contains(out, want) {
			t.Fatalf("/model output missing %q:\n%s", want, out)
		}
	}
	// The palette/help listing carries the usage hint.
	found := false
	for _, in := range r.List() {
		if in.Name == "model" {
			found = true
			if in.Usage == "" || in.Summary == "" {
				t.Fatalf("/model listed without usage/summary: %+v", in)
			}
		}
	}
	if !found {
		t.Fatal("/model not in the command listing")
	}
}

// With no args, a known provider gets a short "try:" list of model
// ids; a stub provider has none to suggest.
func TestModelShowSuggestsModels(t *testing.T) {
	row := kernel.Row{ID: "llm", Plugin: "llm-openrouter", Config: map[string]any{"model": "x/y"}}
	out := showModel(row, []string{"llm-openrouter"})
	for _, want := range []string{"model: llm-openrouter · x/y", "try: anthropic/claude-sonnet-4.5", "usage: /model"} {
		if !strings.Contains(out, want) {
			t.Fatalf("showModel missing %q:\n%s", want, out)
		}
	}
	if out := showModel(kernel.Row{Plugin: "llm-stub"}, nil); strings.Contains(out, "try:") {
		t.Fatalf("stub provider should suggest nothing:\n%s", out)
	}
}

func TestModelSwapsProvider(t *testing.T) {
	ctx, r := mountModelTree(t)
	if got := liveLLM(t, ctx); got != "stub-one:m1" {
		t.Fatalf("pre-swap llm = %q", got)
	}
	out, err := r.Run("model", "llm-stub2")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out, "llm-stub2") {
		t.Fatalf("swap output = %q, want the new provider named", out)
	}
	// The LIVE "llm" service changed, not just the config.
	if got := liveLLM(t, ctx); got != "stub-two:m1" {
		t.Fatalf("post-swap llm = %q, want stub-two:m1", got)
	}

	// provider + model in one go
	if _, err := r.Run("model", "llm-stub mx"); err != nil {
		t.Fatal(err)
	}
	if got := liveLLM(t, ctx); got != "stub-one:mx" {
		t.Fatalf("post provider+model swap llm = %q, want stub-one:mx", got)
	}
}

func TestModelShorthandKeepsPlugin(t *testing.T) {
	ctx, r := mountModelTree(t)
	out, err := r.Run("model", "m9")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out, "llm-stub · m9") {
		t.Fatalf("shorthand output = %q, want same plugin, new model", out)
	}
	if got := liveLLM(t, ctx); got != "stub-one:m9" {
		t.Fatalf("post-shorthand llm = %q, want stub-one:m9", got)
	}
}

func TestModelErrors(t *testing.T) {
	_, r := mountModelTree(t)
	if _, err := r.Run("model", "llm-nope"); err == nil ||
		!strings.Contains(err.Error(), "unknown provider") ||
		!strings.Contains(err.Error(), "llm-stub") {
		t.Fatalf("unknown provider error = %v", err)
	}
	if _, err := r.Run("model", "a b c"); err == nil || !strings.Contains(err.Error(), "usage") {
		t.Fatalf("too-many-args error = %v", err)
	}
	if _, err := r.Run("model", "sonnet extra"); err == nil ||
		!strings.Contains(err.Error(), "not a registered provider") {
		t.Fatalf("two-args-non-provider error = %v", err)
	}
}

func TestModelWithoutConfigSetService(t *testing.T) {
	// A bare tree (no launcher): showing works, swapping errors loud.
	ctx := kernel.NewContext()
	rows := []kernel.Row{
		{ID: "llm", Plugin: "llm-stub"},
		{ID: "commands", Plugin: "commands"},
	}
	if err := ctx.Mount(rows); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(ctx.Unmount)
	r, err := kernel.Get[*Registry](ctx, "commands")
	if err != nil {
		t.Fatal(err)
	}
	if out, err := r.Run("model", ""); err != nil || !strings.Contains(out, "model: llm-stub") {
		t.Fatalf("show without config-set = (%q, %v)", out, err)
	}
	if _, err := r.Run("model", "llm-stub2"); err == nil ||
		!strings.Contains(err.Error(), "config-set") {
		t.Fatalf("swap without config-set error = %v", err)
	}
}

func TestModelNoLLMRow(t *testing.T) {
	ctx := kernel.NewContext()
	if err := ctx.Mount([]kernel.Row{{ID: "commands", Plugin: "commands"}}); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(ctx.Unmount)
	r, err := kernel.Get[*Registry](ctx, "commands")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := r.Run("model", ""); err == nil || !strings.Contains(err.Error(), "no llm row") {
		t.Fatalf("no-llm-row error = %v", err)
	}
}
