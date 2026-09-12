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
	"github.com/andreylukin/bough/plugins/ccplugins"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/offlist"
	"github.com/andreylukin/bough/plugins/prompts"
)

const maxBlocks = 3

// Skills implements loop.Skills. Pools are scanned in order; a later
// pool shadows an earlier one on the same skill name. The skills of
// the user's installed Claude Code plugins come first, so a pool skill
// of the same name still wins.
type Skills struct {
	pools []string
	home  string // user home; "" disables plugin skills and the off switch
	work  string // the path being worked on, for project-scoped plugins
}

// New returns a Skills scanning the given pool directories.
func New(pools ...string) *Skills { return &Skills{pools: pools} }

// Default returns the Skills the agent itself uses: the two pools
// under home, then the repo-local one, plus the plugin skills active
// for the current directory. `bough serve` lists the same set, so the
// web picker and the TUI never disagree about what exists.
func Default(home string) *Skills {
	work, err := os.Getwd()
	if err != nil {
		work = ""
	}
	return DefaultFor(home, work)
}

// DefaultFor is Default for a session working somewhere other than the
// process's own directory — the user runs bough from home, so a
// project-scoped plugin has to be judged against the path being worked
// on, not against the cwd.
func DefaultFor(home, work string) *Skills {
	s := New(
		filepath.Join(home, ".claude", "skills"),
		filepath.Join(home, ".bough", "skills"),
		filepath.Join(".claude", "skills"),
	)
	s.home, s.work = home, work
	return s
}

// boughHome is where off.yml lives; "" when this Skills has no home.
func (s *Skills) boughHome() string {
	if s.home == "" {
		return ""
	}
	return filepath.Join(s.home, ".bough")
}

// off reports whether a skill is switched off in off.yml.
func (s *Skills) off(name string) bool {
	if s.home == "" {
		return false
	}
	return offlist.Load(s.boughHome()).Off("skill", name)
}

// Inject returns "[skill: <name>]\n<SKILL.md contents>" blocks for
// every skill mentioned in input, capped at maxBlocks.
func (s *Skills) Inject(input string) []string {
	skills := s.scan()
	names := slices.Sorted(maps.Keys(skills))

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
		if (manual(skills[name].path) || commonWord(name)) && !strings.Contains(input, "/"+name) {
			continue
		}
		if s.off(name) {
			continue
		}
		matched++
		if len(blocks) >= maxBlocks {
			continue
		}
		body, err := os.ReadFile(skills[name].path)
		if err != nil {
			// Inject runs mid-turn with the TUI owning the tty: a raw
			// stderr write lands inside its frame and tears the screen.
			kernel.Logf("skills: read %s: %v\n", skills[name].path, err)
			continue
		}
		blocks = append(blocks, "[skill: "+name+"]\n"+string(body))
	}
	if matched > maxBlocks {
		kernel.Logf("skills: %d skills matched, injecting first %d\n", matched, maxBlocks)
	}
	return blocks
}

// Names returns every skill name across the pools, sorted, for the
// startup header. Skills switched off in off.yml are not there.
func (s *Skills) Names() []string {
	var out []string
	for _, name := range slices.Sorted(maps.Keys(s.scan())) {
		if !s.off(name) {
			out = append(out, name)
		}
	}
	return out
}

// found is one discovered skill.
type found struct {
	path   string // its SKILL.md
	source string // "plugin" or "pool"
}

// scan returns name -> skill across the pools (later pools shadow
// earlier ones). A symlinked skill directory counts.
func (s *Skills) scan() map[string]found {
	pools := s.pools
	if s.home != "" {
		pools = append(ccplugins.SkillDirs(s.home, s.work), pools...)
	}
	plugins := len(pools) - len(s.pools)

	out := map[string]found{}
	for i, pool := range pools {
		source := "pool"
		if i < plugins {
			source = "plugin"
		}
		entries, err := os.ReadDir(pool)
		if err != nil {
			continue // missing pool is fine
		}
		for _, e := range entries {
			p := filepath.Join(pool, e.Name(), "SKILL.md")
			if _, err := os.Stat(p); err == nil {
				out[e.Name()] = found{path: p, source: source}
			}
		}
	}
	return out
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
	for name, sk := range s.scan() {
		if s.off(name) {
			continue
		}
		info := commands.CommandInfo{Name: name, Usage: "[args]", Kind: "skill",
			Summary: "skill: " + summarize(description(sk.path))}
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
	s := Default(home)
	s.registerCommands(ctx)
	s.registerPluginCommands(ctx)
	ctx.Provide("skills", s)
	return nil
}

// SkillInfo is one skill as a picker shows it.
type SkillInfo struct {
	ID      string `json:"id"` // the off-switch id: the skill's name
	Name    string `json:"name"`
	Summary string `json:"summary"`
	Source  string `json:"source"` // "plugin" or "pool"
	Off     bool   `json:"off"`
	Manual  bool   `json:"manual"` // only ever runs as /name, never on a mention
}

// Catalog lists every skill across the pools, sorted by name. It is
// what a picker needs and nothing more: the SKILL.md body stays on
// disk until the loop injects it.
func (s *Skills) Catalog() []SkillInfo {
	skills := s.scan()
	out := make([]SkillInfo, 0, len(skills))
	for _, name := range slices.Sorted(maps.Keys(skills)) {
		sk := skills[name]
		out = append(out, SkillInfo{
			ID:      name,
			Name:    name,
			Summary: summarize(description(sk.path)),
			Source:  sk.source,
			Off:     s.off(name),
			Manual:  manual(sk.path) || commonWord(name),
		})
	}
	return out
}

// registerPluginCommands adds a "/name" command per Markdown file in
// the commands/ directory of every active plugin. They behave like
// prompt templates: dispatching one submits the file's body with
// "$ARGUMENTS" and "$1".. filled in. A name already taken — by a
// built-in, or by a skill of the same name — keeps its owner, and the
// clash is logged rather than passed off as loaded.
func (s *Skills) registerPluginCommands(ctx *kernel.Context) {
	if s.home == "" {
		return
	}
	reg, err := kernel.Get[*commands.Registry](ctx, "commands")
	if err != nil {
		return
	}
	for _, p := range ccplugins.Active(s.home, s.work) {
		dir := filepath.Join(p.InstallPath, "commands")
		for _, name := range p.Commands {
			path := filepath.Join(dir, name+".md")
			info := commands.CommandInfo{Name: name, Usage: "[args]", Kind: "template",
				Summary: strings.TrimSpace(p.Name + ": " + summarize(description(path)))}
			err := reg.Register(info, func(args string) (string, error) {
				body, err := os.ReadFile(path)
				if err != nil {
					return "", err
				}
				text := strings.TrimSpace(prompts.Expand(frontmatterless(string(body)), args))
				if text == "" {
					return "", fmt.Errorf("/%s: empty command (%s)", name, path)
				}
				return "", commands.SubmitAction(text)
			})
			if err != nil {
				kernel.Logf("skills: plugin %s command /%s: %v\n", p.ID, name, err)
				continue
			}
			ctx.Effect(func() { reg.Unregister(name) })
		}
	}
}

// frontmatterless drops a leading "---" YAML block: it is metadata for
// the loader, not part of the prompt the command submits.
func frontmatterless(body string) string {
	rest, ok := strings.CutPrefix(body, "---\n")
	if !ok {
		return body
	}
	if _, after, ok := strings.Cut(rest, "\n---"); ok {
		return strings.TrimPrefix(after, "\n")
	}
	return body
}
