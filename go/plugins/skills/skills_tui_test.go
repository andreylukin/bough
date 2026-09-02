// TUI-integration tests: real kernel + skills + loop + llm-echo + the
// real ui model. TestMain sandboxes HOME ($HOME/.claude/skills is a
// scanned pool); parallel tests each own a uniquely named skill dir.
package skills_test

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"

	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/llm"
	_ "github.com/andreylukin/bough/plugins/loop"
)

func TestMain(m *testing.M) {
	home, err := os.MkdirTemp("", "bough-skills-tui-home-*")
	if err != nil {
		panic(err)
	}
	os.Setenv("HOME", home)
	code := m.Run()
	os.RemoveAll(home)
	os.Exit(code)
}

// writeSkill installs $HOME/.claude/skills/<name>/SKILL.md atomically
// (pools are rescanned on every Inject).
func writeSkill(t *testing.T, name, body string) {
	t.Helper()
	home, err := os.UserHomeDir()
	if err != nil {
		t.Fatal(err)
	}
	dir := filepath.Join(home, ".claude", "skills", name)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	tmp := filepath.Join(dir, "SKILL.tmp")
	if err := os.WriteFile(tmp, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(tmp, filepath.Join(dir, "SKILL.md")); err != nil {
		t.Fatal(err)
	}
}

// Mentioning a skill injects its SKILL.md into the turn: the echoed
// reply (last user message) carries the marker into the transcript.
func TestMentionedSkillInjectedIntoTranscript(t *testing.T) {
	t.Parallel()
	writeSkill(t, "zzskilldeploy", "SKILLMARK_DEPLOY steps live here.")
	d := uitest.Mount(t, nil, "codemode", "llm-echo", "skills", "loop")
	d.Say("please run zzskilldeploy for me")
	d.WaitFor("SKILLMARK_DEPLOY")
	if !strings.Contains(d.Frame(), "zzskilldeploy") {
		t.Fatalf("skill label missing:\n%s", d.Frame())
	}
}

// A skill that is not mentioned stays out of the turn.
func TestUnmentionedSkillNotInjected(t *testing.T) {
	t.Parallel()
	writeSkill(t, "zzskillsecret", "SKILLMARK_SECRET must not leak.")
	d := uitest.Mount(t, nil, "codemode", "llm-echo", "skills", "loop")
	d.Say("nothing relevant at all")
	d.WaitFor("echo: nothing relevant at all")
	if strings.Contains(d.Frame(), "SKILLMARK_SECRET") {
		t.Fatalf("unmentioned skill injected:\n%s", d.Frame())
	}
}

// A skill is a "/name" palette command: typing "/zzskillcmd" lists it,
// and Enter submits the line to the loop with the skill injected.
func TestSkillIsSlashCommand(t *testing.T) {
	t.Parallel()
	writeSkill(t, "zzskillcmd", "---\ndescription: \"Run the cmd thing.\"\n---\nSKILLMARK_CMD here.")
	d := uitest.Mount(t, nil, "codemode", "llm-echo", "commands", "skills", "loop")
	d.Type("/zzskillcmd")
	if f := d.Frame(); !strings.Contains(f, "skill: Run the cmd thing") {
		t.Fatalf("palette should list the skill with its description:\n%s", f)
	}
	d.Type(" go")
	d.Press("enter")
	d.WaitFor("SKILLMARK_CMD")
	if !strings.Contains(d.Frame(), "echo: /zzskillcmd go") {
		t.Fatalf("the / line should reach the loop as input:\n%s", d.Frame())
	}
}
