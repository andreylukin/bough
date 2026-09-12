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
	"time"
)

// Rule is one rules file.
type Rule struct {
	Path  string   // on disk
	Globs []string // empty = unscoped
	Body  string   // the markdown after the frontmatter
	gate  bool     // a Codex .rules file of prefix_rules
}

// Scoped reports whether the rule applies only to some files.
func (r Rule) Scoped() bool { return len(r.Globs) > 0 }

// ID is how the rest of bough names this rule — in the off list, in
// the UI. The absolute path is stable across restarts.
func (r Rule) ID() string { return r.Path }

// Kind says what the rule does: "prose" joins the preamble every
// turn, "scoped" waits until a matching file is touched, "gate" is a
// Codex prefix_rule file that decides shell commands.
func (r Rule) Kind() string {
	switch {
	case r.gate:
		return "gate"
	case r.Scoped():
		return "scoped"
	}
	return "prose"
}

// claudeDirs is where rules live: user first, then project, so a
// project rule reads after (and so stacks on top of) a user one.
func claudeDirs(home, project string) []string {
	return ruleDirs(".claude", home, project, nil)
}

// ruleDirs is the search path for one tool's rules directory: the
// central one under home, the project's, and then — for every file a
// code block touched — the rules directory of each ancestor of that
// file, walking up to home and no further.
//
// The ancestors matter because the user runs bough from their home
// directory, so home and project are the same place and a rule kept
// in ~/repos/foo/.claude/rules would otherwise never be found. A repo
// rule STACKS with the central one, so the nearest directory comes
// last and its rule reads last.
func ruleDirs(sub, home, project string, paths []string) []string {
	var dirs []string
	if home != "" {
		dirs = append(dirs, filepath.Join(home, sub, "rules"))
	}
	dirs = append(dirs, filepath.Join(project, sub, "rules"))
	return append(dirs, ancestorDirs(sub, home, paths)...)
}

// ancestorDirs walks up from each touched file to home, collecting
// rules directories shallowest first. The walk stops at home and
// never leaves it, so a stray absolute path elsewhere on the disk
// contributes nothing and we never climb to /.
func ancestorDirs(sub, home string, paths []string) []string {
	if home == "" || len(paths) == 0 {
		return nil
	}
	home = filepath.Clean(home)
	prefix := home + string(filepath.Separator)
	seen := map[string]bool{}
	var out []string
	for _, p := range paths {
		var chain []string
		for d := filepath.Dir(filepath.Clean(p)); strings.HasPrefix(d, prefix); d = filepath.Dir(d) {
			chain = append(chain, filepath.Join(d, sub, "rules"))
		}
		for i := len(chain) - 1; i >= 0; i-- {
			if !seen[chain[i]] {
				seen[chain[i]] = true
				out = append(out, chain[i])
			}
		}
	}
	return out
}

// loadClaude reads every .md under the rules dirs, recursively, in
// path order. Missing dirs are fine. Read fresh on every call, so a
// rule added mid-session counts on the next turn.
func loadClaude(dirs []string) []Rule {
	var out []Rule
	for _, dir := range dirs {
		for _, f := range mdFiles(dir) {
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

// mdFiles lists the .md files under a rules directory, recursively,
// in path order. Walking every ancestor of every touched path on
// every tool call would stat a great deal, so the listing is cached
// per directory and thrown away when the directory's mtime or size
// changes — which is when files are added or removed. The files
// themselves are still read fresh, so editing a rule mid-session
// counts on the next turn.
var listing struct {
	sync.Mutex
	dirs map[string]*listingEntry
}

type listingEntry struct {
	files []string
	mtime time.Time
	size  int64
}

func mdFiles(dir string) []string {
	var mtime time.Time
	var size int64
	st, err := os.Stat(dir)
	if err == nil {
		if !st.IsDir() {
			return nil
		}
		mtime, size = st.ModTime(), st.Size()
	}

	listing.Lock()
	defer listing.Unlock()
	if listing.dirs == nil {
		listing.dirs = map[string]*listingEntry{}
	}
	if e, ok := listing.dirs[dir]; ok && e.mtime.Equal(mtime) && e.size == size {
		return e.files
	}

	var files []string
	if err == nil {
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
	}
	listing.dirs[dir] = &listingEntry{files: files, mtime: mtime, size: size}
	return files
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
