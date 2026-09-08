// Package rules is the "rules" plugin: the rule files Claude Code and
// Codex users already keep, honoured by bough — from the home
// directory (~/.claude/rules, ~/.codex/rules) and from the project
// (./.claude/rules, ./.codex/rules), the way hooks come from both.
//
//   - Claude rules (markdown): unscoped ones join the AGENTS.md /
//     CLAUDE.md preamble every turn; ones with `paths:` frontmatter are
//     shown once, in the result, when a code block touches a matching
//     file — the way Claude Code loads them on read.
//   - Codex rules (prefix_rule in .rules files): forbidden refuses the
//     shell command with the rule's justification; prompt asks the
//     user (through the ask row, when mounted); allow is the default.
//
// Files are read fresh each time, so a rule added mid-session counts.
package rules

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
)

// The seams this row uses, each optional.
type sourcer interface {
	AddSource(fn func() []string) func()
}
type hooker interface {
	Add(event, name string, fn func(payload map[string]any) map[string]any) func()
}
type policer interface {
	SetPolicy(fn func(cmd string) error)
}
type asker interface {
	Ask(question string, options ...string) (string, error)
}
type sections interface{ Set(name, text string) }

// Service holds the directories and what has been shown.
type Service struct {
	home, project string
	ask           asker
	findAsk       func() asker // lazy: the ask row mounts after this one

	mu       sync.Mutex
	shown    map[string]bool // scoped rule paths already put in front of the model
	reported map[string]bool // parse errors already printed
}

// New returns a Service for a project directory.
func New(home, project string) *Service {
	return &Service{home: home, project: project, shown: map[string]bool{}, reported: map[string]bool{}}
}

func (s *Service) claude() []Rule { return loadClaude(dedupe(claudeDirs(s.home, s.project))) }

// dedupe drops a repeated dir (the project IS the home directory).
func dedupe(dirs []string) []string {
	seen := map[string]bool{}
	var out []string
	for _, d := range dirs {
		if abs, err := filepath.Abs(d); err == nil {
			d = abs
		}
		if !seen[d] {
			seen[d] = true
			out = append(out, d)
		}
	}
	return out
}

// Unscoped is the preamble source: every rule file without paths.
func (s *Service) Unscoped() []string {
	var out []string
	for _, r := range s.claude() {
		if !r.Scoped() {
			out = append(out, r.Path)
		}
	}
	return out
}

// Touched is the post-result hook: the scoped rules that match a file
// the code mentioned and have not been shown yet, appended to the
// result so the model reads them with the output.
func (s *Service) Touched(code string) string {
	paths := pathsIn(code)
	if len(paths) == 0 {
		return ""
	}
	root, _ := filepath.Abs(s.project)
	var b strings.Builder
	for _, r := range s.claude() {
		if !r.Scoped() {
			continue
		}
		s.mu.Lock()
		done := s.shown[r.Path]
		s.mu.Unlock()
		if done {
			continue
		}
		for _, p := range paths {
			rel := p
			if filepath.IsAbs(p) {
				if r, err := filepath.Rel(root, p); err == nil && !strings.HasPrefix(r, "..") {
					rel = r
				}
			}
			if r.Matches(rel) {
				s.mu.Lock()
				s.shown[r.Path] = true
				s.mu.Unlock()
				fmt.Fprintf(&b, "\n\n[rule: %s — applies to %s]\n%s", s.short(r.Path), strings.Join(r.Globs, ", "), strings.TrimSpace(r.Body))
				break
			}
		}
	}
	return b.String()
}

// short names a rule file the way a person would: ~/... or ./...
func (s *Service) short(p string) string {
	if root, err := filepath.Abs(s.project); err == nil {
		if rel, err := filepath.Rel(root, p); err == nil && !strings.HasPrefix(rel, "..") {
			return rel
		}
	}
	if s.home != "" && strings.HasPrefix(p, s.home) {
		return "~" + strings.TrimPrefix(p, s.home)
	}
	return p
}

