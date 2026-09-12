package rules

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func write(t *testing.T, path, body string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

// Every documented paths: spelling is read, and a file without one is
// unscoped.
func TestFrontmatterSpellings(t *testing.T) {
	for _, c := range []struct {
		body string
		want []string
	}{
		{"---\npaths: \"src/api/**/*,src/services/**/*\"\n---\nbody", []string{"src/api/**/*", "src/services/**/*"}},
		{"---\npaths: src/api/**/*.ts\n---\nbody", []string{"src/api/**/*.ts"}},
		{"---\npaths:\n  - \"src/**/*.{ts,tsx}\"\n  - 'tests/**/*.test.ts'\nother: x\n---\nbody", []string{"src/**/*.{ts,tsx}", "tests/**/*.test.ts"}},
		{"---\npaths: [\"a/*.go\", \"b/*.go\"]\n---\nbody", []string{"a/*.go", "b/*.go"}},
		{"# Just a rule\nbody", nil},
	} {
		globs, rest := frontmatter(c.body)
		if strings.Join(globs, "|") != strings.Join(c.want, "|") || !strings.Contains(rest, "body") {
			t.Errorf("%q: globs %v rest %q", c.body, globs, rest)
		}
	}
}

func TestGlobs(t *testing.T) {
	for _, c := range []struct {
		glob, path string
		want       bool
	}{
		{"src/api/**/*.ts", "src/api/users.ts", true},
		{"src/api/**/*.ts", "src/api/v2/deep/users.ts", true},
		{"src/api/**/*.ts", "src/web/users.ts", false},
		{"**/*.ts", "a/b/c.ts", true},
		{"*.md", "README.md", true},
		{"*.md", "docs/README.md", true}, // no slash: any directory, like .gitignore
		{"src/components/*.tsx", "src/components/Button.tsx", true},
		{"src/components/*.tsx", "src/components/x/Button.tsx", false},
		{"src/**/*.{ts,tsx}", "src/a/b.tsx", true},
		{"src/**/*.{ts,tsx}", "src/a/b.js", false},
		{"go/plugins/**", "go/plugins/rules/rules.go", true},
	} {
		if got := (Rule{Globs: []string{c.glob}}).Matches(c.path); got != c.want {
			t.Errorf("%s ~ %s = %v", c.glob, c.path, got)
		}
	}
}

// A scoped rule is shown once, with the result of the first block that
// touches a matching file; unscoped ones feed the preamble.
func TestScopedRulesShownOnTouch(t *testing.T) {
	home, project := t.TempDir(), t.TempDir()
	write(t, filepath.Join(home, ".claude", "rules", "style.md"), "# Style\nTabs.")
	write(t, filepath.Join(project, ".claude", "rules", "api", "api.md"), "---\npaths: \"src/api/**/*.ts\"\n---\n# API\nValidate input.")
	s := New(home, project)
	if u := s.Unscoped(); len(u) != 1 || !strings.HasSuffix(u[0], "style.md") {
		t.Fatalf("unscoped = %v", u)
	}
	if got := s.Touched(`tools.view("src/web/app.ts")`); got != "" {
		t.Fatalf("unrelated file showed %q", got)
	}
	got := s.Touched(`tools.patch('` + filepath.Join(project, "src/api/users.ts") + `', "a", "b")`)
	if !strings.Contains(got, "[rule: .claude/rules/api/api.md — applies to src/api/**/*.ts]") || !strings.Contains(got, "Validate input.") {
		t.Fatalf("got %q", got)
	}
	if again := s.Touched(`tools.view("src/api/other.ts")`); again != "" {
		t.Fatalf("shown twice: %q", again)
	}
	if sum := s.Summary(); !strings.Contains(sum, "api.md — when working with src/api/**/*.ts") || !strings.Contains(sum, "style.md — always") {
		t.Fatalf("summary: %s", sum)
	}
}

const docRules = `# Prompt before running commands with the prefix gh pr view.
prefix_rule(
    pattern = ["gh", "pr", "view"],
    decision = "prompt",
    justification = "Viewing PRs is allowed with approval",
    match = ["gh pr view 7888"],
    not_match = ["gh pr --repo openai/codex view 7888"],
)
prefix_rule(pattern = ["git", ["push", "force-push"]], decision = "forbidden", justification = "Open a PR instead")
prefix_rule(pattern = ["ls"])
`

func TestParseAndCheck(t *testing.T) {
	rules, err := parseRules(docRules)
	if err != nil {
		t.Fatal(err)
	}
	if len(rules) != 3 || rules[0].Decision != "prompt" || rules[2].Decision != "allow" || len(rules[1].Pattern[1]) != 2 {
		t.Fatalf("rules = %+v", rules)
	}
	for _, c := range []struct{ cmd, want string }{
		{"gh pr view 7888 --json title", "prompt"},
		{"gh pr --repo openai/codex view 7888", "allow"},
		{"git push origin main", "forbidden"},
		{"FOO=1 sudo git force-push", "forbidden"},
		{"ls -la | git push", "forbidden"}, // strictest part of a pipeline wins
		{"echo 'git push'", "allow"},       // quoted, not a command
		{"gh pr view 1 && ls", "prompt"},
	} {
		d, _, _ := Check(rules, c.cmd)
		if d != c.want {
			t.Errorf("%q = %s, want %s", c.cmd, d, c.want)
		}
	}
	if _, err := parseRules(`prefix_rule(pattern = ["a"], decision = "maybe")`); err == nil || !strings.Contains(err.Error(), "line 1: decision must be") {
		t.Fatalf("bad decision: %v", err)
	}
	if _, err := parseRules("prefix_rule(\n  pattern = \"a\"\n)"); err == nil || !strings.Contains(err.Error(), "pattern must be a non-empty list") {
		t.Fatalf("bad pattern: %v", err)
	}
}

type fakeAsk struct{ answer, asked string }

func (f *fakeAsk) Ask(q string, _ ...string) (string, error) { f.asked = q; return f.answer, nil }

// The bash policy: forbidden refuses with the justification, prompt
// asks, a broken file does not take the good ones down.
func TestPolicy(t *testing.T) {
	home, project := t.TempDir(), t.TempDir()
	write(t, filepath.Join(home, ".codex", "rules", "default.rules"), docRules)
	write(t, filepath.Join(project, ".codex", "rules", "broken.rules"), "prefix_rule(pattern = )")
	write(t, filepath.Join(project, ".codex", "rules", "team.rules"), `prefix_rule(pattern = ["rm", "-rf"], decision = "forbidden")`)
	s := New(home, project)
	if err := s.Policy("ls -la"); err != nil {
		t.Fatal(err)
	}
	err := s.Policy("git push origin main")
	if err == nil || !strings.Contains(err.Error(), "refused by rule git push|force-push (~/.codex/rules/default.rules): Open a PR instead") {
		t.Fatalf("forbidden: %v", err)
	}
	if err := s.Policy("rm -rf /"); err == nil || !strings.Contains(err.Error(), ".codex/rules/team.rules") {
		t.Fatalf("project forbidden: %v", err)
	}
	if err := s.Policy("gh pr view 1"); err != nil {
		t.Fatalf("prompt without an asker should allow: %v", err)
	}
	ask := &fakeAsk{answer: "refuse"}
	s.ask = ask
	if err := s.Policy("gh pr view 1"); err == nil || !strings.Contains(err.Error(), "refused by you") || !strings.Contains(ask.asked, "gh pr view 1") {
		t.Fatalf("prompt refused: %v / %q", err, ask.asked)
	}
	ask.answer = "no, write it to notes.txt instead"
	if err := s.Policy("gh pr view 1"); err == nil || !strings.HasSuffix(err.Error(), ": no, write it to notes.txt instead") {
		t.Fatalf("typed answer dropped: %v", err)
	}
	ask.answer = "run"
	if err := s.Policy("gh pr view 1"); err != nil {
		t.Fatalf("prompt allowed: %v", err)
	}
	if sum := s.Summary(); !strings.Contains(sum, "error: ") || !strings.Contains(sum, "broken.rules") || !strings.Contains(sum, "rm -rf → forbidden") {
		t.Fatalf("summary: %s", sum)
	}
	var target error = errors.New("x")
	_ = target
}

// The user runs bough from home, so project == home and the two
// central directories collapse into one. A rule kept in a repo deep
// under home must still be found — and it stacks with the central
// rule rather than replacing it.
func TestRepoRulesStackWithCentral(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	repo := filepath.Join(home, "repos", "foo")
	write(t, filepath.Join(home, ".claude", "rules", "uni.md"), "---\npaths: \"**/*.py\"\n---\nCentral: type hints.")
	write(t, filepath.Join(repo, ".claude", "rules", "local.md"), "---\npaths: \"**/*.py\"\n---\nRepo: no bare except.")
	s := New(home, home)

	got := s.Touched(`tools.patch("` + filepath.Join(repo, "app", "main.py") + `", "a", "b")`)
	if !strings.Contains(got, "Central: type hints.") || !strings.Contains(got, "Repo: no bare except.") {
		t.Fatalf("both rules should apply, got %q", got)
	}
	if strings.Index(got, "Central:") > strings.Index(got, "Repo:") {
		t.Fatalf("the nearest rule should read last: %q", got)
	}
}

// The walk up stops at home: a rules directory above it is never read.
func TestWalkStopsAtHome(t *testing.T) {
	t.Parallel()
	above := t.TempDir()
	home := filepath.Join(above, "home")
	write(t, filepath.Join(above, ".claude", "rules", "outside.md"), "---\npaths: \"**/*.py\"\n---\nOutside.")
	write(t, filepath.Join(home, "repos", "foo", "x.py"), "print()")
	s := New(home, home)
	if got := s.Touched(`tools.view("` + filepath.Join(home, "repos", "foo", "x.py") + `")`); got != "" {
		t.Fatalf("read a rule above home: %q", got)
	}
	for _, d := range ancestorDirs(".claude", home, []string{filepath.Join(home, "repos", "foo", "x.py")}) {
		if !strings.HasPrefix(d, home) {
			t.Fatalf("walked outside home: %s", d)
		}
	}
}

// A rule listed in ~/.bough/off.yml does not load.
func TestOffRuleDoesNotLoad(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	repo := filepath.Join(home, "repos", "foo")
	central := filepath.Join(home, ".claude", "rules", "uni.md")
	local := filepath.Join(repo, ".claude", "rules", "local.md")
	write(t, central, "---\npaths: \"**/*.py\"\n---\nCentral: type hints.")
	write(t, local, "---\npaths: \"**/*.py\"\n---\nRepo: no bare except.")
	write(t, filepath.Join(home, ".bough", "off.yml"), "off:\n  - rule:"+local+"\n")

	s := New(home, home)
	got := s.Touched(`tools.view("` + filepath.Join(repo, "app", "main.py") + `")`)
	if !strings.Contains(got, "Central: type hints.") || strings.Contains(got, "Repo:") {
		t.Fatalf("off rule loaded: %q", got)
	}
}

// Rules() lists everything in force and says what kind each is.
func TestRulesKinds(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	write(t, filepath.Join(home, ".claude", "rules", "style.md"), "# Style\nTabs.")
	write(t, filepath.Join(home, ".claude", "rules", "api.md"), "---\npaths: \"src/**/*.ts\"\n---\nValidate.")
	write(t, filepath.Join(home, ".codex", "rules", "default.rules"), docRules)

	kinds := map[string]string{}
	for _, r := range New(home, home).Rules() {
		kinds[filepath.Base(r.ID())] = r.Kind()
	}
	want := map[string]string{"style.md": "prose", "api.md": "scoped", "default.rules": "gate"}
	for name, k := range want {
		if kinds[name] != k {
			t.Errorf("%s = %q, want %q", name, kinds[name], k)
		}
	}
}

// A repo rule is conditionally in force, so it can never be listed from
// a working directory — the only way to see it is to name it when it
// fires. Without this the stacking works and is invisible.
func TestTouchedNamesWhatFired(t *testing.T) {
	home := t.TempDir()
	write := func(dir, name, body string) {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(dir, name), []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	write(filepath.Join(home, ".claude", "rules"), "central.md",
		"---\npaths:\n  - \"**/*.py\"\n---\nCENTRAL-BODY\n")
	write(filepath.Join(home, "repos", "demo", ".claude", "rules"), "repo.md",
		"---\npaths:\n  - \"**/*.py\"\n---\nREPO-BODY\n")

	// project == home: the user always runs bough from their home dir.
	s := New(home, home)
	text, fired := s.TouchedNamed(`tools.view("repos/demo/app/main.py")`)
	if text == "" {
		t.Fatal("no rules injected for a .py under a repo")
	}
	if !strings.Contains(text, "CENTRAL-BODY") || !strings.Contains(text, "REPO-BODY") {
		t.Errorf("rules did not STACK; got:\n%s", text)
	}
	if len(fired) != 2 {
		t.Fatalf("fired = %v, want both rule files named", fired)
	}
	joined := strings.Join(fired, " ")
	if !strings.Contains(joined, "central.md") || !strings.Contains(joined, "repo.md") {
		t.Errorf("fired = %v, want central.md and repo.md", fired)
	}

	// Already-shown rules are not re-reported on a second touch.
	if _, again := s.TouchedNamed(`tools.view("repos/demo/app/other.py")`); len(again) != 0 {
		t.Errorf("re-reported already-injected rules: %v", again)
	}
}
