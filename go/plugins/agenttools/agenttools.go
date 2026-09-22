// Package agenttools is the "agent-tools" row: it provides the
// registry every tool row registers its native tools into, next to its
// codemode bindings. It holds no tools of its own and reads nothing, so
// under the loop it is inert; the engine row snapshots it per
// coordinator (go/docs/unreal-engine.md §6.2).
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

// Apply provides one registry for the life of the row. A remount would
// hand out a new, empty registry and remount every tool row that read
// it, which is how their registrations move over with it.
func (plugin) Apply(ctx *kernel.Context, _ map[string]any) error {
	ctx.Provide("agent-tools", agenttools.NewRegistry())
	return nil
}
