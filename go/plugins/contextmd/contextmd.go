// Package contextmd is the "context-md" plugin: at the start of every
// turn the loop prepends Preamble() — whichever exist of ./AGENTS.md,
// ./CLAUDE.md, ~/.claude/CLAUDE.md, ~/.bough/BOUGH.md — to the
// system prompt, labeled per file, so a file created or edited
// mid-session is seen on the next turn.
package contextmd

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/andreylukin/bough/kernel"
)

// SystemContext implements loop.SystemContext. Files are read fresh
// on every Preamble call; missing files are skipped.
type SystemContext struct {
	paths []string
}

// New returns a SystemContext reading the given paths in order.
func New(paths ...string) *SystemContext { return &SystemContext{paths: paths} }

// Part is one file's contribution after de-duplication: Text is what
// actually goes into the prompt, Dropped counts the sections already
// said by an earlier file, and Same names that file.
type Part struct {
	Path    string
	Text    string
	Dropped int
	Same    string
}

// Parts reads every existing path and de-duplicates them by section.
// CLAUDE.md is very often a copy of AGENTS.md (or a symlink, or the
// same house rules pasted twice); sending both spends the context
// twice and tells the model the same rule with two voices. A section
// whose body was already said by an earlier file is dropped, and a
// file left with nothing at all disappears.
func (s *SystemContext) Parts() []Part {
	seen := map[string]string{} // section key -> the file that said it
	var parts []Part
	for _, p := range s.paths {
		body, err := os.ReadFile(p)
		if err != nil {
			continue // missing file is fine
		}
		var kept []string
		dropped, same := 0, ""
		for _, sec := range sections(string(body)) {
			k := key(sec)
			if k == "" {
				continue // whitespace only
			}
			if first, dup := seen[k]; dup {
				dropped++
				if same == "" {
					same = first
				}
				continue
			}
			seen[k] = p
			kept = append(kept, sec)
		}
		if len(kept) == 0 {
			continue
		}
		parts = append(parts, Part{
			Path:    p,
			Text:    "# Context: " + p + "\n" + strings.Join(kept, "\n") + "\n",
			Dropped: dropped,
			Same:    same,
		})
	}
	return parts
}

// sections splits a markdown file at its headings: the text before the
// first heading is its own section, then one per heading. Splitting at
// every level means a shared "## Testing" block is caught even when
// the files disagree about the rest.
func sections(body string) []string {
	var out []string
	var cur strings.Builder
	for _, line := range strings.SplitAfter(body, "\n") {
		if strings.HasPrefix(line, "#") && cur.Len() > 0 {
			out = append(out, cur.String())
			cur.Reset()
		}
		cur.WriteString(line)
	}
	if cur.Len() > 0 {
		out = append(out, cur.String())
	}
	return out
}

// key normalises a section for comparison: blank lines and trailing
// whitespace do not make two copies of the same rule different.
func key(sec string) string {
	var b strings.Builder
	for _, line := range strings.Split(sec, "\n") {
		if line = strings.TrimRight(line, " \t"); line != "" {
			b.WriteString(line + "\n")
		}
	}
	return b.String()
}

// Preamble is every part's text, in path order.
func (s *SystemContext) Preamble() string {
	var out strings.Builder
	for _, p := range s.Parts() {
		out.WriteString(p.Text)
	}
	return out.String()
}

// Loaded returns the paths that exist right now — what Preamble
// includes — for the startup header.
func (s *SystemContext) Loaded() []string {
	var out []string
	for _, p := range s.paths {
		if _, err := os.Stat(p); err == nil {
			out = append(out, p)
		}
	}
	return out
}

type plugin struct{}

func init() {
	kernel.Register("context-md", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "context-md" }
func (plugin) Inject() []string { return nil }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	home, err := os.UserHomeDir()
	if err != nil {
		return fmt.Errorf("context-md: home dir: %w", err)
	}
	ctx.Provide("context-md", New(
		"AGENTS.md",
		"CLAUDE.md",
		filepath.Join(home, ".claude", "CLAUDE.md"),
		filepath.Join(home, ".bough", "BOUGH.md"),
	))
	return nil
}
