package rules

// Claude Code rules: markdown files under .claude/rules/ (project,
// recursive) and ~/.claude/rules/ (user). A file with no `paths:`
// frontmatter is an unscoped rule and joins the context preamble like
// CLAUDE.md; one with `paths:` (a comma-separated string or a YAML
// list of globs) is scoped and is shown only when the agent works
// with a matching file.

import (
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"sync"
)

// Rule is one rules file.
type Rule struct {
	Path  string   // on disk
	Globs []string // empty = unscoped
	Body  string   // the markdown after the frontmatter
}

// Scoped reports whether the rule applies only to some files.
func (r Rule) Scoped() bool { return len(r.Globs) > 0 }

// claudeDirs is where rules live: user first, then project, so a
// project rule reads after (and so overrides) a user one.
func claudeDirs(home, project string) []string {
	var dirs []string
	if home != "" {
		dirs = append(dirs, filepath.Join(home, ".claude", "rules"))
	}
	dirs = append(dirs, filepath.Join(project, ".claude", "rules"))
	return dirs
}

// loadClaude reads every .md under the rules dirs, recursively, in
// path order. Missing dirs are fine. Read fresh on every call, so a
// rule added mid-session counts on the next turn.
func loadClaude(dirs []string) []Rule {
	var out []Rule
	for _, dir := range dirs {
		var files []string
		_ = filepath.WalkDir(dir, func(p string, d os.DirEntry, err error) error {
			if err != nil {
				return nil
			}
			if !d.IsDir() && strings.HasSuffix(p, ".md") {
				files = append(files, p)
			}
			return nil
		})
		sort.Strings(files)
		for _, f := range files {
			body, err := os.ReadFile(f)
			if err != nil {
				continue
			}
			globs, text := frontmatter(string(body))
			if strings.TrimSpace(text) == "" {
				continue
			}
			out = append(out, Rule{Path: f, Globs: globs, Body: text})
		}
	}
	return out
}

// frontmatter splits a leading --- block off body and returns the
// `paths` globs it names. Claude Code accepts a quoted or bare
// comma-separated string and a YAML list; so do we.
func frontmatter(body string) (globs []string, rest string) {
	if !strings.HasPrefix(body, "---\n") && !strings.HasPrefix(body, "---\r\n") {
		return nil, body
	}
	end := strings.Index(body[3:], "\n---")
	if end < 0 {
		return nil, body
	}
	fm := body[4 : 3+end]
	rest = strings.TrimPrefix(body[3+end+4:], "\n")
	rest = strings.TrimPrefix(rest, "\r\n")
	inPaths := false
	for _, line := range strings.Split(fm, "\n") {
		line = strings.TrimRight(line, "\r")
		trimmed := strings.TrimSpace(line)
		switch {
		case strings.HasPrefix(trimmed, "paths:"):
			inPaths = true
			// "a,b" · a,b · ["a", "b"] — all seen in the wild.
			value := strings.Trim(unquote(strings.TrimPrefix(trimmed, "paths:")), "[]")
			for _, g := range strings.Split(value, ",") {
				if g = unquote(g); g != "" {
					globs = append(globs, g)
				}
			}
		case inPaths && strings.HasPrefix(trimmed, "- "):
			if g := unquote(strings.TrimPrefix(trimmed, "- ")); g != "" {
				globs = append(globs, g)
			}
		case trimmed == "" || strings.HasPrefix(trimmed, "#"):
		default:
			inPaths = false
		}
	}
	return globs, rest
}

func unquote(s string) string {
	s = strings.TrimSpace(s)
	if len(s) >= 2 && (s[0] == '"' || s[0] == '\'') && s[len(s)-1] == s[0] {
		s = s[1 : len(s)-1]
	}
	return s
}

// Matches reports whether path (relative to the project) matches any
// of the rule's globs. A glob without a slash matches the base name
// anywhere, as in .gitignore; ** spans directories; {a,b} expands.
func (r Rule) Matches(path string) bool {
	path = filepath.ToSlash(path)
	for _, g := range r.Globs {
		for _, alt := range expandBraces(g) {
			if globRe(alt).MatchString(path) {
				return true
			}
		}
	}
	return false
}

var (
	reMu    sync.Mutex
	reCache = map[string]*regexp.Regexp{}
)

// globRe compiles a glob into an anchored regexp.
func globRe(glob string) *regexp.Regexp {
	reMu.Lock()
	defer reMu.Unlock()
	if re, ok := reCache[glob]; ok {
		return re
	}
	var b strings.Builder
	b.WriteString("^")
	if !strings.Contains(glob, "/") {
		b.WriteString("(.*/)?")
	}
	glob = strings.TrimPrefix(glob, "./")
	for i := 0; i < len(glob); i++ {
		c := glob[i]
		switch {
		case c == '*' && i+1 < len(glob) && glob[i+1] == '*':
			i++
			if i+1 < len(glob) && glob[i+1] == '/' {
				i++
				b.WriteString("(.*/)?")
			} else {
				b.WriteString(".*")
			}
		case c == '*':
			b.WriteString("[^/]*")
		case c == '?':
			b.WriteString("[^/]")
		default:
			b.WriteString(regexp.QuoteMeta(string(c)))
		}
	}
	b.WriteString("$")
	re := regexp.MustCompile(b.String())
	reCache[glob] = re
	return re
}

// expandBraces turns "src/**/*.{ts,tsx}" into its alternatives.
func expandBraces(glob string) []string {
	open := strings.Index(glob, "{")
	if open < 0 {
		return []string{glob}
	}
	close := strings.Index(glob[open:], "}")
	if close < 0 {
		return []string{glob}
	}
	close += open
	var out []string
	for _, alt := range strings.Split(glob[open+1:close], ",") {
		out = append(out, expandBraces(glob[:open]+strings.TrimSpace(alt)+glob[close+1:])...)
	}
	return out
}

// pathsIn finds the file paths a code block or command mentions: any
// token with a slash or a file extension, quotes stripped. Over-
// matching is harmless (a non-file token matches no rule).
var pathToken = regexp.MustCompile(`[A-Za-z0-9_.@~-]*(?:/[A-Za-z0-9_.@*-]+)+|[A-Za-z0-9_-]+\.[A-Za-z0-9]{1,8}\b`)

func pathsIn(text string) []string {
	seen := map[string]bool{}
	var out []string
	for _, tok := range pathToken.FindAllString(text, -1) {
		tok = strings.TrimPrefix(tok, "./")
		if tok == "" || seen[tok] || strings.HasPrefix(tok, "http") {
			continue
		}
		seen[tok] = true
		out = append(out, tok)
	}
	return out
}
