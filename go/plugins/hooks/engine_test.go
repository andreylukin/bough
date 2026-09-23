package hooks

import (
	"context"
	"encoding/json"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/unreal/hookbridge"
)

// The hooks service is what the engine's bridge fires through.
var _ hookbridge.Firer = (*Service)(nil)

func engineBridge(s *Service) (*hookbridge.Bridge, *[]string) {
	var notes []string
	b := hookbridge.New(func() (hookbridge.Firer, bool) { return s, true })
	b.Session = "engine-sess"
	b.Notify = func(kind, text string) { notes = append(notes, kind+": "+text) }
	return b, &notes
}

// A hook file written for the loop matches a native call by its command
// text in event.code, and one written for the engine reads event.tool
// and event.args: a deny refuses the call and an args object replaces
// its arguments, through the real goja VM.
func TestEnginePreToolThroughJSHooks(t *testing.T) {
	s := fixture(t)
	writeHook(t, ".", "pre-code-exec", "a-deny.js", `
		if (event.code.indexOf("git push") === 0) return {deny: "no pushing from " + event.tool};
		return null;`)
	writeHook(t, ".", "pre-code-exec", "b-args.js", `
		if (event.tool !== "bash") return null;
		return {args: {command: event.args.command + " --dry-run", timeout: event.args.timeout}};`)
	b, _ := engineBridge(s)
	ctx := context.Background()

	_, deny := b.PreTool(ctx, "bash", agenttools.Call{ID: "c1", Args: json.RawMessage(`{"command":"git push"}`)}, "git push")
	if deny != "no pushing from bash" {
		t.Fatalf("deny = %q", deny)
	}
	args, deny := b.PreTool(ctx, "bash", agenttools.Call{ID: "c2", Args: json.RawMessage(`{"command":"make","timeout":30}`)}, "make")
	if deny != "" {
		t.Fatalf("unexpected deny %q", deny)
	}
	var got map[string]any
	if err := json.Unmarshal(args, &got); err != nil || got["command"] != "make --dry-run" || got["timeout"] != float64(30) {
		t.Fatalf("args = %s (%v)", args, err)
	}
	if args, deny := b.PreTool(ctx, "view", agenttools.Call{ID: "c3", Args: json.RawMessage(`{"path":"a.go"}`)}, "a.go"); args != nil || deny != "" {
		t.Fatalf("view was touched: %s %q", args, deny)
	}

	recs := b.Drain()
	// The deny short-circuits the second file for the first call.
	if len(recs) != 5 {
		t.Fatalf("want 5 fire records, got %d", len(recs))
	}

	// The row's detail is the first line cut at 80 bytes; a deny hook
	// must see the whole command, as it sees the whole block on the loop.
	writeHook(t, ".", "pre-code-exec", "c-anywhere.js", `
		if (event.code.indexOf("git push") >= 0) return {deny: "push found"};
		return null;`)
	long := strings.Repeat("x", 90) + "; git push"
	for _, tc := range []struct{ tool, args, detail string }{
		{"bash", `{"command":"true\ngit push"}`, "true …"},
		{"bash", `{"command":"` + long + `"}`, long[:80]},
		{"run_js", `{"code":"const a = 1;\nawait tools.bash(\"git push\")"}`, "const a = 1;"},
	} {
		if _, deny := b.PreTool(ctx, tc.tool, agenttools.Call{ID: "m", Args: json.RawMessage(tc.args)}, tc.detail); deny != "push found" {
			t.Errorf("%s %s: deny = %q, want the hook to see the whole text", tc.tool, tc.args, deny)
		}
	}
	if recs[0]["decision"] != "denied" || recs[0]["event"] != "pre-code-exec" {
		t.Fatalf("first record %v", recs[0])
	}
	if f := s.Fires(1); f[0].Session != "engine-sess" {
		t.Fatalf("fire recorded under %q", f[0].Session)
	}
}

// post-result sees the text the model would read and may rewrite it;
// a throwing hook file is reported and the other file's rewrite stands.
func TestEnginePostToolThroughJSHooks(t *testing.T) {
	s := fixture(t)
	writeHook(t, ".", "post-result", "a-throw.js", `throw new Error("boom")`)
	writeHook(t, ".", "post-result", "b-upper.js", `
		return {result: event.tool + "/" + event.call + ": " + event.result.toUpperCase() + (event.error ? " (" + event.error + ")" : "")};`)
	b, notes := engineBridge(s)
	r := b.PostTool(context.Background(), "bash", agenttools.Call{ID: "c9"}, "ls", agenttools.Result{Text: "a b", Error: "exit 2"})
	if r.Text != "bash/c9: A B (exit 2)" || r.Error != "exit 2" {
		t.Fatalf("result = %+v", r)
	}
	if len(*notes) != 1 || !strings.HasPrefix((*notes)[0], "error: hook post-result: ") || !strings.Contains((*notes)[0], "boom") {
		t.Fatalf("notes = %v", *notes)
	}
}

// user-prompt-submit, session-start and stop keep the loop's keys.
func TestEngineLifecycleThroughJSHooks(t *testing.T) {
	s := fixture(t)
	writeHook(t, ".", "session-start", "ctx.js", `return {context: "from the hook"}`)
	writeHook(t, ".", "user-prompt-submit", "r.js", `
		if (event.input === "block me") return {block: "blocked"};
		return {input: event.input + "!"};`)
	writeHook(t, ".", "stop", "s.js", `return event.reply.indexOf("tests pass") < 0 ? {block: "run the tests"} : null`)
	b, _ := engineBridge(s)
	ctx := context.Background()
	if got := b.SessionStart(ctx); got != "from the hook" {
		t.Fatalf("SessionStart = %q", got)
	}
	if out, block, _ := b.PromptSubmit(ctx, "hi"); out != "hi!" || block != "" {
		t.Fatalf("rewrite: %q %q", out, block)
	}
	if _, block, _ := b.PromptSubmit(ctx, "block me"); block != "blocked" {
		t.Fatalf("block: %q", block)
	}
	if got := b.Stop(ctx, "done"); got != "run the tests" {
		t.Fatalf("Stop = %q", got)
	}
	if got := b.Stop(ctx, "done, tests pass"); got != "" {
		t.Fatalf("Stop = %q", got)
	}
}
