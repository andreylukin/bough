package artifacts

// OpenUI Lang as this row handles it: a program is a list of
// statements, `identifier = Expression`, one per line (an expression
// may run on until the next statement starts). The real parser lives
// in the browser bundle; here we only need enough of the shape to
// refuse an obviously wrong publish before it goes out, and to merge
// a patch statement-by-statement the way the language's own
// mergeStatements does.

import (
	"errors"
	"fmt"
	"regexp"
	"strings"
)

// stmtRe starts a statement: a name (or $variable) and an equals.
var stmtRe = regexp.MustCompile(`^\s*(\$?[A-Za-z_][A-Za-z0-9_]*)\s*=`)

// statement is one named definition, its text kept verbatim.
type statement struct {
	name string
	text string // the statement's lines, joined
}

// split breaks a program into statements. Comment and blank lines
// before the first statement are dropped; later ones attach to the
// statement above (a continuation, as far as we know).
func split(code string) []statement {
	var out []statement
	for _, line := range strings.Split(strings.ReplaceAll(code, "\r\n", "\n"), "\n") {
		if m := stmtRe.FindStringSubmatch(line); m != nil {
			out = append(out, statement{name: m[1], text: line})
			continue
		}
		if len(out) == 0 {
			continue
		}
		out[len(out)-1].text += "\n" + line
	}
	for i := range out {
		out[i].text = strings.TrimRight(out[i].text, " \t\n")
	}
	return out
}

// check refuses what the browser would reject before the page opens:
// no statements, no root, a name defined twice. It is not the parser;
// the page reports the rest (unknown components, bad arguments) as
// structured errors the agent is told about.
func check(code string) error {
	stmts := split(code)
	if len(stmts) == 0 {
		return errors.New("artifact code has no statements: OpenUI Lang is one `name = Expression` per line, starting with root = Card([...]) — read tools.artifactGuide() first")
	}
	seen := map[string]bool{}
	hasRoot := false
	for _, s := range stmts {
		if seen[s.name] {
			return fmt.Errorf("artifact code defines %q twice; every name is defined once", s.name)
		}
		seen[s.name] = true
		if s.name == "root" {
			hasRoot = true
		}
	}
	if !hasRoot {
		return errors.New("artifact code has no root: the entry point is root = Card([...]) (first line, so the shell renders before the rest)")
	}
	if strings.Contains(code, "```") {
		return errors.New("artifact code contains a markdown fence; pass the OpenUI Lang statements only")
	}
	return nil
}

// merge applies patch to base statement by statement: a statement in
// patch replaces the one in base with the same name (in place), a new
// one is appended, and `name = null` removes it. root moved to the
// front if a patch introduced it elsewhere. Everything else in base
// is kept verbatim.
func merge(base, patch string) string {
	stmts := split(base)
	index := map[string]int{}
	for i, s := range stmts {
		index[s.name] = i
	}
	for _, p := range split(patch) {
		removed := strings.TrimSpace(strings.SplitN(p.text, "=", 2)[1]) == "null"
		if i, ok := index[p.name]; ok {
			if removed {
				stmts[i].text = ""
			} else {
				stmts[i] = p
			}
			continue
		}
		if removed {
			continue
		}
		index[p.name] = len(stmts)
		stmts = append(stmts, p)
	}
	var out []string
	for _, s := range stmts {
		if s.text == "" {
			continue
		}
		if s.name == "root" {
			out = append([]string{s.text}, out...)
		} else {
			out = append(out, s.text)
		}
	}
	return strings.Join(out, "\n") + "\n"
}
