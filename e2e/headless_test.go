// Headless smoke suite: the real binary, --headless, stdin lines in,
// "[kind] text" event lines out. llm-echo (or a JS parrot) keeps every
// case deterministic.
package e2e

import (
	"bufio"
	"encoding/json"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"
)

func TestHeadlessEcho(t *testing.T) {
	t.Parallel()
	out := runHeadless(t, launchOpts{}, "hello bough")
	inOrder(t, out, "[assistant] echo: hello bough", "[done]")
}

func TestHeadlessCodemode(t *testing.T) {
	t.Parallel()
	out := runHeadless(t, launchOpts{}, "CODE!")
	// assistant emits the js block, the block runs, its output feeds back.
	inOrder(t, out,
		"[assistant] ```js",
		"[code] tools.bash",
		"hi from codemode",
		"[assistant] echo: [tool output]",
		"[done]",
	)
}

func TestHeadlessHookRewritesPrompt(t *testing.T) {
	t.Parallel()
	out := runHeadless(t, launchOpts{
		cwd: map[string]string{
			".bough/hooks/user-prompt-submit/rewrite.js": `return {input: event.input + " REWRITTEN_BY_HOOK"}`,
		},
	}, "original words")
	mustContain(t, out, "echo: original words REWRITTEN_BY_HOOK")
}

func TestHeadlessHookBlocksPrompt(t *testing.T) {
	t.Parallel()
	out := runHeadless(t, launchOpts{
		cwd: map[string]string{
			".bough/hooks/user-prompt-submit/block.js": `return {block: "blocked-by-policy-xyz"}`,
		},
	}, "try me")
	inOrder(t, out, "[error] blocked-by-policy-xyz", "[done]")
	mustNotContain(t, out, "echo: try me") // the llm never saw it
}

func TestHeadlessHookDeniesCode(t *testing.T) {
	t.Parallel()
	out := runHeadless(t, launchOpts{
		cwd: map[string]string{
			".bough/hooks/pre-code-exec/deny.js": `return {deny: "nope-project"}`,
		},
	}, "CODE!")
	mustContain(t, out, "[result] [hook denied: nope-project]")
	// The denied block never ran: its output line never appears as a
	// standalone result, only as source text inside the assistant reply.
	mustNotContain(t, out, "[result] hi from codemode")
}

func TestHeadlessGlobalHookFires(t *testing.T) {
	t.Parallel()
	out := runHeadless(t, launchOpts{
		home: map[string]string{
			".bough/hooks/pre-code-exec/deny.js": `return {deny: "nope-global"}`,
		},
	}, "CODE!")
	mustContain(t, out, "[hook denied: nope-global]")
}

func TestHeadlessSkillInjection(t *testing.T) {
	t.Parallel()
	out := runHeadless(t, launchOpts{
		home: map[string]string{
			".claude/skills/frobnicator/SKILL.md": "SKILL_MARKER_777 spin widdershins",
			".claude/skills/unrelated/SKILL.md":   "UNMENTIONED_MARKER_888",
		},
	}, "please use frobnicator now")
	// llm-echo replies with the last user message, skills blocks included.
	mustContain(t, out, "[skill: frobnicator]", "SKILL_MARKER_777")
	mustNotContain(t, out, "UNMENTIONED_MARKER_888")
}

// sysHeadProvider reflects the head of the system prompt back as the
// reply (backticks stripped so codemode never executes the reflection).
const sysHeadProvider = `
bough.provider("syshead", function (system, messages) {
  return "SYSHEAD::" + system.slice(0, 500).replace(/\u0060/g, "'");
});
bough.setup({ provider: { default: "syshead" } });
`

func TestHeadlessContextMD(t *testing.T) {
	t.Parallel()
	out := runHeadless(t, launchOpts{
		cwd: map[string]string{
			"AGENTS.md":      "AGENTS_MD_MARKER_31337 always be kind",
			".bough/init.js": sysHeadProvider,
		},
	}, "anything")
	mustContain(t, out, "AGENTS_MD_MARKER_31337", "# Context:")
}

