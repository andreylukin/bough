package llm

// llm-ollama, llm-script and llm-control are built on the engine's
// adapters, which need the unreal-agent harness, and that does not build
// for Windows at the pin. A config naming one fails its row with the
// reason.

import (
	"errors"

	"github.com/andreylukin/bough/kernel"
)

func init() {
	for _, n := range []string{"llm-ollama", "llm-script", "llm-control"} {
		kernel.Register(n, func() kernel.Plugin { return unavailable(n) })
	}
}

type unavailable string

func (u unavailable) Name() string     { return string(u) }
func (u unavailable) Inject() []string { return nil }

func (u unavailable) Apply(*kernel.Context, map[string]any) error {
	return errors.New(string(u) + ": not available on Windows: it runs on the unreal-agent harness, which is Unix-only at the pin (go/docs/unreal-engine.md §16)")
}
