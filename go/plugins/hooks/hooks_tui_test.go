// TUI-integration tests: real kernel + hooks-js + codemode + loop +
// the real ui model, driven in-process.
//
// TestMain sandboxes HOME to a per-run temp dir (hook files live under
// $HOME/.bough/hooks). Parallel tests each own a distinct event dir,
// so they never write the same path; hook files land via atomic rename
// because hooks-js re-reads them on every fire.
package hooks_test

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"

	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/loop"
	_ "github.com/andreylukin/bough/plugins/tools"
)

func TestMain(m *testing.M) {
	home, err := os.MkdirTemp("", "bough-hooks-tui-home-*")
	if err != nil {
		panic(err)
	}
	os.Setenv("HOME", home)
	code := m.Run()
	os.RemoveAll(home)
	os.Exit(code)
}

// writeHook installs $HOME/.bough/hooks/<event>/<name> atomically.
func writeHook(t *testing.T, event, name, body string) {
	t.Helper()
	home, err := os.UserHomeDir()
	if err != nil {
		t.Fatal(err)
	}
	dir := filepath.Join(home, ".bough", "hooks", event)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	tmp := filepath.Join(dir, name+".tmp")
	if err := os.WriteFile(tmp, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(tmp, filepath.Join(dir, name)); err != nil {
		t.Fatal(err)
	}
}

// A pre-code-exec deny renders as a "[hook denied: ...]" result block
// and the denied code never runs.
func TestPreCodeExecDenyRenders(t *testing.T) {
	t.Parallel()
	writeHook(t, "pre-code-exec", "deny.js", `return {deny: "unit-denied"}`)
	d := uitest.Mount(t, nil, "codemode", "tools-basic", "llm-echo", "hooks-js", "loop")
	d.Say("CODE!")
	d.WaitFor("[hook denied: unit-denied]")
	// The assistant's quoted js legitimately mentions the command; what
	// must not appear is an executed-code box or its boxed output line.
	frame := d.Frame()
	if strings.Contains(frame, "Ran: echo hi from codemode (") || strings.Contains(frame, "│ hi from codemode") {
		t.Fatalf("denied code ran anyway:\n%s", frame)
	}
}

// A user-prompt-submit rewrite is visible in the echoed reply: the
// model saw the rewritten input, and the renderer shows it.
func TestPromptRewriteVisibleInReply(t *testing.T) {
	t.Parallel()
	writeHook(t, "user-prompt-submit", "rw.js", `return {input: event.input + " HOOK_REWROTE_ME"}`)
	d := uitest.Mount(t, nil, "codemode", "llm-echo", "hooks-js", "loop")
	d.Say("original words")
	d.WaitFor("original words HOOK_REWROTE_ME")
}

// A session-start hook's context lands in the system prompt; a stub
// provider that inspects its system prompt proves it, end to end.
func TestSessionStartContextReachesSystemPrompt(t *testing.T) {
	t.Parallel()
	writeHook(t, "session-start", "ctx.js", `return {context: "HOOKCTX_MARK_9314"}`)
	detector := uitest.LLMFunc(func(system string, _ []llm.Message) string {
		if strings.Contains(system, "HOOKCTX_MARK_9314") {
			return "sysmark: present"
		}
		return "sysmark: absent"
	})
	d := uitest.Mount(t, func(c *kernel.Context) { c.Provide("llm", detector) },
		"codemode", "hooks-js", "loop")
	d.Say("anything")
	d.WaitFor("sysmark: present")
}