func TestHeadlessInitJSBadKeyFailsBoot(t *testing.T) {
	t.Parallel()
	b := launchHeadless(t, launchOpts{
		cwd: map[string]string{".bough/init.js": `bough.setup({ bogus: 1 });`},
	})
	b.closeStdin()
	if code := b.waitExit(); code == 0 {
		t.Fatalf("expected nonzero exit; output:\n%s", b.out.String())
	}
	mustContain(t, b.out.String(), `unknown key "bogus"`)
}

func TestHeadlessJSTool(t *testing.T) {
	t.Parallel()
	out := runHeadless(t, launchOpts{
		cwd: map[string]string{".bough/init.js": `
bough.tool("greet", function () { return "TOOL_SAYS_HI_5150"; });
bough.provider("toolp", function (system, messages) {
  var last = messages[messages.length - 1].content;
  if (last.indexOf("[tool output]") >= 0) return "tool done";
  return "\u0060\u0060\u0060js\nconsole.log(tools.greet())\n\u0060\u0060\u0060";
});
bough.setup({ provider: { default: "toolp" } });
`},
	}, "go")
	inOrder(t, out, "TOOL_SAYS_HI_5150", "[assistant] tool done", "[done]")
}

func TestHeadlessParrotProvider(t *testing.T) {
	t.Parallel()
	out := runHeadless(t, launchOpts{
		cwd: map[string]string{".bough/init.js": `
bough.provider("parrot", function (system, messages) {
  var last = messages[messages.length - 1].content;
  return "parrot(" + last + ") after " + messages.length + " msgs";
});
bough.setup({ provider: { default: "parrot" } });
`},
	}, "polly", "wants a cracker")
	inOrder(t, out,
		"parrot(polly) after 1 msgs",
		"parrot(wants a cracker) after 3 msgs",
	)
}

func TestHeadlessSystemAppendCognition(t *testing.T) {
	t.Parallel()
	out := runHeadless(t, launchOpts{
		cwd: map[string]string{".bough/init.js": `
bough.provider("systail", function (system, messages) {
  return "SYSTAIL::" + system.slice(-80).replace(/\u0060/g, "'");
});
bough.setup({
  provider: { default: "systail" },
  system: { append: "APPENDED_COGNITION_MARK_4242" },
});
`},
	}, "anything")
	mustContain(t, out, "APPENDED_COGNITION_MARK_4242")
}

func TestHeadlessProjectionOverride(t *testing.T) {
	t.Parallel()
	out := runHeadless(t, launchOpts{
		cwd: map[string]string{".bough/init.js": `
bough.project(function (entries) {
  return [{ role: "user", content: "PROJECTED_INPUT_2718" }];
});
`},
	}, "the real input")
	mustContain(t, out, "echo: PROJECTED_INPUT_2718")
	mustNotContain(t, out, "echo: the real input")
}

func TestHeadlessHistoryJSONLAndLog(t *testing.T) {
	t.Parallel()
	b := launchHeadless(t, launchOpts{})
	b.send("remember me")
	b.closeStdin()
	if code := b.waitExit(); code != 0 {
		t.Fatalf("exit %d; output:\n%s", code, b.out.String())
	}

	// One session file, every line valid JSON with seq/kind/at.
	files, err := filepath.Glob(filepath.Join(b.home, ".bough", "history", "*.jsonl"))
	if err != nil || len(files) != 1 {
		t.Fatalf("want 1 session file, got %v (err %v)", files, err)
	}
	f, err := os.Open(files[0])
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	kinds := map[string]bool{}
	seq := int64(0)
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		var e struct {
			Seq  int64          `json:"seq"`
			At   time.Time      `json:"at"`
			Kind string         `json:"kind"`
			Data map[string]any `json:"data"`
		}
		if err := json.Unmarshal(sc.Bytes(), &e); err != nil {
			t.Fatalf("bad JSONL line %q: %v", sc.Text(), err)
		}
		if e.Seq != seq+1 {
			t.Fatalf("seq not monotonic: got %d after %d", e.Seq, seq)
		}
		seq = e.Seq
		if e.At.IsZero() {
			t.Fatalf("entry %d has zero timestamp", e.Seq)
		}
		kinds[e.Kind] = true
	}
	for _, k := range []string{"input", "assistant", "done"} {
		if !kinds[k] {
			t.Fatalf("history missing kind %q (have %v)", k, kinds)
		}
	}

	// 'bough log' (no arg = latest session under this HOME) pretty-prints it.
	out, code := runCLI(t, b.home, b.cwd, "log")
	if code != 0 {
		t.Fatalf("bough log exit %d:\n%s", code, out)
	}
	mustContain(t, out, "input", "assistant", "echo: remember me")

	// --raw prints the JSON lines verbatim.
	raw, code := runCLI(t, b.home, b.cwd, "log", "--raw")
	if code != 0 {
		t.Fatalf("bough log --raw exit %d:\n%s", code, raw)
	}
	mustContain(t, raw, `"kind":"input"`, `"kind":"assistant"`)
}

