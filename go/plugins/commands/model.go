package commands

// /model: show or live-swap the llm row. With no args it prints the
// current provider row (plugin + model from the row config), the
// registered llm-* providers from the kernel's plugin catalog, and the
// usage line. "/model <provider> [model]" swaps the row's plugin (and
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
		CommandInfo{Name: "model", Usage: "[provider] [model]", Summary: "show or switch the llm provider/model"},
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

const modelUsage = "usage: /model <provider> [model] | /model <model>"

func runModel(ctx *kernel.Context, args string) (string, error) {
	row, err := llmRow(ctx)
	if err != nil {
		return "", err
	}
	provs := llmProviders()

	fields := strings.Fields(args)
	if len(fields) == 0 {
		return fmt.Sprintf("model: %s\nproviders: %s\n%s",
			describeRow(row), strings.Join(provs, ", "), modelUsage), nil
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
