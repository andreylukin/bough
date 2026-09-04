// Package example is a complete, working plugin, kept small enough to
// read in one sitting and real enough to copy. docs/PLUGINS.md walks
// through this file line by line.
//
// It is compiled into the binary like every other plugin, and mounts
// only if you put its row in your bough.yml:
//
//   - id: wordcount
//     plugin: example-wordcount
//     config:
//     min_length: 4
//
// Then `tools.wordcount(text)` is callable from a code block, and
// `/wordcount <text>` from the palette.
//
// It shows the four things a plugin usually wants: reading and
// validating config, providing a service, using OPTIONAL services when
// they are mounted, and cleaning up after itself.
package example

import (
	"fmt"
	"sort"
	"strings"
	"unicode"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
)

// Register the plugin under the name a bough.yml row uses. init() runs
// because cmd/bough imports every plugin package for its side effects;
// a plugin in your own fork needs that import added there too.
func init() {
	kernel.Register("example-wordcount", func() kernel.Plugin { return plugin{} })
}

type plugin struct{}

// Name is what shows in `bough rows` and in mount errors.
func (plugin) Name() string { return "example-wordcount" }

// Inject lists the service keys that must exist BEFORE Apply runs. The
// kernel holds the row as `pending` until they do, so anything named
// here can be fetched in Apply without checking for absence.
//
// List only what the plugin cannot work without. "codemode" and
// "commands" are wanted here but not required — see Apply.
func (plugin) Inject() []string { return nil }

// Apply mounts the row. It runs again whenever a service it read
// changes, so it must be safe to call more than once.
func (p plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	// Config arrives as whatever YAML produced. Validate it here and
	// return an error: the kernel marks the row `failed`, names it, and
	// leaves the rest of the tree running.
	minLength := 1
	if v, ok := cfg["min_length"]; ok {
		n, ok := v.(int)
		if !ok || n < 1 {
			return fmt.Errorf("example-wordcount: min_length must be a positive integer, got %v", v)
		}
		minLength = n
	}

	counter := &counter{minLength: minLength}

	// A service other rows can consume. Keys are the whole wiring
	// story: nothing imports another plugin's package to reach it.
	ctx.Provide("wordcount", counter)

	// An OPTIONAL dependency: mount without it, pick it up if it
	// appears. The kernel tracks every service read during Apply, so
	// adding a codemode row later remounts this one automatically.
	if cm, err := kernel.Get[toolRegistry](ctx, "codemode"); err == nil {
		cm.RegisterTool("wordcount", counter.Count)
		// Effects run LIFO on unmount — on a hot reload, on a remount,
		// and at exit. A plugin that registers something is
		// responsible for removing it.
		ctx.Effect(func() { cm.RegisterTool("wordcount", nil) })
	}

	if reg, err := kernel.Get[*commands.Registry](ctx, "commands"); err == nil {
		info := commands.CommandInfo{
			Name:    "wordcount",
			Usage:   "<text>",
			Summary: "count the words in some text",
		}
		if err := reg.Register(info, func(args string) (string, error) {
			return render(counter.Count(args)), nil
		}); err != nil {
			return fmt.Errorf("example-wordcount: %w", err)
		}
		ctx.Effect(func() { reg.Unregister("wordcount") })
	}

	return nil
}

// toolRegistry is the slice of the "codemode" service this plugin
// uses. Declaring the method set you need, rather than importing the
// concrete type, is the convention here: it keeps plugins from
// depending on each other's packages.
type toolRegistry interface{ RegisterTool(name string, fn any) }

// counter is the service value. Methods on it are what other rows —
// and, through RegisterTool, the model — actually call.
type counter struct{ minLength int }

// Count returns each word and how often it appears. Words shorter than
// min_length are skipped. A tool function's return value is what the
// model sees, so keep it JSON-shaped: maps, slices, strings, numbers.
func (c *counter) Count(text string) map[string]int {
	out := map[string]int{}
	for _, field := range strings.FieldsFunc(text, func(r rune) bool {
		return !unicode.IsLetter(r) && !unicode.IsNumber(r) && r != '\''
	}) {
		w := strings.ToLower(field)
		if len([]rune(w)) >= c.minLength {
			out[w]++
		}
	}
	return out
}

// render turns a count into the text a slash command prints: most
// frequent first, ties broken alphabetically so the output is stable.
func render(counts map[string]int) string {
	if len(counts) == 0 {
		return "no words"
	}
	words := make([]string, 0, len(counts))
	for w := range counts {
		words = append(words, w)
	}
	sort.Slice(words, func(i, j int) bool {
		if counts[words[i]] != counts[words[j]] {
			return counts[words[i]] > counts[words[j]]
		}
		return words[i] < words[j]
	})
	var b strings.Builder
	for _, w := range words {
		fmt.Fprintf(&b, "%4d  %s\n", counts[w], w)
	}
	return strings.TrimRight(b.String(), "\n")
}
