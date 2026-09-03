package kernel

import (
	"cmp"
	"slices"
)

// Command is a CLI subcommand a plugin contributes: `bough <name>
// [args]` runs it without mounting the config tree. Run receives the
// config of the first row using the plugin (nil when the tree has no
// such row) so a command can read the same settings the mounted
// plugin would, and its own args.
type Command struct {
	Name    string
	Usage   string // argument hint after the name, "" for none
	Summary string // one line for --help
	Run     func(cfg map[string]any, args []string) error
}

// Commander is the optional plugin interface that contributes
// subcommands to the launcher.
type Commander interface {
	Commands() []Command
}

// PluginCommand is a Command tagged with the plugin that owns it.
type PluginCommand struct {
	Plugin string
	Command
}

// Commands returns every subcommand contributed by registered plugins,
// sorted by name. Factories are invoked (cheaply: constructors only)
// to ask for the Commander interface.
func Commands() []PluginCommand {
	regMu.Lock()
	names := make([]string, 0, len(registry))
	for n := range registry {
		names = append(names, n)
	}
	regMu.Unlock()
	slices.Sort(names)
	var out []PluginCommand
	for _, n := range names {
		regMu.Lock()
		f := registry[n]
		regMu.Unlock()
		if c, ok := f().(Commander); ok {
			for _, cmd := range c.Commands() {
				out = append(out, PluginCommand{Plugin: n, Command: cmd})
			}
		}
	}
	slices.SortFunc(out, func(a, b PluginCommand) int { return cmp.Compare(a.Name, b.Name) })
	return out
}

// FindCommand returns the plugin subcommand with the given name.
func FindCommand(name string) (PluginCommand, bool) {
	for _, c := range Commands() {
		if c.Name == name {
			return c, true
		}
	}
	return PluginCommand{}, false
}
