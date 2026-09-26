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
	"context"
	"fmt"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/offlist"
)

// policyReloadGateEnv names a directory that, when set, holds this
// row's dispose exactly in the gap real callers can land in: the
// command policy set to nil, before the row that remounts it (a
// reload; tests/model/specs/rules_reload_mid_approval.fizz) has
// applied its own. It is this flow's own hook (never BOUGH_TEST_STEP_GATE:
// that one also gates headless's per-line stdin pump and ask's answer
// relay, so turning it on here would freeze every turn, not just a
// reload) and does nothing unless BOUGH_TEST_RULES_RELOAD_GATE names a
// directory.
const policyReloadGateEnv = "BOUGH_TEST_RULES_RELOAD_GATE"

// policyReloadHold announces the dispose as <dir>/held and blocks until
// the test drops <dir>/go — but only while the test has armed it
// (<dir>/armed present). A row can dispose for reasons that have
// nothing to do with the reload this hook exists to test (its own
// dependencies landing one at a time during startup, for instance);
// holding one of those hangs the whole reconcile, since it runs on the
// kernel's one reconcile goroutine, wedging every reload after it.
func policyReloadHold() {
	dir := os.Getenv(policyReloadGateEnv)
	if dir == "" {
		return
	}
	if _, err := os.Stat(filepath.Join(dir, "armed")); err != nil {
		return
	}
	os.WriteFile(filepath.Join(dir, "held"), nil, 0o644)
	for {
		if _, err := os.Stat(filepath.Join(dir, "go")); err == nil {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
}

// The seams this row uses, each optional.
type sourcer interface {
	AddSource(fn func() []string) func()
}
type hooker interface {
	Add(event, name string, fn func(payload map[string]any) map[string]any) func()
}
type describedHooker interface {
	AddWithDescription(event, name, description string, fn func(payload map[string]any) map[string]any) func()
}

// Older hook services still accept the registration, just without metadata.
func addHook(h hooker, event, name, description string, fn func(payload map[string]any) map[string]any) func() {
	if described, ok := h.(describedHooker); ok {
		return described.AddWithDescription(event, name, description, fn)
	}
	return h.Add(event, name, fn)
}

type policer interface {
	SetPolicyContext(fn func(ctx context.Context, cmd string) error)
}
type asker interface {
	Ask(question string, options ...string) (string, error)
}

// ctxAsker is an asker whose question the caller's context releases.
type ctxAsker interface {
	AskContext(ctx context.Context, question string, options ...string) (string, error)
}
type sections interface{ Set(name, text string) }

// Service holds the directories and what has been shown.
type Service struct {
	home, project string
	ask           asker // a fixed one (tests); else findAsk's, per approval
	// findAsk is the ask row's current "ask-answers", looked up on every
	// approval: the ask row mounts after this one, and a reload can
	// remount it without remounting this row. The first one found used to be
	// kept, and after a remount approvals went to the disposed Asker,
	// whose questions no answer could reach.
	findAsk func() asker

	mu       sync.Mutex
	shown    map[string]bool // scoped rule paths already put in front of the model
	reported map[string]bool // parse errors already printed
}

// New returns a Service for a project directory.
func New(home, project string) *Service {
	return &Service{home: home, project: project, shown: map[string]bool{}, reported: map[string]bool{}}
}

// claude reads the Claude rules in force, given the files a code
// block touched (nil when nothing is touched: the central and project
// directories still apply).
func (s *Service) claude(paths []string) []Rule {
	off := offlist.Load(filepath.Join(s.home, ".bough"))
	var out []Rule
	for _, r := range loadClaude(dedupe(ruleDirs(".claude", s.home, s.project, paths))) {
		if !off.Off("rule", r.ID()) {
			out = append(out, r)
		}
	}
	return out
}

// codex reads the Codex prefix rules in force, minus the files turned
// off.
func (s *Service) codex(paths []string) ([]Prefix, []error) {
	rules, errs := loadCodex(dedupe(codexDirs(s.home, s.project, paths)))
	off := offlist.Load(filepath.Join(s.home, ".bough"))
	kept := rules[:0]
	for _, r := range rules {
		if !off.Off("rule", r.File) {
			kept = append(kept, r)
		}
	}
	return kept, errs
}

// Rules is what the rest of bough lists: every rule in force, prose
// and scoped and gate alike, without re-implementing the parsing.
func (s *Service) Rules() []Rule {
	out := s.claude(nil)
	px, _ := s.codex(nil)
	var files []string
	bodies := map[string][]string{}
	for _, r := range px {
		if _, ok := bodies[r.File]; !ok {
			files = append(files, r.File)
		}
		bodies[r.File] = append(bodies[r.File], prefixLine(r))
	}
	for _, f := range files {
		out = append(out, Rule{Path: f, Body: strings.Join(bodies[f], "\n"), gate: true})
	}
	return out
}

// prefixLine renders one prefix rule the way /rules shows it.
func prefixLine(r Prefix) string {
	pattern := make([]string, len(r.Pattern))
	for i, alts := range r.Pattern {
		pattern[i] = strings.Join(alts, "|")
	}
	line := fmt.Sprintf("%s → %s", strings.Join(pattern, " "), r.Decision)
	if r.Justification != "" {
		line += fmt.Sprintf(" (%s)", r.Justification)
	}
	return line
}

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
	for _, r := range s.claude(nil) {
		if !r.Scoped() {
			out = append(out, r.Path)
		}
	}
	return out
}

// Touched is the post-result hook: the scoped rules that match a file
// the code mentioned and have not been shown yet, appended to the
// result so the model reads them with the output.
// TouchedNamed is Touched plus the rule files it injected. A repo rule
// is only ever conditionally in force — it applies when a turn touches
// a file under that repo — so it cannot be listed from a working
// directory the way a central rule can. Naming what fired is the only
// honest way to make it visible.
func (s *Service) TouchedNamed(code string) (string, []string) {
	before := s.shownSet()
	text := s.Touched(code)
	if text == "" {
		return "", nil
	}
	var fired []string
	for _, p := range s.shownSet() {
		if !slices.Contains(before, p) {
			fired = append(fired, s.short(p))
		}
	}
	slices.Sort(fired)
	return text, fired
}

// shownSet is the rule paths already injected this session.
func (s *Service) shownSet() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return slices.Sorted(maps.Keys(s.shown))
}

