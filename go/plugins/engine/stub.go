// Package engine is the engine-unreal row plugin: unreal-agent as the
// loop row's engine (go/docs/unreal-engine.md §6.1).
//
// stub.go is B0's placeholder, written by W6 so main.go can import the
// package and `--set loop.plugin=engine-unreal` fails with a named
// error instead of "unknown plugin". W2 owns this package; delete this
// file when W2's row lands.
package engine

import (
	"errors"

	"github.com/andreylukin/bough/kernel"
)

// errNotBuilt is the stub's mount error. The e2e engine suite skips on
// its text, so the suite runs the moment W2's row replaces this file.
var errNotBuilt = errors.New("engine-unreal: not built yet (see go/docs/unreal-engine.md)")

func init() {
	kernel.Register("engine-unreal", func() kernel.Plugin { return plugin{} })
}

type plugin struct{}

func (plugin) Name() string     { return "engine-unreal" }
func (plugin) Inject() []string { return []string{"history", "agent-tools"} }

func (plugin) Apply(*kernel.Context, map[string]any) error { return errNotBuilt }
