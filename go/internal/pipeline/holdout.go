package pipeline

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"unicode"

	"github.com/andreylukin/bough/internal/projectdef"
)

// withheld replaces a holdout node's reply that quoted a holdout file.
const withheld = "[validator feedback withheld: it quoted the acceptance criteria]"

// holdoutSources is every file the pipeline's holdout globs match, in
// node then glob order, without duplicates.
func holdoutSources(p *Pipeline) ([]string, error) {
	var out []string
	seen := map[string]bool{}
	for _, name := range sortedNames(p) {
		for _, g := range p.Nodes[name].Holdout {
			matches, err := filepath.Glob(filepath.Join(p.Dir, g))
			if err != nil {
				return nil, fmt.Errorf("holdout %q: %w", g, err)
			}
			if len(matches) == 0 {
				return nil, fmt.Errorf("holdout %q matches no file under %s", g, p.Dir)
			}
			for _, m := range matches {
				if !seen[m] {
					seen[m] = true
					out = append(out, m)
				}
			}
		}
	}
	return out, nil
}

// stageHoldout copies the holdout files into <runDir>/holdout (0600)
// and returns the staged paths. {{holdout_dir}} names only that copy.
func stageHoldout(p *Pipeline, runDir string) ([]string, error) {
	src, err := holdoutSources(p)
	if err != nil || len(src) == 0 {
		return nil, err
	}
	dir := filepath.Join(runDir, "holdout")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, err
	}
	var staged []string
	for _, s := range src {
		dst := filepath.Join(dir, filepath.Base(s))
		if _, err := os.Stat(dst); err == nil {
			return nil, fmt.Errorf("holdout: two files named %s", filepath.Base(s))
		}
		b, err := os.ReadFile(s)
		if err != nil {
			return nil, fmt.Errorf("holdout: %w", err)
		}
		if err := os.WriteFile(dst, b, 0o600); err != nil {
			return nil, fmt.Errorf("holdout: %w", err)
		}
		staged = append(staged, dst)
	}
	return staged, nil
}

// preflightHoldout refuses holdout files a project node could reach: one
// tracked in a project's repo checkout, or one under the dirs orbs mount.
func preflightHoldout(p *Pipeline, staged []string, home string) error {
	if len(staged) == 0 {
		return nil
	}
	src, err := holdoutSources(p)
	if err != nil {
		return err
	}
	shared := []string{"orbs", "scratch", "projects"}
	for _, s := range src {
		abs := resolveExisting(s)
		for _, d := range shared {
			root := resolveExisting(filepath.Join(home, ".bough", d))
			if within(root, abs) {
				return fmt.Errorf("holdout: %s lies under ~/.bough/%s, which project sessions can reach", s, d)
			}
		}
	}
	slugs := map[string]bool{}
	for _, n := range p.Nodes {
		if n.Type == "agent" && n.Mode == "project" {
			slugs[n.Project] = true
		}
	}
	for slug := range slugs {
		proj, err := projectdef.Load(home, slug)
		if err != nil {
			return fmt.Errorf("holdout preflight: %w", err)
		}
		for _, r := range proj.Def.Repos {
			if r.Path == "" {
				continue // a remote repo has no host checkout to leak from
			}
			repo := projectdef.ExpandPath(home, r.Path)
			for _, s := range src {
				c := exec.Command("git", "-C", repo, "ls-files", "--error-unmatch", s)
				if c.Run() == nil {
					return fmt.Errorf("holdout: %s is tracked in %s, the checkout project %s works on", s, repo, slug)
				}
			}
		}
	}
	return nil
}

func within(root, path string) bool {
	rel, err := filepath.Rel(root, path)
	return err == nil && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator))
}

// resolveExisting is EvalSymlinks on the longest existing prefix.
func resolveExisting(abs string) string {
	rest := ""
	for p := abs; ; p = filepath.Dir(p) {
		if r, err := filepath.EvalSymlinks(p); err == nil {
			return filepath.Join(r, rest)
		}
		if filepath.Dir(p) == p {
			return abs
		}
		rest = filepath.Join(filepath.Base(p), rest)
	}
}

// shingleWords is the overlap that counts as quoting: short common
// phrases share fewer words than this by chance.
const shingleWords = 8

func words(s string) []string {
	return strings.FieldsFunc(strings.ToLower(s), func(r rune) bool {
		return !unicode.IsLetter(r) && !unicode.IsDigit(r)
	})
}

// leaks reports whether reply shares any 8-word shingle (lower-cased,
// punctuation stripped) with a holdout file, and returns that phrase.
func leaks(reply string, holdout []string) (bool, string) {
	set := map[string]bool{}
	for _, h := range holdout {
		b, err := os.ReadFile(h)
		if err != nil {
			continue
		}
		w := words(string(b))
		for i := 0; i+shingleWords <= len(w); i++ {
			set[strings.Join(w[i:i+shingleWords], " ")] = true
		}
	}
	w := words(reply)
	for i := 0; i+shingleWords <= len(w); i++ {
		if s := strings.Join(w[i:i+shingleWords], " "); set[s] {
			return true, s
		}
	}
	return false, ""
}

// verdictOf reads the last non-empty line: exactly VERDICT: PASS or
// VERDICT: FAIL. ok is false for anything else, which routes as FAIL.
func verdictOf(reply string) (pass bool, ok bool) {
	lines := strings.Split(reply, "\n")
	for i := len(lines) - 1; i >= 0; i-- {
		l := strings.TrimSpace(lines[i])
		if l == "" {
			continue
		}
		switch l {
		case "VERDICT: PASS":
			return true, true
		case "VERDICT: FAIL":
			return false, true
		}
		return false, false
	}
	return false, false
}
