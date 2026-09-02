package graphiti

import (
	"encoding/json"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/codemode"
)

func TestFromConfigDefaultsAndOverrides(t *testing.T) {
	d := FromConfig(nil)
	if d.Port != 8621 || d.LLM != "openai" || d.Model != "gpt-5-mini" || !strings.HasSuffix(d.Home, "/.bough/graphiti") {
		t.Fatalf("defaults: %+v", d)
	}
	s := FromConfig(map[string]any{"port": 9000, "llm": "openrouter", "home": "/x"})
	if s.Port != 9000 || s.Model != "openai/gpt-5-mini" || s.Embedder != "openai/text-embedding-3-small" || s.Home != "/x" {
		t.Fatalf("openrouter switch should rename both models: %+v", s)
	}
	s = FromConfig(map[string]any{"port": float64(9001), "llm": "openrouter", "model": "anthropic/claude-haiku-4.5"})
	if s.Port != 9001 || s.Model != "anthropic/claude-haiku-4.5" || s.Embedder != "openai/text-embedding-3-small" {
		t.Fatalf("yaml numbers and a model override: %+v", s)
	}
}

func TestMergeServerKeepsOthers(t *testing.T) {
	in := []byte(`{"servers":{"chrome":{"command":"npx","args":["x"]}},"activations":{"a":1}}`)
	out, err := MergeServer(in, "graphiti", "http://127.0.0.1:8621/mcp/")
	if err != nil {
		t.Fatal(err)
	}
	var m map[string]any
	if err := json.Unmarshal(out, &m); err != nil {
		t.Fatal(err)
	}
	servers := m["servers"].(map[string]any)
	if servers["chrome"] == nil || m["activations"] == nil {
		t.Fatalf("other keys lost: %s", out)
	}
	if servers["graphiti"].(map[string]any)["url"] != "http://127.0.0.1:8621/mcp/" {
		t.Fatalf("graphiti entry: %s", out)
	}
	// Empty file and removal.
	out, err = MergeServer(nil, "graphiti", "http://x/")
	if err != nil || !strings.Contains(string(out), `"graphiti"`) {
		t.Fatalf("from empty: %s %v", out, err)
	}
	out, _ = MergeServer(out, "graphiti", "")
	if strings.Contains(string(out), `"graphiti"`) {
		t.Fatalf("removal: %s", out)
	}
	if _, err := MergeServer([]byte("{nope"), "g", "u"); err == nil {
		t.Fatal("malformed mcp.json must be an error, not a silent overwrite")
	}
}

func TestRenderPlist(t *testing.T) {
	p, err := RenderPlist(Settings{Home: "/h", Port: 1234, LLM: "openrouter", Model: "m", Embedder: "e"})
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"/h/src/mcp_server/.venv/bin/python", "/h/serve.py", "<string>1234</string>", "<string>m</string>", "/h/serve.log", label} {
		if !strings.Contains(p, want) {
			t.Fatalf("plist lacks %q:\n%s", want, p)
		}
	}
	if strings.Contains(p, "__") {
		t.Fatalf("unfilled placeholder:\n%s", p)
	}
}

// vm returns a codemode VM whose tools.bash records the command and
// replies with out (or throws err), the way the real bash tool does.
func vm(t *testing.T, out string, err error, ran *[]string) *codemode.CodeMode {
	t.Helper()
	cm := codemode.New(2 * time.Second)
	cm.RegisterTool("bash", func(cmd string) (string, error) {
		*ran = append(*ran, cmd)
		return out, err
	})
	return cm
}

func TestStopHookRemembersTheTurnInBackground(t *testing.T) {
	body, err := RenderHook("stop.js", "/opt/bough")
	if err != nil {
		t.Fatal(err)
	}
	var ran []string
	cm := vm(t, "", nil, &ran)
	res, err := cm.RunHook(body, map[string]any{
		"input": "what's my db?\n\n[memory]\n- Andrey prefers terse replies.",
		"reply": "it's SQLite",
	})
	if err != nil || res != nil {
		t.Fatalf("stop hook: res=%v err=%v", res, err)
	}
	if len(ran) != 1 {
		t.Fatalf("want one bash call, got %v", ran)
	}
	cmd := ran[0]
	for _, want := range []string{"nohup '/opt/bough' mcp call graphiti/add_memory", `"source":"message"`, "User: what", "Assistant: it", ">/dev/null 2>&1 &"} {
		if !strings.Contains(cmd, want) {
			t.Fatalf("command lacks %q: %s", want, cmd)
		}
	}
	if strings.Contains(cmd, "[memory]") {
		t.Fatalf("the recalled block must not be re-remembered: %s", cmd)
	}

	// Slash commands, shell lines and empty turns are not memories.
	for _, in := range []string{"/help", "!ls", ""} {
		ran = nil
		if _, err := cm.RunHook(body, map[string]any{"input": in, "reply": "x"}); err != nil {
			t.Fatal(err)
		}
		if len(ran) != 0 {
			t.Fatalf("%q should not be remembered: %v", in, ran)
		}
	}
}

