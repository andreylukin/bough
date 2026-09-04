// Package skills is the "skills" plugin: mention-triggered SKILL.md
// injection. Pools (~/.claude/skills, ~/.bough/skills and
// ./.claude/skills) are rescanned fresh on every Inject call; a skill
// directory whose name appears as a case-insensitive whole word in the
// human input gets its SKILL.md injected into that turn. Each skill is
// also a "/name" command in the palette: "/exa foo" submits that line
// as input, so the mention rule injects it.
package skills

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
	found := s.scan()
	names := slices.Sorted(maps.Keys(found))

	var blocks []string
	matched := 0
	for _, name := range names {
		re := regexp.MustCompile(`(?i)\b` + regexp.QuoteMeta(name) + `\b`)
		if !re.MatchString(input) {
			continue
		}
		// A skill named after an ordinary word fires on ordinary
		// English: "check three things in parallel" pulled in 3k
		// characters of the web-search skill. Such a skill is opt-in
		// per mention — "/parallel" runs it, prose does not — either
		// because its SKILL.md says `manual: true` or because its name
		// is a word people write without meaning it.
		if (manual(found[name]) || commonWord(name)) && !strings.Contains(input, "/"+name) {
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

// Names returns every skill name across the pools, sorted, for the
// startup header.
func (s *Skills) Names() []string { return slices.Sorted(maps.Keys(s.scan())) }

// scan returns name -> SKILL.md path across the pools (later pools
// shadow earlier ones). A symlinked skill directory counts.
func (s *Skills) scan() map[string]string {
	found := map[string]string{}
	for _, pool := range s.pools {
		entries, err := os.ReadDir(pool)
		if err != nil {
			continue // missing pool is fine
		}
		for _, e := range entries {
			p := filepath.Join(pool, e.Name(), "SKILL.md")
			if _, err := os.Stat(p); err == nil {
				found[e.Name()] = p
			}
		}
	}
	return found
}

// commonWords are skill names that are also ordinary English: a
// mention of one is usually the word, not the skill. The list is
// short on purpose — `manual: true` in the SKILL.md is the general
// answer, this is the default for the handful that bite immediately.
var commonWords = map[string]bool{
	"parallel": true, "wiki": true, "commit": true, "host": true,
	"prose": true, "notes": true, "search": true, "review": true,
	"test": true, "plan": true, "build": true, "deploy": true,
}

func commonWord(name string) bool { return commonWords[strings.ToLower(name)] }

// manual reports whether a SKILL.md opts out of being injected on a
// mention (`manual: true` in its frontmatter); it is still available
// as /name.
func manual(path string) bool {
	data, err := os.ReadFile(path)
	if err != nil {
		return false
	}
	for line := range strings.SplitSeq(string(data), "\n") {
		if v, ok := strings.CutPrefix(line, "manual:"); ok {
			return strings.TrimSpace(strings.Trim(strings.TrimSpace(v), `"'`)) == "true"
		}
		if strings.HasPrefix(line, "---") && strings.TrimSpace(line) == "---" && len(line) > 0 {
			continue
		}
	}
	return false
}

// description pulls the frontmatter "description:" line of a SKILL.md
// for the palette summary; "" when absent.
func description(path string) string {
	data, err := os.ReadFile(path)
	if err != nil {
		return ""
	}
	for line := range strings.SplitSeq(string(data), "\n") {
		if v, ok := strings.CutPrefix(line, "description:"); ok {
			return strings.Trim(strings.TrimSpace(v), `"'`)
		}
	}
	return ""
}

// triggerPrefixes are the model-facing openers a SKILL.md description
// tends to start with; the palette summary drops them.
var triggerPrefixes = []string{
	"use this skill when ", "use this skill to ", "use this when ", "use when ",
	"trigger when ", "triggers when ", "used when ", "use to ", "use for ",
}

// summarize turns a SKILL.md description (written for the model:
// "Use when the user needs to…") into a palette one-liner: the first
// sentence, minus a leading trigger phrase, capitalized.
func summarize(desc string) string {
	s := strings.Join(strings.Fields(desc), " ")
	if i := strings.Index(s, ". "); i >= 0 {
		s = s[:i]
	}
	s = strings.TrimSuffix(s, ".")
	low := strings.ToLower(s)
	for _, p := range triggerPrefixes {
		if strings.HasPrefix(low, p) {
			s = s[len(p):]
			break
		}
	}
	if r := []rune(s); len(r) > 0 {
		s = strings.ToUpper(string(r[0])) + string(r[1:])
	}
	return s
}

// registerCommands adds a "/name" command per skill to the commands
// registry (when one is mounted) and unregisters them on unmount.
func (s *Skills) registerCommands(ctx *kernel.Context) {
	reg, err := kernel.Get[*commands.Registry](ctx, "commands")
	if err != nil {
		return
	}
	for name, path := range s.scan() {
		info := commands.CommandInfo{Name: name, Usage: "[args]", Kind: "skill",
			Summary: "skill: " + summarize(description(path))}
		err := reg.Register(info, func(args string) (string, error) {
			return "", commands.SubmitAction(strings.TrimSpace("/" + name + " " + args))
		})
		if err != nil {
			fmt.Fprintf(os.Stderr, "skills: %v\n", err)
			continue
		}
		ctx.Effect(func() { reg.Unregister(name) })
	}
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
	s := New(
		filepath.Join(home, ".claude", "skills"),
		filepath.Join(home, ".bough", "skills"),
		filepath.Join(".claude", "skills"),
	)
	s.registerCommands(ctx)
	ctx.Provide("skills", s)
	return nil
}
