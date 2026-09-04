package commands

// /think: how hard the model reasons, changed live. Reasoning models
// take an effort level per request; the llm row's `effort` config sets
// the starting point and this command overrides it for the rest of the
// session (a provider that cannot reason says so). The reasoning
// itself, when the provider streams it, shows as a collapsed
// "thinking" block above the reply.

import (
	"fmt"
	"strings"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"
)

// registerThink installs /think. The llm service is resolved lazily at
// Run time, so a /model swap mid-session is picked up.
func registerThink(r *Registry, ctx *kernel.Context) error {
	return r.Register(
		CommandInfo{Name: "think", Usage: "[off | low | medium | high | xhigh | default]", Summary: "set how hard the model thinks"},
		func(args string) (string, error) { return runThink(ctx, args) },
	)
}

func runThink(ctx *kernel.Context, args string) (string, error) {
	l, err := kernel.Get[llm.LLM](ctx, "llm")
	if err != nil {
		return "", fmt.Errorf("think: no llm service")
	}
	e, ok := l.(llm.Efforter)
	if !ok {
		return "", fmt.Errorf("think: %s cannot change its thinking level", modelName(l))
	}
	arg := strings.ToLower(strings.TrimSpace(args))
	if arg == "" {
		return "thinking: " + level(e.Effort()) + "\nlevels: " + strings.Join(llm.Efforts, ", ") + ", default", nil
	}
	if arg == "default" {
		arg = "" // hand the provider back its own default
	}
	if !llm.ValidEffort(arg) {
		return "", fmt.Errorf("think: unknown level %q (want %s or default)", arg, strings.Join(llm.Efforts, ", "))
	}
	if err := e.SetEffort(arg); err != nil {
		return "", err
	}
	return "thinking: " + level(arg) + " (from the next message)", nil
}

// level names the empty effort for a reader.
func level(e string) string {
	if e == "" {
		return "the provider's default"
	}
	return e
}

// modelName is the provider's model id when it reports one.
func modelName(l llm.LLM) string {
	if m, ok := l.(llm.Modeler); ok && m.Model() != "" {
		return m.Model()
	}
	return "this provider"
}
