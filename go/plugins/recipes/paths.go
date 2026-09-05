package recipes

import (
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
)

// A session started in $HOME and pointed at one clone after another
// carries no useful working directory: what a turn is about is in the
// paths its code and prompt name. Paths finds them; Repos maps them to
// the checkouts they live in.

var (
	// absolute or ~/ paths, and the target of a `cd`: what shell code
	// and tools.readFile("...") calls name.
	absPath = regexp.MustCompile(`(?:~|/(?:Users|home|private|tmp|opt|var|etc|srv))/[\w./@+~-]*[\w/]`)
	cdPath  = regexp.MustCompile(`\bcd\s+(?:--\s+)?([\w./~-]+)`)
	// a bare relative path with a slash or a known root name, as in
	// "the tests in go/plugins/loop" or "look at go/".
	relPath = regexp.MustCompile(`(?:^|[\s"'(=])((?:\.{1,2}/|[\w-]+/)[\w./-]*)`)
)

// Paths extracts the file-system paths text names, resolving relative
// ones against each of bases in turn (the first that exists wins; an
// unresolved one is kept relative to the first base). Duplicates are
// dropped; order is first mention.
func Paths(text string, bases ...string) []string {
	home, _ := os.UserHomeDir()
	var out []string
	add := func(p string) {
		p = strings.TrimRight(p, "/")
		if p == "" || slices.Contains(out, p) {
			return
		}
		out = append(out, p)
	}
	resolve := func(rel string) {
		rel = strings.TrimRight(rel, "/.")
		if rel == "" {
			return
		}
		for _, b := range bases {
			if b == "" {
				continue
			}
			p := filepath.Join(b, rel)
			if _, err := os.Stat(p); err == nil {
				add(p)
				return
			}
		}
		for _, b := range bases {
			if b != "" {
				add(filepath.Join(b, rel))
				return
			}
		}
	}
	for _, m := range absPath.FindAllString(text, -1) {
		if strings.HasPrefix(m, "~/") {
			m = filepath.Join(home, m[2:])
		}
		add(m)
	}
	for _, m := range cdPath.FindAllStringSubmatch(text, -1) {
		p := m[1]
		if strings.HasPrefix(p, "/") || strings.HasPrefix(p, "~") {
			continue // the absolute pass has it
		}
		resolve(p)
	}
	for _, m := range relPath.FindAllStringSubmatch(text, -1) {
		p := m[1]
		if strings.Contains(p, "://") || strings.HasPrefix(p, "//") {
			continue
		}
		resolve(p)
	}
	return out
}

// Repos maps paths to the git checkouts holding them: the nearest
// ancestor with a .git, looked up on disk now (a deleted clone yields
// nothing; a deleted file inside a living clone still resolves).
// Sorted, unique.
func Repos(paths []string) []string {
	var out []string
	for _, p := range paths {
		if r := gitRoot(p); r != "" && !slices.Contains(out, r) {
			out = append(out, r)
		}
	}
	slices.Sort(out)
	return out
}

// gitRoot is the nearest ancestor of p (p included) holding a .git,
// or "".
func gitRoot(p string) string {
	for d := filepath.Clean(p); ; d = filepath.Dir(d) {
		if _, err := os.Stat(filepath.Join(d, ".git")); err == nil {
			return d
		}
		if filepath.Dir(d) == d {
			return ""
		}
	}
}
