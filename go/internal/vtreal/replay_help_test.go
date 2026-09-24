package vtreal

// /help on the real binary: a panel over the transcript (built-ins
// first), gone after esc, and skill descriptions written as YAML block
// scalars or left out never show "skill: |" or a bare "skill:".

import (
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"
)

func TestHelpPanelDismissible(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	pool := filepath.Join(home, ".claude", "skills")
	for name, body := range map[string]string{
		"zblock": "---\ndescription: |\n  Does the block thing\n---\nbody\n",
		"zfold":  "---\ndescription: >-\n  Folds things\n---\nbody\n",
		"znone":  "---\nname: znone\n---\nbody\n",
	} {
		if err := os.MkdirAll(filepath.Join(pool, name), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(pool, name, "SKILL.md"), []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(skillsConfig), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, 140, 45)
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
	a := &app{t: t, term: term, cmd: cmd, cols: 140, rows: 45, home: home}
	t.Cleanup(func() {
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	a.waitFor("say something")

	a.typeText("/help")
	a.key(uv.KeyEnter, 0)
	a.waitFor("/help commands")
	screen := a.settled()
	lines := strings.Split(screen, "\n")
	head := -1
	for i, l := range lines {
		if strings.Contains(l, "/help commands") {
			head = i
			break
		}
	}
	if head < 0 || head+1 >= len(lines) || !regexp.MustCompile(`^\s*/(artifacts|clear|help|keys|model)\b`).MatchString(lines[head+1]) {
		t.Fatalf("the panel's first row should name a built-in command:\n%s", screen)
	}
	if !strings.Contains(screen, "/zblock") || !strings.Contains(screen, "Does the block thing") || !strings.Contains(screen, "Folds things") {
		t.Errorf("skills should list with their block-scalar descriptions:\n%s", screen)
	}
	bad := regexp.MustCompile(`skill: *\||skill:\s*$`)
	for _, l := range lines {
		if bad.MatchString(l) {
			t.Errorf("row %q shows an empty or raw block-scalar skill description", l)
		}
	}

	a.key(uv.KeyEscape, 0)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "/help commands") }, "help panel to close")
	after := a.settled()
	for _, want := range []string{"/help", "/zblock", "Does the block thing"} {
		if strings.Contains(after, want) {
			t.Errorf("after esc the transcript should hold no help rows (%q):\n%s", want, after)
		}
	}
}
