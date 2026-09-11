package vtreal

// bough.yml rewritten while a reply streams, switching the llm row to
// a second replay tape. The streaming turn must finish on the row it
// started on (its assistant entry names model-alpha), the next turn
// must run on the new row (model-beta), the status bar's model chip
// must not show model-beta before the first turn ends, and the reload
// must not leave duplicate notices or turns behind. Both rows are the
// replay plugin, so the recorded provider is "replay" either way; the
// tapes' recorded models tell the rows apart.

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// configReloadLlmRowSwapMidStreamTape is one turn answered by model:
// a stop reply of n words tagged tag, ending in last.
func configReloadLlmRowSwapMidStreamTape(t *testing.T, name, input, model, tag, last string, n int) string {
	t.Helper()
	words := make([]string, n)
	for i := range words {
		words[i] = fmt.Sprintf("%s%02d", tag, i)
	}
	reply := "```stop\n" + strings.Join(append(words, last), " ") + "\n```"
	return statusbarSeed(t, name,
		statusbarEntry("meta", map[string]any{"cwd": "/tmp"}),
		statusbarEntry("input", map[string]any{"text": input}),
		statusbarEntry("assistant", map[string]any{"text": reply, "model": model, "provider": "replay"}),
		statusbarEntry("done", map[string]any{"text": ""}),
	)
}

// configReloadLlmRowSwapMidStreamYml is the project overlay: one
// replay llm row (the embedded base supplies the rest).
func configReloadLlmRowSwapMidStreamYml(tape string, delayMs int) string {
	return fmt.Sprintf(`
- id: llm
  plugin: replay
  config: {file: %q, delay_ms: %d}
`, tape, delayMs) + statusbarQuietRows
}

func configReloadLlmRowSwapMidStreamStatus(a *app) string {
	ls := a.lines()
	for i := len(ls) - 1; i >= 0; i-- {
		if strings.Contains(ls[i], "? keys") {
			return ls[i]
		}
	}
	return ""
}

func configReloadLlmRowSwapMidStreamAssistants(a *app) []history.Entry {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var out []history.Entry
	for _, p := range paths {
		entries, _ := history.Read(p)
		for _, e := range entries {
			if e.Kind == "assistant" {
				out = append(out, e)
			}
		}
	}
	return out
}

func TestConfigReloadLlmRowSwapMidStream(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	work := filepath.Join(home, "work")
	for _, d := range []string{filepath.Join(home, ".bough"), work} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	alpha := configReloadLlmRowSwapMidStreamTape(t, "alpha.jsonl", "first turn", "model-alpha", "wa", "ALPHA_DONE", 50)
	beta := configReloadLlmRowSwapMidStreamTape(t, "beta.jsonl", "second turn", "model-beta", "wb", "BETA_DONE", 3)
	cfg := filepath.Join(work, "bough.yml")
	if err := os.WriteFile(cfg, []byte(configReloadLlmRowSwapMidStreamYml(alpha, 150)), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, 120, 30)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = work
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=",
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: 120, rows: 30, home: home}
	t.Cleanup(func() {
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	a.waitFor("say something")

	a.typeText("first turn")
	a.key(uv.KeyEnter, 0)
	a.waitFor("wa00") // the first streamed word: the reply is mid-stream

	// Sample the chip until the first turn is recorded done: model-beta
	// must never show while it runs.
	var early []string
	sampled := make(chan struct{})
	go func() {
		defer close(sampled)
		deadline := time.Now().Add(30 * time.Second)
		for a.doneCount() < 1 && time.Now().Before(deadline) {
			if s := configReloadLlmRowSwapMidStreamStatus(a); strings.Contains(s, "model-beta") {
				early = append(early, s)
			}
			time.Sleep(30 * time.Millisecond)
		}
	}()
	if err := os.WriteFile(cfg, []byte(configReloadLlmRowSwapMidStreamYml(beta, 0)), 0o644); err != nil {
		t.Fatal(err)
	}
	finished := a.waitDone(1, 30*time.Second)
	<-sampled
	if !finished {
		t.Fatalf("the streaming turn never finished after the llm row was swapped:\n%s", a.text())
	}
	if len(early) > 0 {
		t.Errorf("model chip showed model-beta while the first turn still ran: %q", early[0])
	}

	t.Run("FirstTurnOnOldRow", func(t *testing.T) {
		as := configReloadLlmRowSwapMidStreamAssistants(a)
		if len(as) != 1 {
			t.Fatalf("%d assistant entries after turn 1, want 1: %v", len(as), as)
		}
		if as[0].Data["model"] != "model-alpha" || as[0].Data["provider"] != "replay" {
			t.Fatalf("turn 1 entry = %v, want model-alpha/replay", as[0].Data)
		}
		if txt, _ := as[0].Data["text"].(string); !strings.Contains(txt, "ALPHA_DONE") {
			t.Fatalf("turn 1 reply was cut short by the swap: %q", txt)
		}
		a.waitFor("ALPHA_DONE")
		a.check("after turn 1")
	})

	t.Run("SecondTurnOnNewRow", func(t *testing.T) {
		a.typeText("second turn")
		a.key(uv.KeyEnter, 0)
		if !a.waitDone(2, 30*time.Second) {
			t.Fatalf("second turn never finished:\n%s", a.text())
		}
		a.waitFor("BETA_DONE")
		as := configReloadLlmRowSwapMidStreamAssistants(a)
		if len(as) != 2 {
			t.Fatalf("%d assistant entries after turn 2, want 2: %v", len(as), as)
		}
		if as[1].Data["model"] != "model-beta" || as[1].Data["provider"] != "replay" {
			t.Fatalf("turn 2 entry = %v, want model-beta/replay", as[1].Data)
		}
		a.waitUntil(func(string) bool {
			return strings.Contains(configReloadLlmRowSwapMidStreamStatus(a), "model-beta")
		}, "status bar model chip to name model-beta after turn 2")
		a.check("after turn 2")
	})

	t.Run("NoDuplicateNotices", func(t *testing.T) {
		s := a.settled()
		if n := strings.Count(s, "? keys"); n != 1 {
			t.Fatalf("%d status bars, want 1:\n%s", n, s)
		}
		for _, bad := range []string{"already provided by row", "reload:", "WARNING"} {
			if strings.Contains(s, bad) {
				t.Fatalf("reload left %q on screen:\n%s", bad, s)
			}
		}
		if n := strings.Count(s, "ALPHA_DONE"); n != 1 {
			t.Fatalf("turn 1's reply drawn %d times, want 1:\n%s", n, s)
		}
	})
}
