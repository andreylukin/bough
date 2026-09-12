package commands

// /think: how hard the model thinks, changed at runtime.
//
// plugins/llm has documented Efforter as "the seam for changing how
// hard the model thinks at runtime (the /think command)" for a while,
// but no such command existed — the levels were reachable only through
// the TUI's shift+tab, which left headless sessions and anything
// driving bough over stdin with no way to ask for more or less
// reasoning. This is that command.

import (
	"fmt"
	"strings"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"
)

// registerThink installs /think. Like /model, every service it needs is
// resolved lazily at Run time, so there is no mount-order dependency.
func registerThink(r *Registry, ctx *kernel.Context) error {
	return r.Register(
		CommandInfo{
			Name:    "think",
			Usage:   "[" + strings.Join(llm.Efforts, " | ") + "]",
			Summary: "how hard the model thinks; bare /think shows the current level",
		},
		func(args string) (string, error) { return runThink(ctx, args) },
	)
}

func runThink(ctx *kernel.Context, args string) (string, error) {
	e, err := kernel.Get[llm.Efforter](ctx, "llm")
	if err != nil {
		// Not every provider has reasoning levels; say which one is
		// mounted rather than implying the command is broken.
		return "", fmt.Errorf("think: this llm row does not support reasoning levels")
	}
	level := strings.ToLower(strings.TrimSpace(args))
	if level == "" {
		cur := e.Effort()
		if cur == "" {
			cur = "the provider's default"
		}
		return fmt.Sprintf("thinking: %s\nlevels: %s", cur, strings.Join(llm.Efforts, ", ")), nil
	}
	if !llm.ValidEffort(level) {
		return "", fmt.Errorf("think: %q is not a level (have %s)", level, strings.Join(llm.Efforts, ", "))
	}
	if err := e.SetEffort(level); err != nil {
		return "", fmt.Errorf("think: %w", err)
	}
	return "thinking: " + level, nil
}