// Policy is the bash gate: nil when the command may run, else the
// refusal the model reads.
func (s *Service) Policy(cmd string) error {
	rules, errs := loadCodex(dedupe(codexDirs(s.home, s.project)))
	for _, err := range errs {
		s.mu.Lock()
		seen := s.reported[err.Error()]
		s.reported[err.Error()] = true
		s.mu.Unlock()
		if !seen {
			fmt.Fprintln(os.Stderr, "rules:", err)
		}
	}
	decision, why, ok := Check(rules, cmd)
	if !ok {
		return nil
	}
	pattern := make([]string, len(why.Pattern))
	for i, alts := range why.Pattern {
		pattern[i] = strings.Join(alts, "|")
	}
	rule := fmt.Sprintf("%s (%s)", strings.Join(pattern, " "), s.short(why.File))
	switch decision {
	case "forbidden":
		if why.Justification != "" {
			return fmt.Errorf("command refused by rule %s: %s", rule, why.Justification)
		}
		return fmt.Errorf("command refused by rule %s", rule)
	case "prompt":
		if s.ask == nil && s.findAsk != nil {
			s.ask = s.findAsk()
		}
		if s.ask == nil {
			return nil
		}
		q := fmt.Sprintf("Rule %s asks before running:\n%s", rule, cmd)
		if why.Justification != "" {
			q += "\n" + why.Justification
		}
		answer, err := s.ask.Ask(q, "run", "refuse")
		if err != nil {
			return fmt.Errorf("command not run: %w", err)
		}
		if a := strings.ToLower(strings.TrimSpace(answer)); a != "run" && a != "yes" && a != "y" {
			return fmt.Errorf("command refused by you (rule %s)", rule)
		}
	}
	return nil
}

// Summary is the /rules command and the prompt section: what is in
// force, from where.
func (s *Service) Summary() string {
	var b strings.Builder
	cr := s.claude()
	px, errs := loadCodex(dedupe(codexDirs(s.home, s.project)))
	if len(cr) == 0 && len(px) == 0 && len(errs) == 0 {
		return "no rules: none under " + strings.Join(append(dedupe(claudeDirs(s.home, s.project)), dedupe(codexDirs(s.home, s.project))...), ", ")
	}
	for _, r := range cr {
		if r.Scoped() {
			fmt.Fprintf(&b, "%s — when working with %s\n", s.short(r.Path), strings.Join(r.Globs, ", "))
		} else {
			fmt.Fprintf(&b, "%s — always\n", s.short(r.Path))
		}
	}
	for _, r := range px {
		pattern := make([]string, len(r.Pattern))
		for i, alts := range r.Pattern {
			pattern[i] = strings.Join(alts, "|")
		}
		fmt.Fprintf(&b, "%s: %s → %s", s.short(r.File), strings.Join(pattern, " "), r.Decision)
		if r.Justification != "" {
			fmt.Fprintf(&b, " (%s)", r.Justification)
		}
		b.WriteString("\n")
	}
	for _, err := range errs {
		fmt.Fprintf(&b, "error: %v\n", err)
	}
	return strings.TrimRight(b.String(), "\n")
}

// scopedSection tells the model which scoped rules exist, so it knows
// more guidance arrives when it touches those files.
func (s *Service) scopedSection() string {
	var lines []string
	for _, r := range s.claude() {
		if r.Scoped() {
			lines = append(lines, fmt.Sprintf("- %s: %s", strings.Join(r.Globs, ", "), s.short(r.Path)))
		}
	}
	if len(lines) == 0 {
		return ""
	}
	return "Path-scoped rules — shown with the result the first time a code block touches a matching file:\n" + strings.Join(lines, "\n")
}

type plugin struct{}

func init() {
	kernel.Register("rules", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "rules" }
func (plugin) Inject() []string { return nil }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	for k := range cfg {
		return fmt.Errorf("rules: unknown config key %q", k)
	}
	home, _ := os.UserHomeDir()
	s := New(home, ".")
	s.findAsk = func() asker {
		a, err := kernel.Get[asker](ctx, "ask-answers")
		if err != nil {
			return nil
		}
		return a
	}
	if src, err := kernel.Get[sourcer](ctx, "context-md"); err == nil {
		ctx.Effect(src.AddSource(s.Unscoped))
	}
	if h, err := kernel.Get[hooker](ctx, "hooks"); err == nil {
		ctx.Effect(h.Add("post-result", "rules", func(payload map[string]any) map[string]any {
			code, _ := payload["code"].(string)
			extra := s.Touched(code)
			if extra == "" {
				return nil
			}
			result, _ := payload["result"].(string)
			return map[string]any{"result": result + extra}
		}))
	}
	if p, err := kernel.Get[policer](ctx, "turn-stats"); err == nil {
		p.SetPolicy(s.Policy)
		ctx.Effect(func() { p.SetPolicy(nil) })
	}
	if sec, err := kernel.Get[sections](ctx, "prompt-sections"); err == nil {
		if text := s.scopedSection(); text != "" {
			sec.Set("rules", text)
			ctx.Effect(func() { sec.Set("rules", "") })
		}
	}
	if reg, err := kernel.Get[*commands.Registry](ctx, "commands"); err == nil {
		info := commands.CommandInfo{Name: "rules", Summary: "the Claude and Codex rules in force: ~/.claude/rules, ./.claude/rules, ~/.codex/rules, ./.codex/rules"}
		if err := reg.Register(info, func(string) (string, error) { return s.Summary(), nil }); err != nil {
			return err
		}
		ctx.Effect(func() { reg.Unregister("rules") })
	}
	ctx.Provide("rules", s)
	return nil
}
