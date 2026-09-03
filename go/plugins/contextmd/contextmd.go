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

// Preamble concatenates every existing path as a labeled section.
func (s *SystemContext) Preamble() string {
	var out strings.Builder
	for _, p := range s.paths {
		body, err := os.ReadFile(p)
		if err != nil {
			continue // missing file is fine
		}
		out.WriteString("# Context: " + p + "\n" + string(body) + "\n")
	}
	return out.String()
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
