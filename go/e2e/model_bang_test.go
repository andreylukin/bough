// Wave-2 features, end to end: /model (picker choices, list, live swap via the
// launcher's config-set / Reconcile path, surviving hot reload) and
// "!" bash mode (direct shell, [system] output, command/system history,
// never an LLM turn).
package e2e

import (
	"bufio"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func TestHeadlessModelShowSwapAndReload(t *testing.T) {
	t.Parallel()
	b := launchHeadless(t, launchOpts{})

	// Bare /model is the picker: headless prints its choices.
	b.send("/model")
	b.waitFor("choices: ")
	b.waitFor("llm-cerebras gpt-oss-120b")

	// List: current row (the llm.plugin=llm-echo override) + catalog.
	b.send("/model list")
	b.waitFor("model: llm-echo")
	b.waitFor("llm-anthropic")
	b.waitFor("usage: /model")

	// Unknown provider errors loud, listing what exists.
	b.send("/model llm-bogus")
	b.waitFor(`unknown provider "llm-bogus"`)

	// Shorthand: keep the plugin, change the model — a real Reconcile
	// (the llm row remounts, loop and ui cascade behind it).
	b.send("/model claude-fancy")
	b.waitFor("model: llm-echo · claude-fancy")

	// The loop still runs after the swap.
	b.send("hello after swap")
	b.waitFor("echo: hello after swap")

	// A config hot reload must keep the runtime override.
	cfg, err := os.ReadFile(b.config)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(b.config, append(cfg, []byte("\n# poke reload\n")...), 0o644); err != nil {
		t.Fatal(err)
	}
	b.waitFor("bough: reloaded")
	b.send("/model list")
	b.closeStdin()
	if code := b.waitExit(); code != 0 {
		t.Fatalf("exit %d; output:\n%s", code, b.out.String())
	}
	inOrder(t, b.out.String(), "bough: reloaded", "model: llm-echo · claude-fancy")
	// None of the /model lines became LLM turns.
	mustNotContain(t, b.out.String(), "echo: /model")
}

func TestHeadlessBangRunsShell(t *testing.T) {
	t.Parallel()
	out := runHeadless(t, launchOpts{}, "!echo hi-bang", "!false", "!true")
	mustContain(t, out,
		"[system] hi-bang",
		"exit status 1",
		"[system] (no output)",
	)
	// Never an LLM turn.
	mustNotContain(t, out, "[assistant]", "echo: !echo")
}

func TestHeadlessBangNeverReachesLLM(t *testing.T) {
	t.Parallel()
	b := launchHeadless(t, launchOpts{})
	b.send("!echo bang-history")
	b.waitFor("[system] bang-history")
	b.send("real llm turn")
	b.waitFor("echo: real llm turn")
	b.closeStdin()
	if code := b.waitExit(); code != 0 {
		t.Fatalf("exit %d; output:\n%s", code, b.out.String())
	}

	files, err := filepath.Glob(filepath.Join(b.home, ".bough", "history", "*.jsonl"))
	if err != nil || len(files) != 1 {
		t.Fatalf("want 1 session file, got %v (err %v)", files, err)
	}
	f, err := os.Open(files[0])
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()

	var sawCommand, sawSystem bool
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		var e struct {
			Kind string         `json:"kind"`
			Data map[string]any `json:"data"`
		}
		if err := json.Unmarshal(sc.Bytes(), &e); err != nil {
			t.Fatalf("bad JSONL line %q: %v", sc.Text(), err)
		}
		text, _ := e.Data["text"].(string)
		switch e.Kind {
		case "command":
			if text == "!echo bang-history" {
				sawCommand = true
			}
		case "system":
			if text == "bang-history" {
				sawSystem = true
			}
		case "input", "assistant":
			// The bang line must never appear as a model-visible entry.
			if text == "!echo bang-history" {
				t.Fatalf("bang line recorded as %q entry: %s", e.Kind, sc.Text())
			}
		}
	}
	if !sawCommand || !sawSystem {
		t.Fatalf("history missing bang command/system entries (command=%v system=%v)", sawCommand, sawSystem)
	}
}
