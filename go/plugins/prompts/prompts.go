// Package prompts is the "prompts" plugin: Markdown prompt templates.
// Every ~/.bough/prompts/<name>.md and ./.bough/prompts/<name>.md (the
// project wins a name clash) is a "/name" command in the palette;
// dispatching "/name rest of line" expands the file's body —
// "$ARGUMENTS" is the rest of the line, "$1".."$9" its
// whitespace-split words — and submits the result as the user prompt.
// The file is read at dispatch, so edits apply live; the palette
// summary (first heading, else first non-empty line) is read at mount.
package prompts

import (
	"fmt"
	"maps"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strings"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
)

// Template is one prompt file.
type Template struct {
	Name    string // file name without ".md"
	Path    string
	Summary string // first heading, else first non-empty line; "" when empty
}

// Templates scans directories for *.md templates. A later directory
// shadows an earlier one on the same name.
type Templates struct {
	dirs []string
}

// New returns a Templates scanning the given directories in order.
func New(dirs ...string) *Templates { return &Templates{dirs: dirs} }

// Scan returns the templates across the dirs, sorted by name.
func (t *Templates) Scan() []Template {
	found := map[string]Template{}
	for _, dir := range t.dirs {
		entries, err := os.ReadDir(dir)
		if err != nil {
			continue // missing dir is fine
		}
		for _, e := range entries {
			name, ok := strings.CutSuffix(e.Name(), ".md")
			if !ok || e.IsDir() {
				continue
			}
			p := filepath.Join(dir, e.Name())
			found[name] = Template{Name: name, Path: p, Summary: summary(p)}
		}
	}
	out := make([]Template, 0, len(found))
	for _, name := range slices.Sorted(maps.Keys(found)) {
		out = append(out, found[name])
	}
	return out
}

// summary is the file's first heading (minus the "#" marks), else its
// first non-empty line.
func summary(path string) string {
	data, err := os.ReadFile(path)
	if err != nil {
		return ""
	}
	first := ""
	for line := range strings.SplitSeq(string(data), "\n") {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}
		if strings.HasPrefix(line, "#") {
			return strings.TrimSpace(strings.TrimLeft(line, "#"))
		}
		if first == "" {
			first = line
		}
	}
	return first
}

var argRe = regexp.MustCompile(`\$(ARGUMENTS|[1-9])`)

// Expand substitutes args into body: "$ARGUMENTS" is the whole
// argument string (trimmed), "$1".."$9" its whitespace-split words
// ("" past the last one).
func Expand(body, args string) string {
	args = strings.TrimSpace(args)
	words := strings.Fields(args)
	return argRe.ReplaceAllStringFunc(body, func(m string) string {
		if m == "$ARGUMENTS" {
			return args
		}
		if i := int(m[1] - '1'); i < len(words) {
			return words[i]
		}
		return ""
	})
}

// registerCommands adds a "/name" command per template and
// unregisters them on unmount.
func (t *Templates) registerCommands(ctx *kernel.Context, reg *commands.Registry) {
	for _, tp := range t.Scan() {
		info := commands.CommandInfo{Name: tp.Name, Usage: "[args]", Kind: "template",
			Summary: strings.TrimSpace("template: " + tp.Summary)}
		err := reg.Register(info, func(args string) (string, error) {
			body, err := os.ReadFile(tp.Path)
			if err != nil {
				return "", err
			}
			text := strings.TrimSpace(Expand(string(body), args))
			if text == "" {
				return "", fmt.Errorf("/%s: empty template (%s)", tp.Name, tp.Path)
			}
			return "", commands.SubmitAction(text)
		})
		if err != nil {
			fmt.Fprintf(os.Stderr, "prompts: %v\n", err)
			continue
		}
		ctx.Effect(func() { reg.Unregister(tp.Name) })
	}
}

type plugin struct{}

func init() {
	kernel.Register("prompts", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "prompts" }
func (plugin) Inject() []string { return []string{"commands"} }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	home, err := os.UserHomeDir()
	if err != nil {
		return fmt.Errorf("prompts: home dir: %w", err)
	}
	reg, err := kernel.Get[*commands.Registry](ctx, "commands")
	if err != nil {
		return err
	}
	t := New(filepath.Join(home, ".bough", "prompts"), filepath.Join(".bough", "prompts"))
	t.registerCommands(ctx, reg)
	ctx.Provide("prompts", t)
	return nil
}
