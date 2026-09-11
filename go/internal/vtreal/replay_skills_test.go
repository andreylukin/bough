package vtreal

// Skills catalog (plugins/skills) on the real binary: a $HOME seeded
// with two skills and one broken one, booted with the echo config plus
// the skills row. Skills register their /name commands at mount, so the
// pool must exist before boot — hence skillsStart instead of startCfg.

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"
)

// skills mounts before ui: the ui reads the service at mount for the
// startup header.
var skillsConfig = strings.Replace(config, "- id: ui\n", "- id: skills\n  plugin: skills\n- id: ui\n", 1)

// skillsStart is startCfg with $HOME/.claude/skills seeded first:
// alpha and bravo are well-formed, broken's SKILL.md is a directory
// (it scans as present, every read fails).
func skillsStart(t *testing.T) *app {
	t.Helper()
	home := t.TempDir()
	pool := filepath.Join(home, ".claude", "skills")
	for name, body := range map[string]string{
		"alpha": "---\ndescription: \"Use when the user wants alpha things.\"\n---\nSKILLMARK_ALPHA body.\n",
		"bravo": "---\ndescription: \"Bravo does the bravo job.\"\n---\nSKILLMARK_BRAVO body.\n",
	} {
		if err := os.MkdirAll(filepath.Join(pool, name), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(pool, name, "SKILL.md"), []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.MkdirAll(filepath.Join(pool, "broken", "SKILL.md"), 0o755); err != nil {
		t.Fatal(err)
	}
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(skillsConfig), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, 120, 40)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=",
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: 120, rows: 40, home: home}
	t.Cleanup(func() {
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	a.waitFor("say something")
	return a
}

func (a *app) skillsSay(s string) {
	a.typeText(s)
	a.key(uv.KeyEnter, 0)
}

// The startup header lists the catalogue, broken skill included (it
// scans as present); boot survives the broken SKILL.md.
func TestSkillsHeaderListsCatalogue(t *testing.T) {
	t.Parallel()
	a := skillsStart(t)
	a.waitFor("skills: 3 (alpha, bravo, broken)")
	a.check("skills header")
}

// /help groups the skill commands under a "skills" heading with their
// summarized descriptions (trigger phrase dropped, capitalized).
func TestSkillsHelpListing(t *testing.T) {
	t.Parallel()
	a := skillsStart(t)
	a.skillsSay("/help")
	a.waitFor("skill: Bravo does the bravo job")
	s := a.settled()
	for _, want := range []string{"/alpha", "/bravo", "skill: The user wants alpha things"} {
		if !strings.Contains(s, want) {
			t.Errorf("/help missing %q:\n%s", want, s)
		}
	}
	a.check("after /help")
}

// "/bravo go" submits the line as input; the mention injects bravo's
// SKILL.md (echo returns the last user message, injection included)
// and only bravo's.
func TestSkillsSlashCommandInjects(t *testing.T) {
	t.Parallel()
	a := skillsStart(t)
	a.skillsSay("/bravo go")
	a.waitFor("SKILLMARK_BRAVO")
	s := a.settled()
	if !strings.Contains(s, "/bravo go") {
		t.Errorf("the / line should reach the loop as input:\n%s", s)
	}
	if strings.Contains(s, "SKILLMARK_ALPHA") {
		t.Errorf("unmentioned skill injected:\n%s", s)
	}
	a.check("after /bravo")
}

// Mentioning a skill in prose injects it too.
func TestSkillsMentionInjects(t *testing.T) {
	t.Parallel()
	a := skillsStart(t)
	a.skillsSay("please use alpha here")
	a.waitFor("SKILLMARK_ALPHA")
	a.check("after mention")
}

// The system prompt carries no skill catalogue today: skills reach the
// model only by injection into the user turn. This pins the current
// contract; if a catalogue section is added, flip the assertion.
func TestSkillsSystemPromptHasNoCatalogue(t *testing.T) {
	t.Parallel()
	a := skillsStart(t)
	a.skillsSay("SYSTEM!")
	if !a.waitDone(1, 30e9) {
		t.Fatalf("SYSTEM! turn never finished:\n%s", a.text())
	}
	s := a.settled()
	if strings.Contains(s, "SKILLMARK_") {
		t.Errorf("skill body leaked into the system prompt:\n%s", s)
	}
}

// Mentioning the broken skill is not fatal: the turn completes, the
// app keeps running, and a later good skill still injects. The read
// error goes to stderr only — there is no on-screen report to assert.
// a.check is not used: after this turn the composer shows a ghost
// prediction flush against the prompt (">echo: try broken now"), which
// composerRow's "> " test rejects — a composer quirk, not a skills one —
// so the crash and status-bar invariants are asserted directly.
func TestSkillsBrokenSkillNotFatal(t *testing.T) {
	t.Parallel()
	a := skillsStart(t)
	a.skillsSay("try broken now")
	a.waitFor("echo: try broken now")
	a.skillsSay("/alpha again")
	a.waitFor("SKILLMARK_ALPHA")
	s := a.settled()
	if panicky.MatchString(s) || !strings.Contains(s, "? keys") {
		t.Fatalf("crash text or missing status bar after a broken skill:\n%s", s)
	}
	if strings.Contains(s, "skill injected: broken") {
		t.Errorf("broken skill reported as injected:\n%s", s)
	}
}
