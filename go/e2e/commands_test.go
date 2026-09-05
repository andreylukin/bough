// Slash-command suite: a "/" line dispatches through the commands
// registry, prints "[system] <output>" in headless mode, is recorded in
// history as "command" + "system" entries, and never reaches the LLM.
package e2e

import (
	"bufio"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func TestHeadlessSlashHelp(t *testing.T) {
	t.Parallel()
	out := runHeadless(t, launchOpts{}, "/help")
	// The command table, [system]-prefixed, with the built-ins aligned.
	mustContain(t, out,
		"[system]",
		"/help",
		"/sessions",
		"pick a session to resume",
		"/quit",
	)
	// The line never became an LLM turn.
	mustNotContain(t, out, "[assistant]", "echo: /help")
}

func TestHeadlessSlashUnknown(t *testing.T) {
	t.Parallel()
	out := runHeadless(t, launchOpts{}, "/frobnicate now")
	mustContain(t, out, "[system] unknown command: /frobnicate (try /help)")
	mustNotContain(t, out, "[assistant]")
}

func TestHeadlessSlashExport(t *testing.T) {
	t.Parallel()
	b := launchHeadless(t, launchOpts{})
	b.send("hello there")
	b.waitFor("echo: hello there")
	b.send("/export")
	b.waitFor("exported to ")
	b.closeStdin()
	if code := b.waitExit(); code != 0 {
		t.Fatalf("exit %d; output:\n%s", code, b.out.String())
	}

	files, err := filepath.Glob(filepath.Join(b.home, ".bough", "exports", "*.md"))
	if err != nil || len(files) != 1 {
		t.Fatalf("want 1 exported file, got %v (err %v)", files, err)
	}
	md, err := os.ReadFile(files[0])
	if err != nil {
		t.Fatal(err)
	}
	mustContain(t, string(md), "hello there", "echo: hello there")
	mustContain(t, b.out.String(), "[system] exported to "+files[0])
}

func TestHeadlessJSCommandDispatches(t *testing.T) {
	t.Parallel()
	out := runHeadless(t, launchOpts{
		cwd: map[string]string{".bough/init.js": `
bough.command("shout", "<text>", "make it loud", function (args) {
  return args.toUpperCase() + "!"
});
bough.command("silent", "", "returns nothing", function () { return "" });
`},
	}, "/shout hey bough", "/silent")
	mustContain(t, out, "[system] HEY BOUGH!")
	// M27: empty output echoes the command name as a notice.
	mustContain(t, out, "[system] /silent")
	mustNotContain(t, out, "[assistant]")
}

func TestHeadlessSlashLineNeverReachesLLM(t *testing.T) {
	t.Parallel()
	b := launchHeadless(t, launchOpts{})
	b.send("/help")
	b.send("real words for the llm")
	b.waitFor("echo: real words for the llm")
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
			sawCommand = true
		case "system":
			sawSystem = true
		case "input", "assistant":
			// The slash line must never appear as a model-visible entry.
			if text == "/help" {
				t.Fatalf("slash line recorded as %q entry: %s", e.Kind, sc.Text())
			}
		}
	}
	if !sawCommand || !sawSystem {
		t.Fatalf("history missing command/system entries (command=%v system=%v)", sawCommand, sawSystem)
	}
	// The real input still made a normal LLM turn.
	mustContain(t, b.out.String(), "echo: real words for the llm")
	// And the model never saw or answered the slash line.
	mustNotContain(t, b.out.String(), "echo: /help")
}
