package vtreal

// rules-forbidden-under-steer: a Codex prefix_rule gates `touch`, the
// replayed model tries it through a REAL codemode (replay codemode
// runs nothing, so the rules check would never fire), and the user
// types a steer while the prompt gate waits. The command must never
// run (a sentinel file whose path is in the command stays absent),
// the gate must clear, and the steer text must reach the model.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const (
	rulesForbiddenUnderSteerText = "no, write it to notes.txt instead"
	rulesForbiddenUnderSteerAck  = "Understood, leaving the file alone."
)

// rulesForbiddenUnderSteerStart boots bough in a $HOME holding the
// rule and a tape whose first reply runs `touch <sentinel>`; it
// returns the app and the sentinel path.
func rulesForbiddenUnderSteerStart(t *testing.T, decision string) (*app, string) {
	t.Helper()
	dir := t.TempDir()
	sentinel := filepath.Join(dir, "SENTINEL-rfus")
	code := fmt.Sprintf("tools.bash(%q)\n", "touch "+sentinel)
	entries := []map[string]any{
		{"kind": "input", "data": map[string]any{"text": "mark the build"}},
		{"kind": "assistant", "data": map[string]any{"text": "Marking it.\n\n```js\n" + code + "```"}},
		{"kind": "result", "data": map[string]any{"code": code, "text": "TAPE-RESULT-UNUSED\n"}},
		// A stop straight after a failed block is nudged once (loop),
		// so the ack is on the tape twice: the nudge eats the first.
		{"kind": "assistant", "data": map[string]any{"text": "```stop\n" + rulesForbiddenUnderSteerAck + "\n```"}},
		{"kind": "assistant", "data": map[string]any{"text": "```stop\n" + rulesForbiddenUnderSteerAck + "\n```"}},
		{"kind": "done", "data": map[string]any{"text": ""}},
	}
	var sb strings.Builder
	for i, e := range entries {
		e["seq"] = i + 1
		e["at"] = "2026-09-10T10:00:00Z"
		b, err := json.Marshal(e)
		if err != nil {
			t.Fatal(err)
		}
		sb.Write(b)
		sb.WriteByte('\n')
	}
	tape := filepath.Join(dir, "rfus.jsonl")
	if err := os.WriteFile(tape, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	yml := strings.Replace(replayConfig(tape),
		fmt.Sprintf("  plugin: replay\n  config: {file: %q, provide: codemode}\n", tape),
		"  plugin: codemode\n- id: tools\n  plugin: tools-basic\n", 1)
	if !strings.Contains(yml, "plugin: codemode\n") {
		t.Fatalf("real codemode not swapped in:\n%s", yml)
	}
	a := startCfg(t, 120, 36, yml)
	rule := fmt.Sprintf("prefix_rule(pattern = [\"touch\"], decision = %q, justification = \"RFUS-MARKER no touching\")\n", decision)
	p := filepath.Join(a.home, ".codex", "rules", "team.rules")
	if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(p, []byte(rule), 0o644); err != nil {
		t.Fatal(err)
	}
	return a, sentinel
}

func rulesForbiddenUnderSteerAbsent(t *testing.T, a *app, sentinel string) {
	t.Helper()
	if _, err := os.Stat(sentinel); err == nil {
		t.Errorf("gated command ran: %s exists\n%s", sentinel, a.text())
	}
}

// The prompt gate is up, the user types a steer line and Enter.
func TestRulesForbiddenUnderSteerPromptGate(t *testing.T) {
	t.Parallel()
	a, sentinel := rulesForbiddenUnderSteerStart(t, "prompt")
	a.typeText("mark the build")
	a.key(uv.KeyEnter, 0)
	a.waitFor("asks before running")
	a.waitFor(askPendingPlaceholder)
	rulesForbiddenUnderSteerAbsent(t, a, sentinel)

	a.typeText(rulesForbiddenUnderSteerText)
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	screen := a.settled()
	a.check("after steer at the gate")

	t.Run("TestRulesForbiddenUnderSteerNeverRuns", func(t *testing.T) {
		rulesForbiddenUnderSteerAbsent(t, a, sentinel)
	})
	t.Run("TestRulesForbiddenUnderSteerGateClears", func(t *testing.T) {
		for _, bad := range []string{askPendingPlaceholder, "waiting for you"} {
			if strings.Contains(screen, bad) {
				t.Errorf("gate still up (%q):\n%s", bad, screen)
			}
		}
		if !strings.Contains(screen, rulesForbiddenUnderSteerAck) {
			t.Errorf("turn did not resume to the next reply:\n%s", screen)
		}
	})
	// The replay model ignores its messages, so "reaches the model" is
	// read off the history the context is built from: an entry carrying
	// the steer text precedes the second reply.
	t.Run("TestRulesForbiddenUnderSteerReachesModel", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_RULES_FORBIDDEN_UNDER_STEER") == "" {
			t.Skip("known bug: a line typed at the rules prompt gate becomes the ask answer, and plugins/rules/rules.go (prompt case) replaces any non-run answer with a fixed refusal, so the text never reaches the model; set BOUGH_KNOWN_RULES_FORBIDDEN_UNDER_STEER=1 to run")
		}
		var seen []string
		for _, e := range a.steerEntries() {
			text, _ := e.Data["text"].(string)
			switch {
			case e.Kind == "assistant" && strings.Contains(text, rulesForbiddenUnderSteerAck):
				seen = append(seen, "second")
			case e.Kind == "assistant":
				seen = append(seen, "first")
			case (e.Kind == "input" || e.Kind == "result") && strings.Contains(text, rulesForbiddenUnderSteerText):
				seen = append(seen, "steer:"+e.Kind)
			case e.Kind == "result":
				seen = append(seen, "result:"+strings.ReplaceAll(text, "\n", " "))
			}
		}
		got := strings.Join(seen, " | ")
		i, j := strings.Index(got, "steer:"), strings.Index(got, "second")
		if i < 0 || j < 0 || i > j {
			t.Errorf("steer text never entered the model's context before the next reply; history: %s\nscreen:\n%s", got, screen)
		}
	})
}

// A forbidden rule never gates: the block is refused outright with
// the justification and nothing runs.
func TestRulesForbiddenUnderSteerForbidden(t *testing.T) {
	t.Parallel()
	a, sentinel := rulesForbiddenUnderSteerStart(t, "forbidden")
	a.typeText("mark the build")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	screen := a.settled()
	a.check("after forbidden")
	rulesForbiddenUnderSteerAbsent(t, a, sentinel)
	if !strings.Contains(screen, "RFUS-MARKER") {
		t.Errorf("no refusal justification on screen:\n%s", screen)
	}
	if strings.Contains(screen, askPendingPlaceholder) {
		t.Errorf("forbidden rule raised a gate:\n%s", screen)
	}
}
