package rules

// Codex rules: .rules files under .codex/rules/ (project) and
// ~/.codex/rules/ (user), each a list of prefix_rule(...) calls that
// say what to do with a shell command by its leading words —
// allow, prompt, or forbidden. bough has no permission prompts of its
// own, so: forbidden refuses the command with the justification,
// prompt asks the user when an ask service is mounted (and allows
// otherwise), allow is the default anyway.

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

// Prefix is one prefix_rule: a pattern of positions, each a set of
// accepted words.
type Prefix struct {
	Pattern       [][]string
	Decision      string // allow, prompt, forbidden
	Justification string
	File          string
}

// codexDirs is where the files live, user then project, then the
// ancestors of any file the command mentions.
func codexDirs(home, project string, paths []string) []string {
	return ruleDirs(".codex", home, project, paths)
}

// loadCodex parses every .rules file in dirs. A file that fails to
// parse is reported and skipped; the others still apply (Codex itself
// drops them all, which is the bug its users complain about).
func loadCodex(dirs []string) (rules []Prefix, errs []error) {
	for _, dir := range dirs {
		entries, err := os.ReadDir(dir)
		if err != nil {
			continue
		}
		var names []string
		for _, e := range entries {
			if !e.IsDir() && strings.HasSuffix(e.Name(), ".rules") {
				names = append(names, e.Name())
			}
		}
		sort.Strings(names)
		for _, n := range names {
			p := filepath.Join(dir, n)
			body, err := os.ReadFile(p)
			if err != nil {
				continue
			}
			rs, err := parseRules(string(body))
			if err != nil {
				errs = append(errs, fmt.Errorf("%s: %w", p, err))
				continue
			}
			for i := range rs {
				rs[i].File = p
			}
			rules = append(rules, rs...)
		}
	}
	return rules, errs
}

// Check applies the rules to cmd: the most restrictive decision among
// the matching rules wins (forbidden > prompt > allow). Every command
// in a pipeline or a && chain is checked; a shell line is only as
// allowed as its strictest part.
func Check(rules []Prefix, cmd string) (decision string, why Prefix, matched bool) {
	rank := map[string]int{"allow": 0, "prompt": 1, "forbidden": 2}
	decision = "allow"
	for _, part := range splitCommands(cmd) {
		words := shellWords(part)
		for _, r := range rules {
			if !r.matches(words) {
				continue
			}
			if !matched || rank[r.Decision] > rank[decision] {
				decision, why, matched = r.Decision, r, true
			}
		}
	}
	return decision, why, matched
}

func (r Prefix) matches(words []string) bool {
	if len(words) < len(r.Pattern) {
		return false
	}
	for i, alts := range r.Pattern {
		ok := false
		for _, a := range alts {
			if a == words[i] {
				ok = true
				break
			}
		}
		if !ok {
			return false
		}
	}
	return true
}

// splitCommands breaks a shell line at |, ||, &&, ; and newlines,
// outside quotes. Good enough for a prefix check; a heredoc body is
// harmless extra parts.
func splitCommands(cmd string) []string {
	var parts []string
	var cur strings.Builder
	quote := byte(0)
	flush := func() {
		if s := strings.TrimSpace(cur.String()); s != "" {
			parts = append(parts, s)
		}
		cur.Reset()
	}
	for i := 0; i < len(cmd); i++ {
		c := cmd[i]
		switch {
		case quote != 0:
			if c == quote {
				quote = 0
			}
			cur.WriteByte(c)
		case c == '\'' || c == '"':
			quote = c
			cur.WriteByte(c)
		case c == '\\' && i+1 < len(cmd):
			cur.WriteByte(c)
			i++
			cur.WriteByte(cmd[i])
		case c == '|' || c == '&' || c == ';' || c == '\n':
			flush()
		default:
			cur.WriteByte(c)
		}
	}
	flush()
	return parts
}

// shellWords splits one command into words, honouring quotes, and
// drops leading VAR=value assignments and a leading sudo/env so the
// rule sees the program the user meant.
func shellWords(cmd string) []string {
	var words []string
	var cur strings.Builder
	quote := byte(0)
	in := false
	for i := 0; i < len(cmd); i++ {
		c := cmd[i]
		switch {
		case quote != 0:
			if c == quote {
				quote = 0
			} else {
				cur.WriteByte(c)
			}
		case c == '\'' || c == '"':
			quote, in = c, true
		case c == '\\' && i+1 < len(cmd):
			i++
			cur.WriteByte(cmd[i])
			in = true
		case c == ' ' || c == '\t':
			if in {
				words = append(words, cur.String())
				cur.Reset()
				in = false
			}
		default:
			cur.WriteByte(c)
			in = true
		}
	}
	if in {
		words = append(words, cur.String())
	}
	for len(words) > 0 {
		w := words[0]
		if eq := strings.Index(w, "="); eq > 0 && !strings.ContainsAny(w[:eq], "/-") {
			words = words[1:] // FOO=bar prefix
			continue
		}
		if w == "sudo" || w == "env" || w == "command" || w == "exec" {
			words = words[1:]
			continue
		}
		break
	}
	return words
}