func (s *Service) Touched(code string) string {
	paths := pathsIn(code)
	if len(paths) == 0 {
		return ""
	}
	root, _ := filepath.Abs(s.project)
	abs := make([]string, 0, len(paths))
	for _, p := range paths {
		if !filepath.IsAbs(p) {
			p = filepath.Join(root, p)
		}
		abs = append(abs, p)
	}
	var b strings.Builder
	for _, r := range s.claude(abs) {
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
	return s.PolicyContext(context.Background(), cmd)
}

// PolicyContext is Policy for a command run under ctx (the block's run,
// the native call's): a prompt rule's question is released by its end.
func (s *Service) PolicyContext(ctx context.Context, cmd string) error {
	rules, errs := s.codex(pathsIn(cmd))
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
		ask := s.ask
		if ask == nil && s.findAsk != nil {
			ask = s.findAsk()
		}
		if ask == nil {
			return nil
		}
		q := fmt.Sprintf("Rule %s asks before running:\n%s", rule, cmd)
		if why.Justification != "" {
			q += "\n" + why.Justification
		}
		var answer string
		var err error
		if c, ok := ask.(ctxAsker); ok {
			answer, err = c.AskContext(ctx, q, "run", "refuse")
		} else {
			answer, err = ask.Ask(q, "run", "refuse")
		}
		if err != nil {
			return fmt.Errorf("command not run: %w", err)
		}
		if a := strings.ToLower(strings.TrimSpace(answer)); a != "run" && a != "yes" && a != "y" {
			// Anything but the refuse button is the user's words: pass
			// them on so they reach the model.
			if a != "" && a != "refuse" {
				return fmt.Errorf("command refused by you (rule %s): %s", rule, strings.TrimSpace(answer))
			}
			return fmt.Errorf("command refused by you (rule %s)", rule)
		}
	}
	return nil
}

// Summary is the /rules command and the prompt section: what is in
// force, from where.
func (s *Service) Summary() string {
	var b strings.Builder
	cr := s.claude(nil)
	px, errs := s.codex(nil)
	if len(cr) == 0 && len(px) == 0 && len(errs) == 0 {
		return "no rules: none under " + strings.Join(append(dedupe(claudeDirs(s.home, s.project)), dedupe(codexDirs(s.home, s.project, nil))...), ", ")
	}
	for _, r := range cr {
		if r.Scoped() {
			fmt.Fprintf(&b, "%s — when working with %s\n", s.short(r.Path), strings.Join(r.Globs, ", "))
		} else {
			fmt.Fprintf(&b, "%s — always\n", s.short(r.Path))
		}
	}
	for _, r := range px {
		fmt.Fprintf(&b, "%s: %s\n", s.short(r.File), prefixLine(r))
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
	for _, r := range s.claude(nil) {
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
		ctx.Effect(addHook(h, "post-result", "rules", "Append matching path-scoped rules to code results once per session, and report which rule files were applied.", func(payload map[string]any) map[string]any {
			code, _ := payload["code"].(string)
			extra, fired := s.TouchedNamed(code)
			if extra == "" {
				return nil
			}
			result, _ := payload["result"].(string)
			// The notice names the files; it reaches the ledger and the
			// transcript, never the model, which already has the rule
			// text itself in the result above.
			return map[string]any{
				"result": result + extra,
				"notice": "applied " + strings.Join(fired, ", "),
			}
		}))
	}
	if p, err := kernel.Get[policer](ctx, "turn-stats"); err == nil {
		p.SetPolicyContext(s.PolicyContext)
		ctx.Effect(func() {
			p.SetPolicyContext(nil)
			policyReloadHold()
		})
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
