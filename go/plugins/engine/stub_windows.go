// Package engine is the engine-unreal row. On Windows it is a stub: the
// unreal-agent harness at the pin runs processes through Unix-only
// syscalls (harness/primitives/process.go has no build tag), so the
// engine is not built there, and a config that names it fails its row
// with the reason instead of failing the whole binary's build.
package engine

import (
	"errors"

	"github.com/andreylukin/bough/kernel"
)

const name = "engine-unreal"

func init() {
	kernel.Register(name, func() kernel.Plugin { return plugin{} })
}

type plugin struct{}

func (plugin) Name() string     { return name }
func (plugin) Inject() []string { return nil }

func (plugin) Apply(*kernel.Context, map[string]any) error {
	return errors.New(name + ": not available on Windows: the unreal-agent harness at the pin is Unix-only (go/docs/unreal-engine.md §16); use plugin: loop")
}
