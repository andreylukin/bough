// Package skills is the "skills" plugin: mention-triggered SKILL.md
// injection. Pools (~/.claude/skills and ./.claude/skills) are
// rescanned fresh on every Inject call; a skill directory whose name
// appears as a case-insensitive whole word in the human input gets
// its SKILL.md injected into that turn.
package skills

import (
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"

	"github.com/andreylukin/bough/kernel"
)

const maxBlocks = 3

// Skills implements loop.Skills. Pools are scanned in order; a later
// pool shadows an earlier one on the same skill name.
type Skills struct {
	pools []string
}

// New returns a Skills scanning the given pool directories.
func New(pools ...string) *Skills { return &Skills{pools: pools} }

// Inject returns "[skill: <name>]\n<SKILL.md contents>" blocks for
// every skill mentioned in input, capped at maxBlocks.
func (s *Skills) Inject(input string) []string {
	found := map[string]string{} // name -> SKILL.md path
	for _, pool := range s.pools {
		entries, err := os.ReadDir(pool)
		if err != nil {
			continue // missing pool is fine
		}
		for _, e := range entries {
			if !e.IsDir() {
				continue
			}
			p := filepath.Join(pool, e.Name(), "SKILL.md")
			if _, err := os.Stat(p); err == nil {
				found[e.Name()] = p
			}
		}
	}

	names := make([]string, 0, len(found))
	for n := range found {
		names = append(names, n)
	}
	sort.Strings(names)

	var blocks []string
	matched := 0
	for _, name := range names {
		re := regexp.MustCompile(`(?i)\b` + regexp.QuoteMeta(name) + `\b`)
		if !re.MatchString(input) {
			continue
		}
		matched++
		if len(blocks) >= maxBlocks {
			continue
		}
		body, err := os.ReadFile(found[name])
		if err != nil {
			fmt.Fprintf(os.Stderr, "skills: read %s: %v\n", found[name], err)
			continue
		}
		blocks = append(blocks, "[skill: "+name+"]\n"+string(body))
	}
	if matched > maxBlocks {
		fmt.Fprintf(os.Stderr, "skills: %d skills matched, injecting first %d\n", matched, maxBlocks)
	}
	return blocks
}

type plugin struct{}

func init() {
	kernel.Register("skills", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "skills" }
func (plugin) Inject() []string { return nil }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	home, err := os.UserHomeDir()
	if err != nil {
		return fmt.Errorf("skills: home dir: %w", err)
	}
	ctx.Provide("skills", New(
		filepath.Join(home, ".claude", "skills"),
		filepath.Join(".claude", "skills"),
	))
	return nil
}
