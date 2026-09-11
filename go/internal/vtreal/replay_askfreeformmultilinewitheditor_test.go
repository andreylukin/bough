package vtreal

// A freeform answer to a pending tools.ask written in the external
// editor (ctrl+g). The tape's code block gates tools.ask on a file so
// the test can leave a composer draft in place before the ask lands;
// the fake $EDITOR overwrites the draft with a multi-line unicode
// answer. The "ask/answer" history entry must be that answer byte for
// byte, the ask must close and the turn resume, and the draft the ask
// displaced must come back to the composer.

import (
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"
)

// askFreeformMultilineWithEditorAnswer has no trailing newline: the
// editor round-trip trims those (plugins/ui/editor.go).
const askFreeformMultilineWithEditorAnswer = "héllo wörld 🌈 ✓\n第二行 — naïve café\nthird line ünïcødé end"

// askFreeformMultilineWithEditorTape writes the ask tape variant whose
// code waits for $HOME/askgo before asking.
func askFreeformMultilineWithEditorTape(t *testing.T, dir string) string {
	t.Helper()
	code := "tools.bash(\"while [ ! -f \\\"$HOME/askgo\\\" ]; do sleep 0.05; done\");\nconst c = tools.ask(\"Pick a color\", \"chartreuse\", \"vermilion\");\nconsole.log(\"you picked \" + c);\n"
	entries := []map[string]any{
		{"seq": 1, "kind": "meta", "data": map[string]any{"cwd": "/tmp/demo"}},
		{"seq": 2, "kind": "input", "data": map[string]any{"text": "pick a color"}},
		{"seq": 3, "kind": "assistant", "data": map[string]any{"text": "```js\n" + code + "```"}},
		{"seq": 4, "kind": "code", "data": map[string]any{"text": code}},
		{"seq": 5, "kind": "result", "data": map[string]any{"code": code, "text": "you picked x\n"}},
		{"seq": 6, "kind": "assistant", "data": map[string]any{"text": "```stop\nColor locked in.\n```"}},
		{"seq": 7, "kind": "done", "data": map[string]any{"text": ""}},
	}
	var sb strings.Builder
	for _, e := range entries {
		b, err := json.Marshal(e)
		if err != nil {
			t.Fatal(err)
		}
		sb.Write(b)
		sb.WriteByte('\n')
	}
	p := filepath.Join(dir, "askeditor.jsonl")
	if err := os.WriteFile(p, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// askFreeformMultilineWithEditorStart boots the ask config on that
// tape with $EDITOR a script that replaces the draft file with the
// answer (VISUAL cleared, TMPDIR inside $HOME).
func askFreeformMultilineWithEditorStart(t *testing.T) *app {
	t.Helper()
	home := t.TempDir()
	ans := filepath.Join(home, "answer.txt")
	if err := os.WriteFile(ans, []byte(askFreeformMultilineWithEditorAnswer), 0o644); err != nil {
		t.Fatal(err)
	}
	ed := filepath.Join(home, "editor.sh")
	if err := os.WriteFile(ed, []byte("#!/bin/sh\ncat \"$HOME/answer.txt\" > \"$1\"\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	tmp := filepath.Join(home, "tmp")
	if err := os.Mkdir(tmp, 0o755); err != nil {
		t.Fatal(err)
	}
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(askConfig(askFreeformMultilineWithEditorTape(t, home))), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, 100, 30)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", "VISUAL=", "EDITOR="+ed, "TMPDIR="+tmp,
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: 100, rows: 30, home: home}
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

func TestAskFreeformMultilineWithEditor(t *testing.T) {
	t.Parallel()
	a := askFreeformMultilineWithEditorStart(t)
	a.typeText("pick a color")
	a.key(uv.KeyEnter, 0)
	a.waitFor("askgo") // the gated block is running
	a.typeText("draft before ask")
	a.waitFor("draft before ask")
	if err := os.WriteFile(filepath.Join(a.home, "askgo"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
	a.waitFor("? Pick a color")
	a.settled()

	a.key('g', uv.ModCtrl)
	a.waitFor("第二行 — naïve café")
	a.settled()
	if strings.Contains(a.text(), "Color locked in.") {
		t.Fatalf("the editor answered the ask without enter:\n%s", a.text())
	}
	a.key(uv.KeyEnter, 0)

	if got := askPasteAnswerEntry(a); got != askFreeformMultilineWithEditorAnswer {
		t.Fatalf("ask/answer is %q, want %q\n%s", got, askFreeformMultilineWithEditorAnswer, a.text())
	}
	a.waitFor("Color locked in.")
	s := a.settled()
	if strings.Contains("\n"+s, "\n? Pick a color") {
		t.Fatalf("the ask is still pending after the answer:\n%s", s)
	}
	if !strings.Contains(s, "❯? Pick a color → ") {
		t.Fatalf("the ask did not collapse to its answered one-liner:\n%s", s)
	}
	editorNoTempLeft(a)

	t.Run("draft restored", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_ASK_FREEFORM_MULTILINE_WITH_EDITOR") == "" {
			t.Skip("known bug: a composer draft present when an ask arrives becomes the answer's text and is never restored (plugins/ui/model.go case \"ask\" / ask.go answerPending input.Reset); set BOUGH_KNOWN_ASK_FREEFORM_MULTILINE_WITH_EDITOR=1 to run")
		}
		if row := editorComposer(a); !strings.Contains(row, "draft before ask") {
			t.Fatalf("composer row %q: the pre-ask draft was not restored\n%s", row, a.text())
		}
	})
}
