package rules_test

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/unreal/hookbridge"
	"github.com/andreylukin/bough/kernel"

	_ "github.com/andreylukin/bough/plugins/agenttools"
	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/hooks"
	_ "github.com/andreylukin/bough/plugins/rules"
	_ "github.com/andreylukin/bough/plugins/tools"
)

// engineAsk stands in for the ask row's "ask-answers": a prompt rule
// must reach the user through it on the engine as on the loop.
type engineAsk struct {
	mu     sync.Mutex
	answer string
	asked  []string
}

func (a *engineAsk) Ask(q string, _ ...string) (string, error) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.asked = append(a.asked, q)
	return a.answer, nil
}

var theAsk = &engineAsk{}

func init() {
	kernel.Register("rules-engine-test-ask", func() kernel.Plugin { return askRow{} })
}

type askRow struct{}

func (askRow) Name() string     { return "rules-engine-test-ask" }
func (askRow) Inject() []string { return nil }
func (askRow) Apply(ctx *kernel.Context, _ map[string]any) error {
	ctx.Provide("ask-answers", theAsk)
	return nil
}

func writeFile(t *testing.T, path, body string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

// engineTree mounts the rows an engine session has around the rules
// row, in a project with one scoped Claude rule and two Codex prefix
// rules. HOME and cwd are the test's own (the rules row reads ".").
func engineTree(t *testing.T) *kernel.Context {
	t.Helper()
	t.Setenv("HOME", t.TempDir())
	project := t.TempDir()
	t.Chdir(project)
	writeFile(t, filepath.Join(project, ".claude", "rules", "api.md"), "---\npaths: \"src/api/**/*.ts\"\n---\n# API\nValidate input.")
	writeFile(t, filepath.Join(project, ".codex", "rules", "team.rules"),
		`prefix_rule(pattern = ["echo", "forbidden"], decision = "forbidden", justification = "not here")
prefix_rule(pattern = ["echo", "ask-me"], decision = "prompt")`)
	ctx := kernel.NewContext()
	err := ctx.Mount([]kernel.Row{
		{ID: "codemode", Plugin: "codemode"},
		{ID: "agent-tools", Plugin: "agent-tools"},
		{ID: "hooks", Plugin: "hooks-js"},
		{ID: "tools", Plugin: "tools-basic"},
		{ID: "rules", Plugin: "rules"},
		{ID: "ask", Plugin: "rules-engine-test-ask"},
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(ctx.Unmount)
	return ctx
}

func bridgeOf(t *testing.T, ctx *kernel.Context) *hookbridge.Bridge {
	t.Helper()
	h, err := kernel.Get[hookbridge.Firer](ctx, "hooks")
	if err != nil {
		t.Fatal(err)
	}
	return hookbridge.New(func() (hookbridge.Firer, bool) { return h, true })
}

// A native write's detail is its path (§8.1), and post-result carries
// it as "code": the scoped rule shows up with the first result that
// touches a matching file, once per session, exactly as it does after a
// loop code block.
func TestEngineScopedRuleOnNativeWrite(t *testing.T) {
	ctx := engineTree(t)
	b := bridgeOf(t, ctx)
	call := agenttools.Call{ID: "w1", Args: json.RawMessage(`{"path":"src/api/users.ts","content":"x"}`)}
	// The hook's code is the call's path and text, read from its args.
	web := agenttools.Call{ID: "w0", Args: json.RawMessage(`{"path":"src/web/app.ts","content":"x"}`)}

	r := b.PostTool(context.Background(), "write", web, "src/web/app.ts", agenttools.Result{Text: "wrote src/web/app.ts"})
	if r.Text != "wrote src/web/app.ts" {
		t.Fatalf("unrelated path got a rule: %q", r.Text)
	}
	r = b.PostTool(context.Background(), "write", call, "src/api/users.ts", agenttools.Result{Text: "wrote src/api/users.ts (+1 -0)"})
	if !strings.HasPrefix(r.Text, "wrote src/api/users.ts (+1 -0)") ||
		!strings.Contains(r.Text, "[rule: .claude/rules/api.md — applies to src/api/**/*.ts]") || !strings.Contains(r.Text, "Validate input.") {
		t.Fatalf("scoped rule missing: %q", r.Text)
	}
	r = b.PostTool(context.Background(), "patch", call, "src/api/users.ts", agenttools.Result{Text: "patched"})
	if r.Text != "patched" {
		t.Fatalf("scoped rule shown twice: %q", r.Text)
	}
	var applied bool
	for _, rec := range b.Drain() {
		if rec["notice"] == "applied .claude/rules/api.md" {
			applied = true
		}
	}
	if !applied {
		t.Fatal("the fire record does not name the rule that applied")
	}
}

// The Codex prefix rules gate native bash inside the tools row's bash,
// the same function the loop's tools.bash runs: forbidden comes back to
// the model as the call's error, and prompt asks through ask-answers.
func TestEngineBashRulesGateNativeBash(t *testing.T) {
	ctx := engineTree(t)
	reg, err := kernel.Get[agenttools.Registry](ctx, "agent-tools")
	if err != nil {
		t.Fatal(err)
	}
	bash, ok := reg.Lookup("bash")
	if !ok {
		t.Skip("native bash is registered by the tools row (W3); this runs once it is merged")
	}
	run := func(cmd string) (agenttools.Result, error) {
		args, _ := json.Marshal(map[string]any{"command": cmd})
		return bash.Call(context.Background(), agenttools.Call{ID: "b", Args: args})
	}
	failure := func(r agenttools.Result, err error) string {
		if err != nil {
			return err.Error() + " " + r.Error
		}
		return r.Error
	}

	res, err := run("echo forbidden now")
	if f := failure(res, err); !strings.Contains(f, "command refused by rule echo forbidden") || !strings.Contains(f, "not here") {
		t.Fatalf("forbidden prefix ran: %+v %v", res, err)
	}

	theAsk.mu.Lock()
	theAsk.answer = "refuse"
	theAsk.mu.Unlock()
	res, err = run("echo ask-me please")
	if f := failure(res, err); !strings.Contains(f, "refused by you") {
		t.Fatalf("refused prompt ran: %+v %v", res, err)
	}
	theAsk.mu.Lock()
	theAsk.answer = "run"
	asked := len(theAsk.asked)
	theAsk.mu.Unlock()
	res, err = run("echo ask-me please")
	if f := failure(res, err); f != "" || !strings.Contains(res.Text, "ask-me please") {
		t.Fatalf("approved prompt did not run: %+v %v", res, err)
	}
	theAsk.mu.Lock()
	defer theAsk.mu.Unlock()
	if len(theAsk.asked) != asked+1 || !strings.Contains(theAsk.asked[asked], "echo ask-me please") {
		t.Fatalf("asked = %v", theAsk.asked)
	}
}
