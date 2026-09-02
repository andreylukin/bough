// tools.ask round-trip through the real binary in headless mode: the
// model asks, the next stdin line answers, and the answer comes back
// to the model as tool output.
package e2e

import "testing"

// askProvider asks once via tools.ask, then reflects the tool output.
const askProvider = `
bough.provider("asker", function (system, messages) {
  var last = messages[messages.length - 1].content;
  if (last.indexOf("[tool output]") >= 0) return "final answer: " + last;
  return "\u0060\u0060\u0060js\nconsole.log(tools.ask('fav color?', 'red', 'blue'))\n\u0060\u0060\u0060";
});
bough.setup({ provider: { default: "asker" } });
`

func TestHeadlessAskFreeform(t *testing.T) {
	t.Parallel()
	b := launchHeadless(t, launchOpts{
		cwd: map[string]string{".bough/init.js": askProvider},
	})
	b.send("go")
	b.waitFor("[ask] fav color?")
	b.waitFor("2. blue")
	b.send("chartreuse actually")
	b.waitFor("final answer:")
	b.closeStdin()
	if code := b.waitExit(); code != 0 {
		t.Fatalf("exit %d; output:\n%s", code, b.out.String())
	}
	out := b.out.String()
	mustContain(t, out, "chartreuse actually", "[done]")

	// Both halves are durable history entries.
	log, code := runCLI(t, b.home, b.cwd, "log", "--raw")
	if code != 0 {
		t.Fatalf("bough log --raw exit %d:\n%s", code, log)
	}
	mustContain(t, log, `"kind":"ask"`, `"kind":"ask/answer"`, "fav color?", "chartreuse actually")
}

// sysCheckProvider reports whether the ask options nudge (pass each
// option as a separate argument) reached the system prompt.
const sysCheckProvider = `
bough.provider("syschk", function (system, messages) {
  if (system.indexOf("separate argument") >= 0 && system.indexOf("tools.ask(") >= 0) return "NUDGE_PRESENT";
  return "NUDGE_MISSING";
});
bough.setup({ provider: { default: "syschk" } });
`

func TestAskOptionsNudgeInSystemPrompt(t *testing.T) {
	t.Parallel()
	b := launchHeadless(t, launchOpts{
		cwd: map[string]string{".bough/init.js": sysCheckProvider},
	})
	b.send("go")
	b.waitFor("NUDGE_PRESENT")
	b.closeStdin()
	if code := b.waitExit(); code != 0 {
		t.Fatalf("exit %d; output:\n%s", code, b.out.String())
	}
}

func TestHeadlessAskNumberPicksOption(t *testing.T) {
	t.Parallel()
	b := launchHeadless(t, launchOpts{
		cwd: map[string]string{".bough/init.js": askProvider},
	})
	b.send("go")
	b.waitFor("[ask] fav color?")
	b.send("2")
	b.waitFor("final answer:")
	b.closeStdin()
	if code := b.waitExit(); code != 0 {
		t.Fatalf("exit %d; output:\n%s", code, b.out.String())
	}
	mustContain(t, b.out.String(), "blue")
}
