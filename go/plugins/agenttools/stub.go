// Package agenttools is the agent-tools row: it provides the
// agenttools.Registry every tool row registers its native tools into.
// Harmless under the loop, which never reads it.
//
// stub.go is B0's complete row (go/docs/unreal-engine.md §6.2, §18.1
// step 3), written by W6 for the main.go and bough.yml wiring. W3 owns
// this package; delete this file if W3's branch brings its own row.
package agenttools

import (
	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/kernel"
)

func init() {
	kernel.Register("agent-tools", func() kernel.Plugin { return plugin{} })
}

type plugin struct{}

func (plugin) Name() string     { return "agent-tools" }
func (plugin) Inject() []string { return nil }

func (plugin) Apply(ctx *kernel.Context, _ map[string]any) error {
	ctx.Provide("agent-tools", agenttools.NewRegistry())
	return nil
}
