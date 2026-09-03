package kernel

import (
	"fmt"
	"maps"
	"slices"
	"sync"
)

// Plugin is the one interface everything implements.
type Plugin interface {
	Name() string
	Inject() []string // service keys this plugin needs before Apply
	Apply(ctx *Context, cfg map[string]any) error
}

var (
	regMu    sync.Mutex
	registry = map[string]func() Plugin{}
)

// Register adds a plugin factory to the compile-time catalog.
// Called from plugin package init(). Panics on duplicate name.
func Register(name string, factory func() Plugin) {
	regMu.Lock()
	defer regMu.Unlock()
	if _, dup := registry[name]; dup {
		panic(fmt.Sprintf("kernel: duplicate plugin %q", name))
	}
	registry[name] = factory
}

// Plugins returns every registered plugin name, sorted — the
// compile-time catalog (e.g. so /model can list the llm-* providers).
func Plugins() []string {
	regMu.Lock()
	defer regMu.Unlock()
	names := slices.Sorted(maps.Keys(registry))
	return names
}

func lookup(name string) (func() Plugin, bool) {
	regMu.Lock()
	defer regMu.Unlock()
	f, ok := registry[name]
	return f, ok
}