func TestRowsCommand(t *testing.T) {
	t.Parallel()
	home, cwd, _ := sandbox(t, launchOpts{})
	out, code := runCLI(t, home, cwd, "rows", "--config", "bough.yml", "--set", "llm.plugin=llm-echo")
	if code != 0 {
		t.Fatalf("bough rows exit %d:\n%s", code, out)
	}
	mustContain(t, out, "ID", "PLUGIN", "STATE")
	for _, re := range []string{
		`llm\s+llm-echo\s+active`,
		`loop\s+loop\s+active`,
		`ui\s+ui\s+active`,
		`history\s+history\s+active`,
	} {
		if !regexp.MustCompile(re).MatchString(out) {
			t.Fatalf("rows table missing /%s/:\n%s", re, out)
		}
	}
}

func TestBadSetUnknownRowFails(t *testing.T) {
	t.Parallel()
	home, cwd, _ := sandbox(t, launchOpts{})
	out, code := runCLI(t, home, cwd, "--config", "bough.yml", "--set", "nosuch.key=v", "--headless")
	if code == 0 {
		t.Fatalf("expected nonzero exit:\n%s", out)
	}
	mustContain(t, out, `no row with id "nosuch"`)
}

func TestHeadlessConfigHotReload(t *testing.T) {
	t.Parallel()
	b := launchHeadless(t, launchOpts{})
	b.send("before reload")
	b.waitFor("echo: before reload")

	// A real row change: give the skills row a config key. The watcher
	// debounces 300ms, reconciles, and logs the reload; the --set
	// llm-echo override is re-applied so the session keeps answering.
	yml, err := os.ReadFile(b.config)
	if err != nil {
		t.Fatal(err)
	}
	edited := strings.Replace(string(yml),
		"- id: skills\n  plugin: skills",
		"- id: skills\n  plugin: skills\n  config:\n    marker: reloaded", 1)
	if edited == string(yml) {
		t.Fatal("config edit did not apply (skills row not found)")
	}
	if err := os.WriteFile(b.config, []byte(edited), 0o644); err != nil {
		t.Fatal(err)
	}
	b.waitFor("bough: reloaded")

	b.send("after reload")
	b.waitFor("echo: after reload")
	b.closeStdin()
	if code := b.waitExit(); code != 0 {
		t.Fatalf("exit %d; output:\n%s", code, b.out.String())
	}
}

func TestHeadlessBrokenReloadKeepsLastGoodTree(t *testing.T) {
	t.Parallel()
	b := launchHeadless(t, launchOpts{})
	b.send("still good")
	b.waitFor("echo: still good")

	if err := os.WriteFile(b.config, []byte("this is: [not valid yaml\n  nope"), 0o644); err != nil {
		t.Fatal(err)
	}
	b.waitFor("keeping current tree")

	b.send("after breakage")
	b.waitFor("echo: after breakage")
	b.closeStdin()
	if code := b.waitExit(); code != 0 {
		t.Fatalf("exit %d; output:\n%s", code, b.out.String())
	}
}
