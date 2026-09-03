package commands

// /model: pick or live-swap the llm row. With no args it opens the
// UI's model picker (ModelPickerAction) over every registered llm-*
// provider's curated models, the current row marked; "/model list"
// prints the same as text. "/model <provider> [model]" swaps the row's plugin (and
// optionally model); the shorthand "/model <model>" keeps the current
// plugin and changes only the model.
//
// The swap goes through the launcher's "config-set" service — the same
// LoadFile + overrides + Reconcile path the session picker uses to
// swap the history row — so the change survives a config hot reload.
// Without that service (bare kernel mounts, `bough rows`) the swap
// errors loudly; showing the current state still works.

import (
	"fmt"
	"strings"

	"github.com/andreylukin/bough/kernel"
)

// registerModel installs /model. Like /sessions, every service it
// needs is resolved lazily at Run time, so there is no mount-order
// dependency.
func registerModel(r *Registry, ctx *kernel.Context) error {
	return r.Register(
		CommandInfo{Name: "model", Usage: "[list | provider [model] | model]", Summary: "pick or switch the llm provider/model"},
		func(args string) (string, error) { return runModel(ctx, args) },
	)
}

// llmProviders returns the llm-* plugin names from the compile-time
// catalog, sorted (kernel.Plugins is sorted already).
func llmProviders() []string {
	var out []string
	for _, n := range kernel.Plugins() {
		if strings.HasPrefix(n, "llm-") {
			out = append(out, n)
		}
	}
	return out
}

// llmRow finds the provider row in the desired tree: the row with id
// "llm", or failing that the first row running an llm-* plugin.
func llmRow(ctx *kernel.Context) (kernel.Row, error) {
	rows := ctx.Desired()
	for _, r := range rows {
		if r.ID == "llm" {
			return r, nil
		}
	}
	for _, r := range rows {
		if strings.HasPrefix(r.Plugin, "llm-") {
			return r, nil
		}
	}
	return kernel.Row{}, fmt.Errorf("model: no llm row in the config tree")
}

// describeRow renders "plugin · model" (model omitted when the row
// config has none).
func describeRow(r kernel.Row) string {
	s := r.Plugin
	if m, ok := r.Config["model"].(string); ok && m != "" {
		s += " · " + m
	}
	return s
}

const modelUsage = "usage: /model | /model list | /model <provider> [model] | /model <model>"

// curatedModels is the short list /model suggests per provider (there
// is no history of used models yet; OpenRouter's catalog is too long
// to print).
var curatedModels = map[string][]string{
	"llm-openrouter": {
		"anthropic/claude-sonnet-4.5", "anthropic/claude-opus-4.1",
		"openai/gpt-5", "google/gemini-2.5-pro", "deepseek/deepseek-chat-v3.1",
	},
	"llm-anthropic": {"claude-sonnet-4-5", "claude-opus-4-1", "claude-haiku-4-5"},
	"llm-openai":    {"gpt-5", "gpt-5-mini", "gpt-5-nano"},
	"llm-cerebras":  {"gpt-oss-120b", "qwen-3.8-27b"},
}

// modelChoices is the picker's list: "provider model" for every
// curated model of every registered provider (a bare "provider" for
// one with no curated list), the current row's pair first when it is
// not curated. Pure.
func modelChoices(row kernel.Row, provs []string) (current string, choices []string) {
	current = row.Plugin
	if m, ok := row.Config["model"].(string); ok && m != "" {
		current += " " + m
	}
	for _, p := range provs {
		list := curatedModels[p]
		if len(list) == 0 {
			choices = append(choices, p)
			continue
		}
		for _, m := range list {
			choices = append(choices, p+" "+m)
		}
	}
	for _, c := range choices {
		if c == current {
			return current, choices
		}
	}
	return current, append([]string{current}, choices...)
}

// showModel is /model with no args: the current row, the providers,
// and a few model ids to try on the current provider.
func showModel(row kernel.Row, provs []string) string {
	var b strings.Builder
	fmt.Fprintf(&b, "model: %s\nproviders: %s\n", describeRow(row), strings.Join(provs, ", "))
	if list := curatedModels[row.Plugin]; len(list) > 0 {
		fmt.Fprintf(&b, "try: %s\n", strings.Join(list, ", "))
	}
	b.WriteString(modelUsage)
	return b.String()
}

func pickerAction(row kernel.Row, provs []string) UIAction {
	cur, choices := modelChoices(row, provs)
	return ModelPickerAction(cur, choices)
}

func runModel(ctx *kernel.Context, args string) (string, error) {
	row, err := llmRow(ctx)
	if err != nil {
		return "", err
	}
	provs := llmProviders()

	fields := strings.Fields(args)
	if len(fields) == 0 {
		return "", pickerAction(row, provs)
	}
	if len(fields) == 1 && fields[0] == "list" {
		return showModel(row, provs), nil
	}
	if len(fields) > 2 {
		return "", fmt.Errorf("model: too many arguments\n%s", modelUsage)
	}

	known := false
	for _, p := range provs {
		if p == fields[0] {
			known = true
		}
	}
	var sets []string
	switch {
	case known:
		sets = append(sets, row.ID+".plugin="+fields[0])
		if len(fields) == 2 {
			sets = append(sets, row.ID+".model="+fields[1])
		}
	case strings.HasPrefix(fields[0], "llm-"):
		return "", fmt.Errorf("model: unknown provider %q (have: %s)",
			fields[0], strings.Join(provs, ", "))
	case len(fields) == 2:
		return "", fmt.Errorf("model: %q is not a registered provider\n%s", fields[0], modelUsage)
	default: // shorthand: keep the plugin, change the model
		sets = append(sets, row.ID+".model="+fields[0])
	}

	set, err := kernel.Get[func(...string) error](ctx, "config-set")
	if err != nil {
		return "", fmt.Errorf("model: no config-set service (runtime swap needs the bough launcher)")
	}
	if err := set(sets...); err != nil {
		return "", err
	}

	// Reconcile tolerates a broken row (Failed/Pending) instead of
	// erroring — report that loudly rather than claiming success.
	for _, rs := range ctx.Rows() {
		if rs.ID != row.ID {
			continue
		}
		switch rs.State {
		case kernel.StateFailed:
			return "", fmt.Errorf("model: swap failed: %v (swap back with /model)", rs.Err)
		case kernel.StatePending:
			return "", fmt.Errorf("model: row %q pending: missing %s (swap back with /model)",
				rs.ID, strings.Join(rs.Missing, ", "))
		}
	}
	now, err := llmRow(ctx)
	if err != nil {
		return "", err
	}
	return "model: " + describeRow(now), nil
}
