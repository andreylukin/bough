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
	"slices"
	"strings"

	"github.com/andreylukin/bough/internal/models"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"
)

// registerModel installs /model. Like /sessions, every service it
// needs is resolved lazily at Run time, so there is no mount-order
// dependency.
func registerModel(r *Registry, ctx *kernel.Context) error {
	return r.Register(
		CommandInfo{Name: "model", Usage: "[small] [list | provider [model] | model]", Summary: "pick or switch the model (or the small one)"},
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
// "llm", or failing that the first row running an llm-* plugin that is
// not the small one.
func llmRow(ctx *kernel.Context) (kernel.Row, error) {
	rows := ctx.Desired()
	for _, r := range rows {
		if r.ID == "llm" {
			return r, nil
		}
	}
	for _, r := range rows {
		if strings.HasPrefix(r.Plugin, "llm-") && !isSmall(r) {
			return r, nil
		}
	}
	return kernel.Row{}, fmt.Errorf("model: no llm row in the config tree")
}

// isSmall reports whether a row publishes the cheap model: named
// llm-small, or asking for that service key.
func isSmall(r kernel.Row) bool {
	if r.ID == llm.SmallKey {
		return true
	}
	s, _ := r.Config["service"].(string)
	return s == llm.SmallKey
}

// smallRow finds the llm-small row. Its absence is a normal state, not
// a broken config, so the error says how to add one.
func smallRow(ctx *kernel.Context) (kernel.Row, error) {
	for _, r := range ctx.Desired() {
		if isSmall(r) {
			return r, nil
		}
	}
	return kernel.Row{}, fmt.Errorf("model: no small-model row. Add one to your bough.yml:\n\n" +
		"- id: llm-small\n  plugin: llm-openrouter\n  config:\n    service: llm-small\n    model: <a cheap model>\n\n" +
		"It runs the jobs that are not the conversation: the session name, the memory, the status line, the composer's guess.")
}

// rowFor picks which row a /model invocation targets.
func rowFor(ctx *kernel.Context, target string) (kernel.Row, error) {
	if target == "small" {
		return smallRow(ctx)
	}
	return llmRow(ctx)
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

const modelUsage = "usage: /model [small] | /model list | /model [small] <provider> [model] | /model [small] <model>"

// pickerModels caps what one provider contributes to the picker: the
// catalogue has 359 OpenRouter models and a list that long is not a
// choice. Newest first, so the cap keeps what a person is likely
// reaching for; any id still works as an argument.
const pickerModels = 12

// suggestModels is a provider's models, newest first, from the model
// catalogue (models.dev, refreshed weekly, embedded snapshot offline).
// n <= 0 is the picker's cap.
func suggestModels(plugin string, n int) []string {
	if n <= 0 {
		n = pickerModels
	}
	return models.List(plugin, n)
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
		list := suggestModels(p, 0)
		if len(list) == 0 {
			choices = append(choices, p)
			continue
		}
		for _, m := range list {
			choices = append(choices, p+" "+m)
		}
	}
	if slices.Contains(choices, current) {
		return current, choices
	}
	return current, append([]string{current}, choices...)
}

// showModel is /model list: both rows, the providers, and a few model
// ids to try on the current provider.
func showModel(row kernel.Row, small string, provs []string) string {
	var b strings.Builder
	fmt.Fprintf(&b, "model: %s\nsmall: %s\nproviders: %s\n", describeRow(row), small, strings.Join(provs, ", "))
	if list := suggestModels(row.Plugin, 6); len(list) > 0 {
		fmt.Fprintf(&b, "try: %s\n", strings.Join(list, ", "))
	}
	b.WriteString(modelUsage)
	return b.String()
}

func pickerAction(target string, row kernel.Row, provs []string) UIAction {
	cur, choices := modelChoices(row, provs)
	return ModelPickerAction(target, cur, choices)
}

func runModel(ctx *kernel.Context, args string) (string, error) {
	provs := llmProviders()
	fields := strings.Fields(args)

	// "/model small …" targets the cheap model's row; everything else
	// is the agent's own.
	target := ""
	if len(fields) > 0 && fields[0] == "small" {
		target, fields = "small", fields[1:]
	}
	if len(fields) == 1 && fields[0] == "list" {
		row, err := llmRow(ctx)
		if err != nil {
			return "", err
		}
		small := "(none — /model small says how to add one)"
		if sr, err := smallRow(ctx); err == nil {
			small = describeRow(sr)
		}
		return showModel(row, small, provs), nil
	}
	row, err := rowFor(ctx, target)
	if err != nil {
		return "", err
	}
	if len(fields) == 0 {
		return "", pickerAction(target, row, provs)
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
	now, err := rowFor(ctx, target)
	if err != nil {
		return "", err
	}
	label := "model: "
	if target == "small" {
		label = "small model: "
	}
	return label + describeRow(now), nil
}