// parseRules reads the prefix_rule(...) calls in a .rules file. The
// grammar is the Starlark subset Codex documents: keyword arguments,
// string literals, lists (a list inside pattern is a union), comments,
// trailing commas. Anything else is an error naming the line.
func parseRules(src string) ([]Prefix, error) {
	p := &parser{src: src}
	var out []Prefix
	for {
		p.skip()
		if p.eof() {
			return out, nil
		}
		name := p.ident()
		if name != "prefix_rule" {
			return nil, p.errorf("expected prefix_rule, got %q", name)
		}
		p.skip()
		if !p.eat('(') {
			return nil, p.errorf("expected ( after prefix_rule")
		}
		r := Prefix{Decision: "allow"}
		for {
			p.skip()
			if p.eat(')') {
				break
			}
			key := p.ident()
			p.skip()
			if !p.eat('=') {
				return nil, p.errorf("expected = after %s", key)
			}
			p.skip()
			val, err := p.value()
			if err != nil {
				return nil, err
			}
			switch key {
			case "pattern":
				list, ok := val.([]any)
				if !ok || len(list) == 0 {
					return nil, p.errorf("pattern must be a non-empty list")
				}
				for _, el := range list {
					switch e := el.(type) {
					case string:
						r.Pattern = append(r.Pattern, []string{e})
					case []any:
						var alts []string
						for _, a := range e {
							s, ok := a.(string)
							if !ok {
								return nil, p.errorf("pattern union must hold strings")
							}
							alts = append(alts, s)
						}
						r.Pattern = append(r.Pattern, alts)
					default:
						return nil, p.errorf("pattern element must be a string or a list")
					}
				}
			case "decision":
				s, _ := val.(string)
				if s != "allow" && s != "prompt" && s != "forbidden" {
					return nil, p.errorf("decision must be allow, prompt or forbidden")
				}
				r.Decision = s
			case "justification":
				r.Justification, _ = val.(string)
			case "match", "not_match":
				// Inline examples; not needed to apply the rule.
			default:
				return nil, p.errorf("unknown field %s", key)
			}
			p.skip()
			p.eat(',')
		}
		if len(r.Pattern) == 0 {
			return nil, p.errorf("prefix_rule needs a pattern")
		}
		out = append(out, r)
	}
}

type parser struct {
	src string
	pos int
}

func (p *parser) eof() bool { return p.pos >= len(p.src) }

func (p *parser) errorf(format string, a ...any) error {
	line := 1 + strings.Count(p.src[:min(p.pos, len(p.src))], "\n")
	return fmt.Errorf("line %d: %s", line, fmt.Sprintf(format, a...))
}

// skip passes whitespace and # comments.
func (p *parser) skip() {
	for !p.eof() {
		c := p.src[p.pos]
		switch {
		case c == ' ' || c == '\t' || c == '\n' || c == '\r':
			p.pos++
		case c == '#':
			for !p.eof() && p.src[p.pos] != '\n' {
				p.pos++
			}
		default:
			return
		}
	}
}

func (p *parser) eat(c byte) bool {
	if !p.eof() && p.src[p.pos] == c {
		p.pos++
		return true
	}
	return false
}

func (p *parser) ident() string {
	start := p.pos
	for !p.eof() {
		c := p.src[p.pos]
		if c == '_' || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') {
			p.pos++
		} else {
			break
		}
	}
	return p.src[start:p.pos]
}

// value parses a string literal or a list of values.
func (p *parser) value() (any, error) {
	if p.eof() {
		return nil, p.errorf("unexpected end of file")
	}
	switch c := p.src[p.pos]; {
	case c == '"' || c == '\'':
		return p.str(c)
	case c == '[':
		p.pos++
		var list []any
		for {
			p.skip()
			if p.eat(']') {
				return list, nil
			}
			v, err := p.value()
			if err != nil {
				return nil, err
			}
			list = append(list, v)
			p.skip()
			p.eat(',')
		}
	}
	return nil, p.errorf("expected a string or a list")
}

func (p *parser) str(q byte) (string, error) {
	p.pos++
	var b strings.Builder
	for !p.eof() {
		c := p.src[p.pos]
		p.pos++
		switch {
		case c == q:
			return b.String(), nil
		case c == '\\' && !p.eof():
			e := p.src[p.pos]
			p.pos++
			switch e {
			case 'n':
				b.WriteByte('\n')
			case 't':
				b.WriteByte('\t')
			default:
				b.WriteByte(e)
			}
		default:
			b.WriteByte(c)
		}
	}
	return "", p.errorf("unterminated string")
}
