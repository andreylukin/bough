package vtreal

// The rules plugin seen from the real binary: Claude rules under
// .claude/rules and a Codex prefix_rule under .codex/rules, seeded in
// the temp $HOME (which is also the cwd, so home and project rules are
// the same directory). The prompt side is read back through SYSTEM!,
// the scoped rule through a replayed block's result, and the Codex
// gate through a real CODE! block, since replay codemode runs nothing.

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

const (
	rulesAlways = "# House style\nRULES-ALWAYS-MARKER: tabs, not spaces.\n"
	rulesScoped = "---\npaths: \"src/api/**/*.ts\"\n---\n# API\nRULES-SCOPED-MARKER: validate every input.\n"
	rulesCodex  = "prefix_rule(pattern = [\"echo\"], decision = \"forbidden\", justification = \"RULES-CODEX-MARKER no echo here\")\n"
)

// rulesStart is startCfg with the rule files written before boot: the
// scoped-rules prompt section is set once, when the row mounts.
func rulesStart(t *testing.T, cols, rows int, yml string) *app {
	t.Helper()
	home := t.TempDir()
	for p, body := range map[string]string{
		".claude/rules/style.md":   rulesAlways,
		".claude/rules/api/api.md": rulesScoped,
		".codex/rules/team.rules":  rulesCodex,
		"bough.yml":                yml,
	} {
		p = filepath.Join(home, p)
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	term, err := NewTerminal(t, cols, rows)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", filepath.Join(home, "bough.yml"))
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=",
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: cols, rows: rows, home: home}
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

// rulesEntries is the text of every history entry of one kind in this
// run's $HOME.
func rulesEntries(a *app, kind string) []string {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var out []string
	for _, p := range paths {
		entries, err := history.Read(p)
		if err != nil {
			continue
		}
		for _, e := range entries {
			if e.Kind == kind {
				s, _ := e.Data["text"].(string)
				out = append(out, s)
			}
		}
	}
	return out
}

func rulesSend(a *app, text string, turn int) {
	a.t.Helper()
	a.typeText(text)
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(turn, 60*time.Second) {
		a.t.Fatalf("turn %d (%q) never finished:\n%s", turn, text, a.text())
	}
}

// The unscoped rule joins the preamble and the scoped one is only
// announced (glob + file), its body held back until a touch.
func TestRulesPromptViaSystem(t *testing.T) {
	t.Parallel()
	a := rulesStart(t, 100, 30, config)
	rulesSend(a, "SYSTEM!", 1)
	a.check("after SYSTEM!")
	replies := rulesEntries(a, "assistant")
	if len(replies) == 0 {
		t.Fatalf("no assistant entry in history:\n%s", a.text())
	}
	// The prompt is not in a stop fence, so the loop may nudge and the
	// echo answers again: the prompt is in one of the replies.
	prompt := strings.Join(replies, "\n")
	for _, want := range []string{
		"RULES-ALWAYS-MARKER",
		"Path-scoped rules",
		"- src/api/**/*.ts: .claude/rules/api/api.md",
	} {
		if !strings.Contains(prompt, want) {
			t.Errorf("prompt lacks %q\nscreen:\n%s", want, a.text())
		}
	}
	if strings.Contains(prompt, "RULES-SCOPED-MARKER") {
		t.Errorf("scoped rule body is in the prompt before any touch\nscreen:\n%s", a.text())
	}
}

// /rules lists all three with where they came from.
func TestRulesCommandLists(t *testing.T) {
	t.Parallel()
	a := rulesStart(t, 120, 30, config)
	a.typeText("/rules")
	a.key(uv.KeyEnter, 0)
	for _, want := range []string{
		".claude/rules/style.md — always",
		".claude/rules/api/api.md — when working with src/api/**/*.ts",
		".codex/rules/team.rules: echo → forbidden",
	} {
		a.waitFor(want)
	}
	a.check("after /rules")
}

// A real block running a forbidden prefix is refused with the
// justification; the echo model repeats the result, so it is on screen.
func TestRulesCodexForbidsPrefix(t *testing.T) {
	t.Parallel()
	a := rulesStart(t, 120, 30, config)
	rulesSend(a, "CODE!", 1)
	a.waitFor("RULES-CODEX-MARKER")
	a.check("after CODE!")
	if s := a.text(); !strings.Contains(s, "✗ error: command refused by rule echo") {
		t.Errorf("no refusal line on screen:\n%s", s)
	}
	// The home dir is also the cwd, so the file may print as ~/... or ./...
	res := rulesEntries(a, "result")
	if len(res) == 0 || !strings.Contains(res[0], "command refused by rule echo (") ||
		!strings.Contains(res[0], ".codex/rules/team.rules): RULES-CODEX-MARKER") ||
		strings.Contains(res[0], "\nhi from codemode") {
		t.Errorf("result entries %q\nscreen:\n%s", res, a.text())
	}
}

// A replayed block touching src/api/users.ts gets the scoped rule
// appended to its result exactly once, and the ui shows it in the
// block when expanded.
func TestRulesScopedShownOnTouch(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/rules.jsonl")
	a := rulesStart(t, 120, 40, replayConfig(tape))
	rulesSend(a, "show me the users api", 1)
	a.check("turn 1")
	rulesSend(a, "again", 2)
	a.check("turn 2")
	res := rulesEntries(a, "result")
	if len(res) != 2 {
		t.Fatalf("want 2 result entries, got %q\nscreen:\n%s", res, a.text())
	}
	if !strings.Contains(res[0], "[rule: .claude/rules/api/api.md — applies to src/api/**/*.ts]") || !strings.Contains(res[0], "RULES-SCOPED-MARKER") {
		t.Errorf("first touch lacks the rule: %q\nscreen:\n%s", res[0], a.text())
	}
	if strings.Contains(res[1], "RULES-SCOPED-MARKER") {
		t.Errorf("rule shown twice: %q\nscreen:\n%s", res[1], a.text())
	}
	// Expand the first result block: the rule notice is in it.
	row := -1
	for i, l := range a.lines() {
		if strings.Contains(l, "▸ result") {
			row = i
			break
		}
	}
	if row < 0 {
		t.Fatalf("no collapsed result block:\n%s", a.text())
	}
	a.click(2, row)
	a.waitFor("RULES-SCOPED-MARKER")
	a.check("expanded")
}