func TestStopHookSurvivesBashFailure(t *testing.T) {
	body, _ := RenderHook("stop.js", "/opt/bough")
	var ran []string
	cm := vm(t, "", errors.New("boom"), &ran)
	if res, err := cm.RunHook(body, map[string]any{"input": "remember this please", "reply": "ok"}); err != nil || res != nil {
		t.Fatalf("a failing bash must not fail the hook: res=%v err=%v", res, err)
	}
}

func TestPromptHookAppendsFacts(t *testing.T) {
	body, err := RenderHook("prompt.js", "/opt/bough")
	if err != nil {
		t.Fatal(err)
	}
	facts := `{"message":"ok","facts":[{"fact":"Andrey prefers terse replies."},{"fact":"bough stores sessions in SQLite."},{"uuid":"no-fact-field"}]}`
	var ran []string
	cm := vm(t, facts, nil, &ran)
	res, err := cm.RunHook(body, map[string]any{"input": "what database does bough use?"})
	if err != nil {
		t.Fatal(err)
	}
	got, _ := res["input"].(string)
	want := "what database does bough use?\n\n[memory]\n- Andrey prefers terse replies.\n- bough stores sessions in SQLite."
	if got != want {
		t.Fatalf("input rewrite:\n got %q\nwant %q", got, want)
	}
	if len(ran) != 1 || !strings.Contains(ran[0], "'/opt/bough' mcp call graphiti/search_memory_facts") || !strings.Contains(ran[0], `"max_facts":5`) {
		t.Fatalf("search command: %v", ran)
	}
}

func TestPromptHookIsSilentWhenNothingOrBroken(t *testing.T) {
	body, _ := RenderHook("prompt.js", "/opt/bough")
	cases := []struct {
		name string
		out  string
		err  error
		in   string
	}{
		{"no facts", `{"message":"No relevant facts found","facts":[]}`, nil, "what database does bough use?"},
		{"server down", "", errors.New("connection refused"), "what database does bough use?"},
		{"garbage", "not json", nil, "what database does bough use?"},
		{"slash command", `{"facts":[{"fact":"x"}]}`, nil, "/help me now please"},
		{"short", `{"facts":[{"fact":"x"}]}`, nil, "hi"},
	}
	for _, c := range cases {
		var ran []string
		cm := vm(t, c.out, c.err, &ran)
		res, err := cm.RunHook(body, map[string]any{"input": c.in})
		if err != nil || res != nil {
			t.Fatalf("%s: want silence, got res=%v err=%v", c.name, res, err)
		}
	}
}

func TestPromptSectionNamesTheServer(t *testing.T) {
	s := PromptSection(FromConfig(nil))
	if !strings.HasPrefix(s, "## memory") || !strings.Contains(s, "http://127.0.0.1:8621/mcp/") || !strings.Contains(s, "search_memory_facts") {
		t.Fatalf("section: %s", s)
	}
}

func TestEnsureRowAnchorsAfterMcp(t *testing.T) {
	tree := "- id: hooks\n  plugin: hooks-js\n\n- id: mcp\n  plugin: mcp\n  # config:\n  #   servers: {}\n\n- id: todo\n  plugin: todo\n"
	out, changed := EnsureRow([]byte(tree))
	if !changed {
		t.Fatal("want the row added")
	}
	got := string(out)
	mcp, gr, todo := strings.Index(got, "plugin: mcp"), strings.Index(got, "plugin: graphiti"), strings.Index(got, "plugin: todo")
	if !(mcp < gr && gr < todo) {
		t.Fatalf("row order: %s", got)
	}
	if strings.Contains(got, "\n\n\n") {
		t.Fatalf("no double blank lines:\n%s", got)
	}
	if !strings.Contains(got, "  #   servers: {}\n\n# Long-term memory") {
		t.Fatalf("must insert after the mcp row's comment block:\n%s", got)
	}
	if _, again := EnsureRow(out); again {
		t.Fatal("second call must be a no-op")
	}
	if _, ok := EnsureRow([]byte("- id: llm\n  plugin: llm-echo\n")); ok {
		t.Fatal("no mcp row: nothing to anchor on, leave it alone")
	}
	// mcp row at the very end of the file.
	out, _ = EnsureRow([]byte("- id: mcp\n  plugin: mcp\n"))
	if !strings.HasSuffix(string(out), "plugin: graphiti\n") {
		t.Fatalf("append at end: %q", out)
	}
}

func TestHasKeyReadsTheEnvFileShapes(t *testing.T) {
	env := []byte("# keys\nexport OPENROUTER_API_KEY=\"sk-or-1\"\nOPENAI_API_KEY=\nEXA_API_KEY='x'\n")
	if !HasKey(env, "OPENROUTER_API_KEY") || !HasKey(env, "EXA_API_KEY") {
		t.Fatal("quoted and exported keys count")
	}
	if HasKey(env, "OPENAI_API_KEY") || HasKey(env, "NOPE") || HasKey(nil, "OPENROUTER_API_KEY") {
		t.Fatal("empty, absent, or no file is not a key")
	}
	if FromConfig(nil).KeyName() != "OPENAI_API_KEY" || FromConfig(map[string]any{"llm": "openrouter"}).KeyName() != "OPENROUTER_API_KEY" {
		t.Fatal("key name follows the llm")
	}
}
